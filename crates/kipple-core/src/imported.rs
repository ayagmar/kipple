//! Plans read from a file: never run as written (spec M3b pinned 2, D-067).
//!
//! `plan -o` writes a plan as `kipple.plan/v1`. Reading one gives an [`ImportedPlan`],
//! which authorizes nothing and can't be applied:
//!
//! ```compile_fail,E0308
//! // An imported plan can't be applied: apply takes a plan the planner made.
//! fn run(imported: &kipple_core::ImportedPlan) -> kipple_core::ApplyRequest<'_> {
//!     kipple_core::ApplyRequest { plan: imported, authorization: kipple_core::Authorization::SafeOnly }
//! }
//! ```
//!
//! Its only use is [`replan`], which plans again from a fresh scan under the current
//! config and lets the file only narrow that plan: an item the file names must be found
//! again at the same place by the same rule, below a root the current config scans with
//! the identity it had, Safe now, and removed by the method the file names only where the
//! current config's method is the same or the file's narrows it (trash over delete). Each
//! item then removes only the entries both the fresh freeze and the file's tree authorize.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::cancel::CancelToken;
use crate::engine::{FindingId, ScanReport};
use crate::frozen::{FrozenTree, Mark, NodeId, RelPath};
use crate::outcome::Skip;
use crate::plan::{Plan, PlanEntry, Selection, VerifiedRoot};
use crate::planner::{
    Judge, Methods, MutationPorts, PlanMode, PlanRequest, Planned, leave_out, plan,
};
use crate::probe::EntryKind;
use crate::rule::{Method, RuleId};

/// A plan read from a file. Untrusted: it authorizes nothing, and its only use is
/// [`replan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedPlan {
    pub(crate) items: Vec<ImportedItem>,
}

impl ImportedPlan {
    /// How many items the file names.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether it names none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// One item of a plan file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedItem {
    pub(crate) root: VerifiedRoot,
    pub(crate) relative: RelPath,
    pub(crate) rule: RuleId,
    pub(crate) method: Method,
    pub(crate) tree: FrozenTree,
}

impl ImportedItem {
    fn path(&self) -> PathBuf {
        self.relative.below(&self.root.path)
    }
}

/// Why an item of a plan file isn't in the fresh plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dropped {
    /// The fresh scan has no finding at its place by its rule: it is gone, its root isn't
    /// one the current config scans, or its rule no longer finds it.
    NotFound,
    /// It is below another root now, or its root has another identity.
    RootChanged,
    /// The file removes it by a method the current config doesn't choose, and that isn't
    /// narrower (a delete where the config says trash).
    MethodWidened {
        /// The file's method.
        file: Method,
        /// The current config's.
        now: Method,
    },
    /// The file names its rule with two methods: only the current config's is kept.
    MethodsDiffer,
    /// Planning it now refused it, as for any selected finding: a Review item, which
    /// needs the TUI (pinned 1), is [`Skip::Riskier`].
    Refused(Skip),
    /// Its fresh tree and the file's share no entry to remove.
    NothingInCommon,
    /// Another item of the file is the same item now.
    Duplicate,
}

/// An item of a plan file left out of the fresh plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedItem {
    /// Its place in the file, from 0.
    pub index: usize,
    /// Where the file says it is.
    pub path: PathBuf,
    /// Why it was left out.
    pub why: Dropped,
}

/// A plan file planned again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replanned {
    /// The fresh plan, narrowed by the file. Empty when every item dropped, which after a
    /// kipple upgrade means the file is stale.
    pub plan: Plan,
    /// The methods it was planned with: the current config's, narrowed to the file's.
    /// Apply judges it with these.
    pub methods: Methods,
    /// The file's items left out, in file order.
    pub dropped: Vec<DroppedItem>,
}

/// What to plan again.
#[derive(Debug, Clone, Copy)]
pub struct ReplanRequest<'file> {
    /// The plan file, read.
    pub imported: &'file ImportedPlan,
    /// A fresh scan of the roots the current config resolves.
    pub scan: &'file ScanReport,
    /// [`PlanMode::SafeOnly`] for `apply --yes`, [`PlanMode::Safe`] for a person who
    /// reviews the summary. Review items drop either way.
    pub mode: PlanMode,
}

/// Plans `request`'s file again from its fresh scan, under `judge`, the current config.
///
/// Nothing the file says widens what the fresh plan would remove.
#[must_use]
pub fn replan(
    request: &ReplanRequest<'_>,
    judge: &Judge<'_>,
    ports: &MutationPorts<'_>,
    cancel: &CancelToken,
) -> Replanned {
    let ReplanRequest {
        imported,
        scan,
        mode,
    } = *request;
    let mut dropped = Vec::new();
    let methods = narrowed(imported, judge.methods, &mut dropped);
    let found = matched(imported, scan, &mut dropped);
    let selection = Selection(found.keys().copied().collect());
    let judge = Judge {
        methods: &methods,
        ..*judge
    };
    let planned = plan(
        &PlanRequest {
            scan,
            selection: &selection,
            mode,
        },
        &judge,
        ports,
        cancel,
    );
    let plan = narrowed_plan(imported, &found, planned, &mut dropped);
    dropped.sort_by_key(|item| item.index);
    dropped.dedup_by_key(|item| item.index);
    Replanned {
        plan,
        methods,
        dropped,
    }
}

/// The current `methods`, narrowed by the file: a rule the file names only with trash
/// where the current method is a delete gets trash; every other rule keeps the current
/// method. An item whose method isn't the one its rule gets is dropped.
fn narrowed(imported: &ImportedPlan, methods: &Methods, dropped: &mut Vec<DroppedItem>) -> Methods {
    let mut by_rule: BTreeMap<&RuleId, BTreeSet<Method>> = BTreeMap::new();
    for item in &imported.items {
        by_rule.entry(&item.rule).or_default().insert(item.method);
    }
    let mut narrowed = Methods {
        command_line: None,
        rules: methods.rules.clone(),
        config: methods.config,
    };
    for (rule, file) in &by_rule {
        let now = methods.choice(rule).resolve();
        let only_trash = file.len() == 1 && file.contains(&Method::Trash);
        let kept = if only_trash { Method::Trash } else { now };
        narrowed.rules.insert((*rule).clone(), kept);
    }
    for (index, item) in imported.items.iter().enumerate() {
        if narrowed.choice(&item.rule).resolve() == item.method {
            continue;
        }
        let why = if by_rule.get(&item.rule).is_some_and(|all| all.len() > 1) {
            Dropped::MethodsDiffer
        } else {
            Dropped::MethodWidened {
                file: item.method,
                now: methods.choice(&item.rule).resolve(),
            }
        };
        dropped.push(drop(index, item, why));
    }
    narrowed
}

/// The fresh finding each item of the file names, by finding: the one at its place found
/// by its rule. Items already dropped, found nowhere or naming a finding another item
/// named are dropped.
fn matched(
    imported: &ImportedPlan,
    scan: &ScanReport,
    dropped: &mut Vec<DroppedItem>,
) -> BTreeMap<FindingId, Vec<usize>> {
    let gone: BTreeSet<usize> = dropped.iter().map(|item| item.index).collect();
    let by_place: HashMap<(&Path, &RuleId), FindingId> = scan
        .findings
        .iter()
        .map(|finding| ((finding.path.as_path(), &finding.rule), finding.id))
        .collect();
    let mut found: BTreeMap<FindingId, Vec<usize>> = BTreeMap::new();
    for (index, item) in imported.items.iter().enumerate() {
        if gone.contains(&index) {
            continue;
        }
        let path = item.path();
        match by_place.get(&(path.as_path(), &item.rule)) {
            Some(id) if found.contains_key(id) => {
                dropped.push(drop(index, item, Dropped::Duplicate));
            }
            Some(id) => {
                found.insert(*id, vec![index]);
            }
            None => dropped.push(drop(index, item, Dropped::NotFound)),
        }
    }
    found
}

/// `planned` with each item narrowed to the file's tree, or dropped: refused, below
/// another root than the file's, or with nothing left in common.
fn narrowed_plan(
    imported: &ImportedPlan,
    found: &BTreeMap<FindingId, Vec<usize>>,
    planned: Planned,
    dropped: &mut Vec<DroppedItem>,
) -> Plan {
    let mut found = found.clone();
    for merged in &planned.merged {
        let Some(head) = planned.plan.entry(merged.into).map(PlanEntry::finding) else {
            continue;
        };
        let alias = found.remove(&merged.finding).unwrap_or_default();
        found.entry(head).or_default().extend(alias);
    }
    for refused in &planned.refused {
        for index in found.remove(&refused.finding).unwrap_or_default() {
            if let Some(item) = imported.items.get(index) {
                dropped.push(drop(index, item, Dropped::Refused(refused.skip.clone())));
            }
        }
    }
    let mut entries = Vec::new();
    for mut entry in planned.plan.into_entries() {
        let indices = found.remove(&entry.finding()).unwrap_or_default();
        let items: Vec<(usize, &ImportedItem)> = indices
            .iter()
            .filter_map(|index| Some((*index, imported.items.get(*index)?)))
            .collect();
        match narrow(&mut entry, &items) {
            Ok(()) => entries.push(entry),
            Err(why) => dropped.extend(
                items
                    .iter()
                    .map(|(index, item)| drop(*index, item, why.clone())),
            ),
        }
    }
    Plan::new(entries)
}

/// Narrows `entry` to what every file item planned as it authorizes, or says why it
/// drops.
fn narrow(entry: &mut PlanEntry, items: &[(usize, &ImportedItem)]) -> Result<(), Dropped> {
    if items.is_empty() {
        return Err(Dropped::NotFound);
    }
    for (_, item) in items {
        let anchor = entry.anchor();
        if anchor.root != item.root || anchor.relative != item.relative {
            return Err(Dropped::RootChanged);
        }
        if entry.method() != item.method {
            return Err(Dropped::MethodWidened {
                file: item.method,
                now: entry.method(),
            });
        }
        let outside = outside(entry.tree(), &item.tree);
        leave_out(entry, &outside);
    }
    if entry.tree().entries().any(|one| one.mark() == Mark::Remove) {
        Ok(())
    } else {
        Err(Dropped::NothingInCommon)
    }
}

/// The entries `fresh` authorizes that `file` doesn't: at the same path below the target,
/// authorized in the file, of the same kind and identity, and for anything but a
/// directory with the same fingerprint. A directory's times change whenever an entry is
/// added below it, and each such entry is left out on its own.
fn outside(fresh: &FrozenTree, file: &FrozenTree) -> Vec<NodeId> {
    fresh
        .entries()
        .filter(|entry| entry.mark() == Mark::Remove)
        .filter(|entry| {
            let then = file
                .find(&entry.path())
                .filter(|id| file.mark(*id) == Mark::Remove)
                .and_then(|id| file.entry(id))
                .and_then(|then| then.snapshot());
            let same = then.zip(entry.snapshot()).is_some_and(|(then, now)| {
                then.kind == now.kind
                    && then.identity == now.identity
                    && (now.kind == EntryKind::Dir || then.fingerprint == now.fingerprint)
            });
            !same
        })
        .map(|entry| entry.id())
        .collect()
}

fn drop(index: usize, item: &ImportedItem, why: Dropped) -> DroppedItem {
    DroppedItem {
        index,
        path: item.path(),
        why,
    }
}
