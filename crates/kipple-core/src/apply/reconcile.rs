//! Reconciling items a crash interrupted (`docs/04-safety-model.md` §7; spec M3b pinned 8,
//! 9, 12).
//!
//! An item whose intent is durable and whose outcome isn't was interrupted. What happened
//! to it can't be told, so its outcome is `indeterminate`, with what a fresh look finds as
//! evidence: the target unchanged, partly gone, gone or replaced, compared with the tree
//! stored with the plan, or for a trash item the item at the slot its intent named, which
//! restores it. A gone entry is never counted as removed by kipple, since something else
//! may have removed it. Every comparison is with the digests stored, never computed again.
//!
//! Reconciliation never deletes, moves or restores. The one thing it removes is the trash
//! record a move wrote before a crash kept the item from moving ([`TrashBackend::forget_unmoved`]),
//! and only when the slot is empty and the item is unchanged at its place, recorded
//! before the outcome. A restore a crash interrupted is reconciled the same way: restored
//! when the item is back at its place, else indeterminate with what was found.
//!
//! [`TrashBackend::forget_unmoved`]: crate::TrashBackend::forget_unmoved

use crate::cancel::CancelToken;
use crate::frozen::{FrozenTree, Mark};
use crate::journal::{Intent, Journal, Record, RestoreResult};
use crate::mutation::Freeze;
use crate::outcome::{ApplyError, ErrorCode, Found, ItemOutcome, Trashed};
use crate::plan::ItemId;
use crate::planner::MutationPorts;
use crate::rule::Method;
use crate::run::{StoredItem, StoredRun};
use crate::trash::{Forgotten, SlotState, TrashSlot};

/// An item reconciled, with what was recorded for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciled {
    /// An interrupted run: always [`ItemOutcome::Indeterminate`], with what was found.
    Item {
        /// The item.
        item: ItemId,
        /// Its outcome.
        outcome: ItemOutcome,
    },
    /// An interrupted restore.
    Restore {
        /// The item.
        item: ItemId,
        /// How it ended.
        result: RestoreResult,
    },
}

/// Looks at each pending item and restore of `run`, read with its trees, through
/// `ports`, and records what was found in `journal`, which appends to `run`.
///
/// Stops once `cancel` is set, recording nothing for the item it was looking at: that
/// one and the rest stay pending for the next start.
///
/// # Errors
/// When the journal can't record: the items not recorded stay pending.
pub fn reconcile(
    run: &StoredRun,
    ports: &MutationPorts<'_>,
    journal: &mut dyn Journal,
    cancel: &CancelToken,
) -> Result<Vec<Reconciled>, ApplyError> {
    let mut reconciled = Vec::new();
    for intent in run.pending() {
        if cancel.is_cancelled() {
            return Ok(reconciled);
        }
        let stored = stored(run, intent);
        let found = stored.map_or_else(
            || Found::Unreadable(ApplyError::new(ErrorCode::Journal)),
            |(item, tree)| find(item, tree, intent, ports, cancel),
        );
        if cancelled(&found) {
            return Ok(reconciled);
        }
        forget(&found, intent, stored.map(|(item, _)| item), ports, journal)?;
        let outcome = ItemOutcome::Indeterminate(found);
        journal.record(&Record::Outcome {
            item: intent.item,
            outcome: outcome.clone(),
        })?;
        reconciled.push(Reconciled::Item {
            item: intent.item,
            outcome,
        });
    }
    for pending in run.pending_restores() {
        if cancel.is_cancelled() {
            break;
        }
        let item = pending.item;
        let result = restored(run, item, pending.slot, ports, cancel);
        if matches!(&result, RestoreResult::Indeterminate(found) if cancelled(found)) {
            break;
        }
        journal.record(&Record::RestoreOutcome {
            item,
            result: result.clone(),
        })?;
        reconciled.push(Reconciled::Restore { item, result });
    }
    Ok(reconciled)
}

fn cancelled(found: &Found) -> bool {
    matches!(found, Found::Unreadable(error) if error.code == ErrorCode::Cancelled)
}

/// The stored item `intent` was recorded for, with its tree, if the intent matches it as
/// stored.
fn stored<'run>(
    run: &'run StoredRun,
    intent: &Intent,
) -> Option<(&'run StoredItem, &'run FrozenTree)> {
    let item = run.plan().item(intent.item)?;
    let matches = intent.plan == run.plan().digest()
        && intent.reviewed == item.tree_digest()
        && intent.method == item.method();
    let tree = item.tree()?;
    matches.then_some((item, tree))
}

/// What is there now of `item`, whose run `intent` started.
fn find(
    item: &StoredItem,
    tree: &FrozenTree,
    intent: &Intent,
    ports: &MutationPorts<'_>,
    cancel: &CancelToken,
) -> Found {
    if let (Method::Trash, Some(slot)) = (intent.method, &intent.trash) {
        let Some(top) = tree.top().snapshot() else {
            return Found::Unreadable(ApplyError::new(ErrorCode::Journal));
        };
        match ports.trash.find(slot, &top) {
            Ok(SlotState::Holds) => return Found::Trashed(Trashed { slot: slot.clone() }),
            Ok(SlotState::Empty | SlotState::Other) => {}
            Err(error) => return Found::Unreadable(error),
        }
    }
    match ports.freezer.freeze(item.anchor(), cancel) {
        Ok(fresh) => compare(tree, &fresh),
        Err(error) if error.code == ErrorCode::TargetMissing => Found::Gone,
        Err(error) if error.code == ErrorCode::TargetChanged => Found::Replaced,
        Err(error) => Found::Unreadable(error),
    }
}

/// Removes the trash record of a move that never happened: the intent named a slot, the
/// slot is empty and the item is unchanged at its place. Records it when one was removed.
fn forget(
    found: &Found,
    intent: &Intent,
    item: Option<&StoredItem>,
    ports: &MutationPorts<'_>,
    journal: &mut dyn Journal,
) -> Result<(), ApplyError> {
    let (Found::Unchanged, Some(slot), Some(item)) = (found, &intent.trash, item) else {
        return Ok(());
    };
    let empty = item
        .tree()
        .and_then(|tree| tree.top().snapshot())
        .is_some_and(|top| matches!(ports.trash.find(slot, &top), Ok(SlotState::Empty)));
    if !empty {
        return Ok(());
    }
    // A record that can't be removed only leaves a file manager listing an empty slot.
    if ports.trash.forget_unmoved(slot, item.anchor()) == Ok(Forgotten::Removed) {
        journal.record(&Record::TrashRecordRemoved {
            item: intent.item,
            slot: slot.clone(),
        })?;
    }
    Ok(())
}

/// How the restore of `item` from `slot` a crash interrupted ended: restored when the
/// item is back at its place, the same file; else indeterminate with what was found.
fn restored(
    run: &StoredRun,
    item: ItemId,
    slot: &TrashSlot,
    ports: &MutationPorts<'_>,
    cancel: &CancelToken,
) -> RestoreResult {
    let Some((stored, tree)) = run
        .plan()
        .item(item)
        .and_then(|stored| Some((stored, stored.tree()?)))
    else {
        return RestoreResult::Indeterminate(Found::Unreadable(ApplyError::new(
            ErrorCode::Journal,
        )));
    };
    let Some(top) = tree.top().snapshot() else {
        return RestoreResult::Indeterminate(Found::Unreadable(ApplyError::new(
            ErrorCode::Journal,
        )));
    };
    match ports.trash.find(slot, &top) {
        Ok(SlotState::Holds) => {
            return RestoreResult::Indeterminate(Found::Trashed(Trashed { slot: slot.clone() }));
        }
        Ok(SlotState::Empty | SlotState::Other) => {}
        Err(error) => return RestoreResult::Indeterminate(Found::Unreadable(error)),
    }
    match ports.freezer.freeze(stored.anchor(), cancel) {
        Ok(fresh)
            if fresh
                .tree
                .top()
                .snapshot()
                .map(|now| (now.kind, now.identity))
                == Some((top.kind, top.identity)) =>
        {
            RestoreResult::Restored
        }
        Ok(_) => RestoreResult::Indeterminate(Found::Replaced),
        Err(error) if error.code == ErrorCode::TargetMissing => {
            RestoreResult::Indeterminate(Found::Gone)
        }
        Err(error) if error.code == ErrorCode::TargetChanged => {
            RestoreResult::Indeterminate(Found::Replaced)
        }
        Err(error) => RestoreResult::Indeterminate(Found::Unreadable(error)),
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
