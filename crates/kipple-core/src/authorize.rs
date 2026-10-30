//! Authorization: the one place an operation that may change files is made
//! (`docs/04-safety-model.md` §1, §2, §3.5, §3.6).
//!
//! An [`AuthorizedOp`] has no public constructor. Only revalidation in this crate makes
//! one, from an item of a [`Plan`], the user's [`Authorization`] and the item's facts
//! established afresh at apply. A frontend, the platform or a plan file can't make one,
//! so nothing reaches the [`Executor`](crate::Executor) or the
//! [`TrashBackend`](crate::TrashBackend) without passing through it.
//!
//! A delete and a trash move are different types: a [`TrashOp`] can't become a
//! [`DeleteOp`], so a failed trash move can't fall back to deleting.
//!
//! ```compile_fail,E0616
//! // A trash item can't be turned into a delete.
//! fn fall_back(op: kipple_core::TrashOp) -> kipple_core::DeleteOp {
//!     kipple_core::DeleteOp(op.0)
//! }
//! ```

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "the apply loop (M3a-apply) calls `admit`")
)]

#[cfg(test)]
mod tests;

use std::sync::Arc;

use crate::digest::Digest;
use crate::frozen::{FrozenTree, Mark, Marks, NodeId};
use crate::mutation::{FileSystemClass, Freeze};
use crate::outcome::{LeftReason, Skip, Tally};
use crate::plan::{Anchor, ItemId, Judgement, Plan, PlanEntry, Standing};
use crate::policy::{Proposal, Verdict};
use crate::rule::Method;

/// What the user authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authorization {
    /// The user selected these items and saw this plan, frozen, in review. Items may be
    /// Safe or Review; each keeps the cautions it was reviewed with.
    Reviewed {
        /// The digest of the plan shown ([`Plan::digest`]). A plan with another digest
        /// isn't authorized.
        plan: Digest,
    },
    /// Automation (`--only`, a profile, an imported plan, each with `--yes`): Safe items
    /// only, never Review, at planning and again at apply, and deletes only: a trash move
    /// takes a directory whole, which needs a review automation doesn't have yet (M3b).
    SafeOnly,
}

/// What an operation may change: an item's reviewed tree with what revalidation left of
/// it. Reachable only inside a [`DeleteOp`] or a [`TrashOp`].
#[derive(Debug)]
pub struct Authorized {
    item: ItemId,
    anchor: Anchor,
    tree: Arc<FrozenTree>,
    marks: Marks,
    digest: Digest,
}

impl Authorized {
    /// The plan item.
    #[must_use]
    pub const fn item(&self) -> ItemId {
        self.item
    }

    /// Where the target is.
    #[must_use]
    pub const fn anchor(&self) -> &Anchor {
        &self.anchor
    }

    /// The tree the user reviewed.
    #[must_use]
    pub fn tree(&self) -> &FrozenTree {
        &self.tree
    }

    /// What of the tree may still be removed.
    #[must_use]
    pub const fn marks(&self) -> &Marks {
        &self.marks
    }

    /// Whether `id` may be removed.
    #[must_use]
    pub fn may_remove(&self, id: NodeId) -> bool {
        self.marks.is_authorized(id)
    }

    /// The digest the intent records for this set ([`FrozenTree::digest_with`]).
    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }
}

/// An authorized delete. Only [`Executor::delete`](crate::Executor::delete) runs it.
#[derive(Debug)]
pub struct DeleteOp(Authorized);

/// An authorized trash move of a whole item: every entry of its reviewed tree is still
/// authorized, and none was left out. Only a [`TrashBackend`](crate::TrashBackend)
/// runs it.
#[derive(Debug)]
pub struct TrashOp(Authorized);

impl DeleteOp {
    /// What it may remove.
    #[must_use]
    pub const fn target(&self) -> &Authorized {
        &self.0
    }
}

impl TrashOp {
    /// What it moves.
    #[must_use]
    pub const fn target(&self) -> &Authorized {
        &self.0
    }
}

/// An operation revalidation authorized.
#[derive(Debug)]
pub enum AuthorizedOp {
    /// A delete.
    Delete(DeleteOp),
    /// A trash move.
    Trash(TrashOp),
}

impl AuthorizedOp {
    /// What it acts on.
    #[must_use]
    pub const fn target(&self) -> &Authorized {
        match self {
            Self::Delete(op) => op.target(),
            Self::Trash(op) => op.target(),
        }
    }

    /// Its method.
    #[must_use]
    pub const fn method(&self) -> Method {
        match self {
            Self::Delete(_) => Method::Delete,
            Self::Trash(_) => Method::Trash,
        }
    }
}

/// An item's facts established afresh at apply, through the same ports and
/// [`Policy::verdict`](crate::Policy::verdict) as the scan, under the current policy, with
/// a fresh activity snapshot and a fresh freeze.
#[derive(Debug)]
pub(crate) struct Recheck<'facts> {
    /// The verdict now.
    pub(crate) verdict: &'facts Verdict,
    /// What the rule proposes now.
    pub(crate) proposal: &'facts Proposal,
    /// What the item is judged under now.
    pub(crate) judgement: &'facts Judgement,
    /// The target frozen now.
    pub(crate) freeze: Freeze,
}

/// Authorizes `item` of `plan`, or says why not. Only ever subtracts: an item survives
/// only if every check passes, and its tree only with what the fresh freeze confirms.
///
/// In order: the item must be in the plan and the authorization for this plan, and
/// Safe-only only for a Safe item removed by a delete; the item must be judged under the
/// same policy, rule, level, recent window, method and kept and excluded paths; it must
/// not be Protected now; its bucket must still be one it was authorized in (a Safe item
/// stays Safe, a Review item gains no caution, and under Safe-only it is Safe); its rule
/// must still offer its method; its file system must be supported; the fresh freeze must
/// confirm at least one reviewed entry; and a trash move needs every entry of the tree
/// confirmed, nothing left out.
pub(crate) fn admit(
    plan: &Plan,
    item: ItemId,
    authorization: Authorization,
    recheck: Recheck<'_>,
) -> Result<AuthorizedOp, Skip> {
    let entry = plan.entry(item).ok_or(Skip::NotReviewed)?;
    authorized_for(plan, entry, authorization)?;
    if let Some(field) = entry.judgement.first_difference(recheck.judgement) {
        return Err(Skip::Rejudged(field));
    }
    still_allowed(&entry.standing, recheck.verdict, authorization)?;
    match recheck.proposal {
        Proposal::Remove(methods) if methods.contains(&entry.method) => {}
        Proposal::Remove(_) | Proposal::Report | Proposal::Advice => {
            return Err(Skip::MethodNotOffered(entry.method));
        }
    }
    let freeze = recheck.freeze;
    if let FileSystemClass::Unsupported(why) = freeze.file_system {
        return Err(Skip::FileSystem(why));
    }
    let tree = &entry.tree;
    let mut marks = tree.recheck(&freeze.tree);
    if entry.anchor.parent != freeze.parent {
        marks.drop_subtree(tree, NodeId::TOP, LeftReason::Replaced);
    }
    if marks.authorized() == 0 {
        return Err(Skip::AllDropped(Tally::new(tree, &marks).before_run().left));
    }
    let whole = marks_whole(&marks, tree);
    let target = Authorized {
        item: entry.item,
        anchor: entry.anchor.clone(),
        digest: tree.digest_with(&marks),
        tree: Arc::clone(tree),
        marks,
    };
    match entry.method {
        Method::Delete => Ok(AuthorizedOp::Delete(DeleteOp(target))),
        Method::Trash if whole => Ok(AuthorizedOp::Trash(TrashOp(target))),
        Method::Trash => {
            let effect = Tally::new(tree, &target.marks).before_run();
            Err(Skip::NotWhole {
                left: effect.left,
                excluded: effect.excluded,
            })
        }
    }
}

/// Whether every entry of `tree` is still to be removed: nothing dropped, excluded, or
/// kept as a root.
fn marks_whole(marks: &Marks, tree: &FrozenTree) -> bool {
    tree.entries()
        .all(|entry| marks.get(entry.id()) == Mark::Remove)
}

fn authorized_for(
    plan: &Plan,
    entry: &PlanEntry,
    authorization: Authorization,
) -> Result<(), Skip> {
    match authorization {
        Authorization::Reviewed { plan: shown } if shown != plan.digest() => Err(Skip::NotReviewed),
        Authorization::SafeOnly => match (&entry.standing, entry.method) {
            (Standing::Review(cautions), _) => {
                Err(Skip::Riskier(cautions.iter().copied().collect()))
            }
            // A trash move takes the directory whole, with whatever is in it then (04
            // §3.5): automation has no review of that until a profile records one (M3b).
            (Standing::Safe, Method::Trash) => Err(Skip::WholeNotReviewed),
            (Standing::Safe, Method::Delete) => Ok(()),
        },
        Authorization::Reviewed { .. } => Ok(()),
    }
}

/// Whether `verdict` now is within the `standing` the item was authorized with.
fn still_allowed(
    standing: &Standing,
    verdict: &Verdict,
    authorization: Authorization,
) -> Result<(), Skip> {
    let cautions = match verdict {
        Verdict::Protected(holds) => return Err(Skip::Protected(holds.clone())),
        Verdict::Safe => return Ok(()),
        Verdict::Review(cautions) => cautions,
    };
    let reviewed = match (standing, authorization) {
        (Standing::Review(reviewed), Authorization::Reviewed { .. }) => reviewed,
        (Standing::Safe, _) | (Standing::Review(_), Authorization::SafeOnly) => {
            return Err(Skip::Riskier(cautions.clone()));
        }
    };
    let new: Vec<_> = cautions
        .iter()
        .filter(|caution| !reviewed.contains(caution))
        .copied()
        .collect();
    if new.is_empty() {
        Ok(())
    } else {
        Err(Skip::Riskier(new))
    }
}
