//! Reconciling items a crash interrupted (`docs/04-safety-model.md` §7; spec v2 pinned 8,
//! v2.1 D, F).
//!
//! An item whose intent is durable and whose outcome isn't was interrupted. What happened
//! to it can't be told, so its outcome is `indeterminate`, with what a fresh look finds as
//! evidence: the target unchanged, partly gone, gone or replaced, compared with the tree
//! stored with the plan, or for a trash item the item at the slot its intent named, which
//! restores it. A gone entry is never counted as removed by kipple, since something else
//! may have removed it. Reconciliation only reads: it never deletes, moves or restores.

use crate::cancel::CancelToken;
use crate::frozen::{FrozenTree, Mark};
use crate::journal::{Intent, Journal};
use crate::mutation::Freeze;
use crate::outcome::{ApplyError, ErrorCode, Found, ItemOutcome, Trashed};
use crate::plan::{ItemId, Plan, PlanEntry};
use crate::planner::MutationPorts;
use crate::rule::Method;
use crate::trash::SlotState;

/// What a crash left unfinished: the stored plan, and the intents recorded for it that
/// have no outcome.
#[derive(Debug, Clone, Copy)]
pub struct Pending<'journal> {
    /// The plan, as stored before the first intent.
    pub plan: &'journal Plan,
    /// Its intents without an outcome, in the order they were recorded.
    pub intents: &'journal [Intent],
}

/// An item reconciled, with the outcome recorded for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// The item.
    pub item: ItemId,
    /// Always [`ItemOutcome::Indeterminate`], with what was found.
    pub outcome: ItemOutcome,
}

/// Looks at each pending item through `ports` and records it as indeterminate in
/// `journal`, with what was found.
///
/// Stops once `cancel` is set, recording nothing for the item it was looking at: that
/// one and the rest stay pending for the next start.
///
/// # Errors
/// When the journal can't record an outcome: the items not recorded stay pending.
pub fn reconcile(
    pending: &Pending<'_>,
    ports: &MutationPorts<'_>,
    journal: &mut dyn Journal,
    cancel: &CancelToken,
) -> Result<Vec<Reconciled>, ApplyError> {
    let mut reconciled = Vec::with_capacity(pending.intents.len());
    for intent in pending.intents {
        if cancel.is_cancelled() {
            break;
        }
        let found = stored(pending.plan, intent).map_or_else(
            || Found::Unreadable(ApplyError::new(ErrorCode::Journal)),
            |entry| find(entry, intent, ports, cancel),
        );
        if matches!(&found, Found::Unreadable(error) if error.code == ErrorCode::Cancelled) {
            break;
        }
        let outcome = ItemOutcome::Indeterminate(found);
        journal.outcome(intent.item, &outcome)?;
        reconciled.push(Reconciled {
            item: intent.item,
            outcome,
        });
    }
    Ok(reconciled)
}

/// The stored item `intent` was recorded for, if the intent matches the stored plan.
fn stored<'plan>(plan: &'plan Plan, intent: &Intent) -> Option<&'plan PlanEntry> {
    plan.entry(intent.item).filter(|entry| {
        intent.plan == plan.digest()
            && intent.reviewed == entry.tree().digest()
            && intent.method == entry.method()
    })
}

/// What is there now of `entry`, whose run `intent` started.
fn find(
    entry: &PlanEntry,
    intent: &Intent,
    ports: &MutationPorts<'_>,
    cancel: &CancelToken,
) -> Found {
    if let (Method::Trash, Some(slot)) = (intent.method, &intent.trash) {
        let Some(top) = entry.tree().top().snapshot() else {
            return Found::Unreadable(ApplyError::new(ErrorCode::Journal));
        };
        match ports.trash.find(slot, &top) {
            Ok(SlotState::Holds) => return Found::Trashed(Trashed { slot: slot.clone() }),
            Ok(SlotState::Empty | SlotState::Other) => {}
            Err(error) => return Found::Unreadable(error),
        }
    }
    match ports.freezer.freeze(entry.anchor(), cancel) {
        Ok(fresh) => compare(entry.tree(), &fresh),
        Err(error) if error.code == ErrorCode::TargetMissing => Found::Gone,
        Err(error) if error.code == ErrorCode::TargetChanged => Found::Replaced,
        Err(error) => Found::Unreadable(error),
    }
}

/// How `fresh` differs from the stored tree: by the identity of its top, then by how many
/// of the entries the plan authorized aren't at their place with their identity and kind.
fn compare(stored: &FrozenTree, fresh: &Freeze) -> Found {
    let identity = |tree: &FrozenTree| tree.top().snapshot().map(|top| (top.kind, top.identity));
    if identity(stored) != identity(&fresh.tree) {
        return Found::Replaced;
    }
    let mut of = 0;
    let mut gone = 0;
    for entry in stored.entries() {
        if entry.mark() != Mark::Remove {
            continue;
        }
        of += 1;
        let there = fresh
            .tree
            .find(&entry.path())
            .and_then(|id| fresh.tree.entry(id))
            .and_then(|now| now.snapshot());
        let same = there
            .zip(entry.snapshot())
            .is_some_and(|(now, then)| now.kind == then.kind && now.identity == then.identity);
        if !same {
            gone += 1;
        }
    }
    if gone == 0 {
        Found::Unchanged
    } else {
        Found::PartlyGone { gone, of }
    }
}
