//! Plans: the user's selection, frozen, with what each item was judged under
//! (`docs/04-safety-model.md` §3.5, §3.6; `docs/03-functional-spec.md` §2 J3, J6).
//!
//! A [`Plan`] is built only by the core's planner, and frozen before review is shown, so
//! review shows exactly what apply may do and a child created after review is in no
//! frozen tree. Every item was selected by its ID ([`Selection`]); nothing joins a plan
//! by a rule, group or bucket at apply. A plan read from a file is untrusted: it is
//! planned again from a fresh scan and each tree intersected with the fresh one
//! ([`FrozenTree::recheck`]), so it can only lose entries.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::digest::Digest;
use crate::engine::FindingId;
use crate::frozen::{FrozenTree, RelPath, Scope};
use crate::mutation::LocalFileSystem;
use crate::policy::{Caution, Facts, Level, Policy, Proposal, Verdict};
use crate::probe::FileIdentity;
use crate::root::SymbolicRoot;
use crate::rule::{Method, Rule, RuleId};
use crate::size::SpaceEstimate;

/// An item's place in its plan. A reference, never authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemId(pub u32);

/// A root as the scan opened it, with the identity recorded then (`OpenedRoot`). Apply
/// compares the root with it before anything else.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VerifiedRoot {
    /// Which root.
    pub root: SymbolicRoot,
    /// The directory, as resolved (D-041).
    pub path: PathBuf,
    /// Its identity when the scan opened it.
    pub identity: FileIdentity,
}

/// Where a target is: a verified root and the components below it. Mutation walks the
/// components from the root's handle; the joined path is only for showing people.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Anchor {
    /// The root it is below.
    pub root: VerifiedRoot,
    /// The components from the root to the target. Empty when the target is the root,
    /// whose contents are its tree and which is itself never removed.
    pub relative: RelPath,
    /// The identity the directory holding the target must have, checked when set. A plan
    /// item's anchor has it for every target below its root.
    pub parent: Option<FileIdentity>,
}

impl Anchor {
    /// What its tree's top is.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        if self.relative.is_empty() {
            Scope::Contents
        } else {
            Scope::Whole
        }
    }

    /// The target's path, for showing people.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.relative.below(&self.root.path)
    }
}

/// The bucket an item was authorized in, with the cautions it was reviewed with.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Standing {
    /// Safe.
    Safe,
    /// Review, with these cautions. Revalidation drops it if it gains another.
    Review(BTreeSet<Caution>),
}

impl Standing {
    /// The standing `verdict` gives, unless it is Protected.
    #[must_use]
    pub fn of(verdict: &Verdict) -> Option<Self> {
        match verdict {
            Verdict::Safe => Some(Self::Safe),
            Verdict::Review(cautions) => Some(Self::Review(cautions.iter().copied().collect())),
            Verdict::Protected(_) => None,
        }
    }
}

named! {
    /// One thing an item was judged under.
    JudgementField {
        /// Its rule, by its content: selector, root, operation and methods.
        Rule = "rule",
        /// Its effective carefulness level.
        Level = "level",
        /// Its effective recent window.
        RecentWindow = "recent-window",
        /// Its method.
        Method = "method",
        /// The kept and excluded paths.
        Held = "held",
        /// Anything else in the policy.
        Policy = "policy",
    }
}

/// What an item was judged under, stored with it. At apply each is compared with the
/// current one, and any difference drops the item: a changed policy or rule is unknown
/// (04 §3.3).
///
/// The digests are of each value's `Debug` form ([`Digest::of_debug`]), so any change to
/// it, even one only of how kipple prints it, drops the item rather than letting it
/// through.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Judgement {
    /// The whole policy.
    pub policy: Digest,
    /// The rule's ID.
    pub rule: RuleId,
    /// The rule's content. A built-in rule changes with the kipple that carries it; a
    /// pack's rule with its pack.
    pub rule_digest: Digest,
    /// The effective level.
    pub level: Level,
    /// The effective recent window, in days.
    pub recent_days: u16,
    /// The method.
    pub method: Method,
    /// The kept and excluded paths.
    pub held: Digest,
}

impl Judgement {
    /// What `rule`'s finding with `facts` is judged under by `policy`, to be removed by
    /// `method`.
    #[must_use]
    pub fn new(policy: &Policy, rule: &Rule, facts: &Facts, method: Method) -> Self {
        Self {
            policy: Digest::of_debug(policy),
            rule: rule.id.clone(),
            rule_digest: Digest::of_debug(rule),
            level: policy.level_of(&rule.id),
            recent_days: policy.recent_window_days(&rule.id, facts),
            method,
            held: Digest::of_debug(&[&policy.keep, &policy.exclude]),
        }
    }

    /// The first way `now` differs from this, the most specific first.
    #[must_use]
    pub fn first_difference(&self, now: &Self) -> Option<JudgementField> {
        let differs = [
            (
                self.rule != now.rule || self.rule_digest != now.rule_digest,
                JudgementField::Rule,
            ),
            (self.level != now.level, JudgementField::Level),
            (
                self.recent_days != now.recent_days,
                JudgementField::RecentWindow,
            ),
            (self.method != now.method, JudgementField::Method),
            (self.held != now.held, JudgementField::Held),
            (self.policy != now.policy, JudgementField::Policy),
        ];
        differs
            .into_iter()
            .find_map(|(differs, field)| differs.then_some(field))
    }
}

/// The findings the user chose, each by its ID. There is no "everything" and no rule or
/// bucket here: a shortcut or `--only` picks IDs before planning, and membership is fixed
/// then.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection(pub BTreeSet<FindingId>);

/// The methods that may decide how an item is removed, in precedence order
/// (`docs/03-functional-spec.md` §6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MethodChoice {
    /// `--method`.
    pub command_line: Option<Method>,
    /// `rules.<id>.method`.
    pub rule: Option<Method>,
    /// `clean.method`.
    pub config: Option<Method>,
}

/// Why an item can't be planned with the method chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MethodRefusal {
    /// Its rule only reports.
    #[error("its rule only reports")]
    ReportOnly,
    /// Its rule doesn't offer the method chosen. Another is never picked instead.
    #[error("its rule doesn't offer {0}")]
    NotOffered(Method),
}

impl MethodChoice {
    /// The method: `--method`, else `rules.<id>.method`, else `clean.method`, else
    /// `delete`.
    #[must_use]
    pub fn resolve(&self) -> Method {
        self.command_line
            .or(self.rule)
            .or(self.config)
            .unwrap_or(Method::Delete)
    }

    /// The method for a finding whose rule proposes `proposal`.
    ///
    /// # Errors
    /// When the rule only reports or doesn't offer the method resolved. A trash item is
    /// never planned as a delete, nor the other way round.
    pub fn for_proposal(&self, proposal: &Proposal) -> Result<Method, MethodRefusal> {
        let method = self.resolve();
        match proposal {
            Proposal::Report | Proposal::Advice => Err(MethodRefusal::ReportOnly),
            Proposal::Remove(methods) if methods.contains(&method) => Ok(method),
            Proposal::Remove(_) => Err(MethodRefusal::NotOffered(method)),
        }
    }
}

/// One item of a plan. Built only by the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub(crate) item: ItemId,
    pub(crate) finding: FindingId,
    pub(crate) path: PathBuf,
    pub(crate) anchor: Anchor,
    pub(crate) method: Method,
    pub(crate) standing: Standing,
    pub(crate) judgement: Judgement,
    pub(crate) estimate: Option<SpaceEstimate>,
    pub(crate) file_system: LocalFileSystem,
    pub(crate) tree: Arc<FrozenTree>,
}

impl PlanEntry {
    /// Its place in the plan.
    #[must_use]
    pub const fn item(&self) -> ItemId {
        self.item
    }

    /// The finding it was planned from.
    #[must_use]
    pub const fn finding(&self) -> FindingId {
        self.finding
    }

    /// The finding's path, as the scan reported it.
    #[must_use]
    pub const fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Where its target is.
    #[must_use]
    pub const fn anchor(&self) -> &Anchor {
        &self.anchor
    }

    /// How it is to be removed.
    #[must_use]
    pub const fn method(&self) -> Method {
        self.method
    }

    /// The bucket and cautions it was planned with.
    #[must_use]
    pub const fn standing(&self) -> &Standing {
        &self.standing
    }

    /// What it was judged under.
    #[must_use]
    pub const fn judgement(&self) -> &Judgement {
        &self.judgement
    }

    /// What the scan estimated removing it frees.
    #[must_use]
    pub const fn estimate(&self) -> Option<&SpaceEstimate> {
        self.estimate.as_ref()
    }

    /// The file system it is on, which is supported: an item on another one isn't
    /// planned.
    #[must_use]
    pub const fn file_system(&self) -> LocalFileSystem {
        self.file_system
    }

    /// Its frozen tree, as reviewed.
    #[must_use]
    pub fn tree(&self) -> &FrozenTree {
        &self.tree
    }
}

/// A frozen selection, ready for review. Built only by the core's planner, before review
/// is shown.
///
/// ```compile_fail,E0451
/// // A frontend can't make a plan of its own, or change one it was given.
/// fn forge(plan: &kipple_core::Plan) -> kipple_core::Plan {
///     kipple_core::Plan { entries: plan.entries().to_vec(), digest: plan.digest() }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    entries: Vec<PlanEntry>,
    digest: Digest,
}

impl Plan {
    /// A plan of `entries`, numbered in order.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the planner (M3a-apply) calls it")
    )]
    pub(crate) fn new(mut entries: Vec<PlanEntry>) -> Self {
        for (index, entry) in entries.iter_mut().enumerate() {
            entry.item = ItemId(u32::try_from(index).unwrap_or(u32::MAX));
        }
        // What the digest covers of each item: everything but the display path and the
        // estimate.
        let covered: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.item,
                    entry.finding,
                    &entry.anchor,
                    entry.method,
                    &entry.standing,
                    &entry.judgement,
                    entry.file_system,
                    entry.tree.digest(),
                )
            })
            .collect();
        let digest = Digest::of_debug(&covered);
        Self { entries, digest }
    }

    /// Its items, in order.
    #[must_use]
    pub fn entries(&self) -> &[PlanEntry] {
        &self.entries
    }

    /// The digest of everything it authorizes. A reviewed authorization names it, so a
    /// plan other than the one shown can't be applied with it.
    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }
}
