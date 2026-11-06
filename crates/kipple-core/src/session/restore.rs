//! `restore`: moving a run's trashed items back, each recorded before and after.

use super::{ApplyEvent, Ending, EventSink, RunEvents, Session, StoreJournal, prepare};
use crate::cancel::CancelToken;
use crate::journal::{Journal, Record, RestoreResult};
use crate::outcome::{Found, ItemOutcome};
use crate::plan::ItemId;
use crate::run::{Depth, ItemState, RunRead, StoredItem, StoredRun, read_run};
use crate::store::RunId;
use crate::trash::{RestoreRequest, TrashSlot};

/// What `restore` asks for: a run, and some of its items or all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreAsk {
    /// The run.
    pub run: RunId,
    /// Its items to restore; empty for every item it moved to the trash.
    pub items: Vec<ItemId>,
}

/// What `restore` did with one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restoring {
    /// It was restored, or the restore failed, as recorded.
    Ran(RestoreResult),
    /// It was restored before.
    AlreadyRestored,
    /// The run didn't move it to the trash.
    NotTrashed,
    /// The run has no such item.
    NoSuchItem,
    /// The restore was cancelled before it.
    Cancelled,
}

/// One item `restore` looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    /// The item.
    pub item: ItemId,
    /// What happened.
    pub restoring: Restoring,
}

/// How `restore` finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreFinished {
    /// How it ended: `Done` when every item asked for was restored, `NoneRan` when none
    /// could be, `Failed` when a restore failed.
    pub ending: Ending,
    /// Each item looked at, in item order.
    pub items: Vec<Restored>,
}

/// `restore`: under the lock, after reconciling, moves `ask`'s items back from the trash,
/// recording each restore in its run before and after it.
pub fn run_restore(
    ask: &RestoreAsk,
    session: &Session<'_>,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
) -> RestoreFinished {
    let before = |ending| RestoreFinished {
        ending,
        items: Vec::new(),
    };
    let lock = match prepare(session, events, cancel) {
        Ok(lock) => lock,
        Err(ending) => return before(ending),
    };
    let run = match read_run(session.store, &ask.run, Depth::Trees, session.codec) {
        RunRead::Read(run) => run,
        RunRead::NeverStarted(_) => return before(Ending::NoneRan),
        RunRead::Unreadable(run, why) => {
            events.event(ApplyEvent::Unreadable { run, why });
            return before(Ending::Failed);
        }
    };
    let asked: Vec<ItemId> = if ask.items.is_empty() {
        run.plan().items().iter().map(StoredItem::item).collect()
    } else {
        ask.items.clone()
    };
    let mut restorer = Restorer {
        run: &run,
        session,
        journal: StoreJournal::after(&run, &lock, session),
        events: RunEvents {
            run: run.id().clone(),
            events,
        },
    };
    let mut items = Vec::with_capacity(asked.len());
    for item in asked {
        let restoring = if cancel.is_cancelled() {
            Restoring::Cancelled
        } else {
            restorer.restore(item, cancel)
        };
        items.push(Restored { item, restoring });
    }
    RestoreFinished {
        ending: restore_ending(&items),
        items,
    }
}

/// How restoring `items` ended: done when an item was moved back and none failed, like a
/// clean whose other items skipped; the items with nothing to restore say so one by one.
fn restore_ending(items: &[Restored]) -> Ending {
    let failed = items.iter().any(|item| {
        matches!(
            item.restoring,
            Restoring::Ran(RestoreResult::Failed(_) | RestoreResult::Indeterminate(_))
        )
    });
    if failed {
        Ending::Failed
    } else if items
        .iter()
        .any(|item| item.restoring == Restoring::Cancelled)
    {
        Ending::Cancelled
    } else if items
        .iter()
        .any(|item| item.restoring == Restoring::Ran(RestoreResult::Restored))
    {
        Ending::Done
    } else {
        Ending::NoneRan
    }
}

/// What restores the items of one run.
struct Restorer<'run> {
    run: &'run StoredRun,
    session: &'run Session<'run>,
    journal: StoreJournal<'run>,
    events: RunEvents<'run>,
}

impl Restorer<'_> {
    /// Restores `item`, if its run moved it to the trash and it wasn't restored since.
    fn restore(&mut self, item: ItemId, cancel: &CancelToken) -> Restoring {
        let Self {
            run,
            session,
            journal,
            events,
        } = self;
        restore_one(run, item, session, Steps { journal, events }, cancel)
    }
}

/// Where one restore records and reports its steps.
struct Steps<'steps, 'run> {
    journal: &'steps mut StoreJournal<'run>,
    events: &'steps mut RunEvents<'run>,
}

/// Restores `item` of `run`, if it moved to the trash and wasn't restored since.
/// Where `item` of `run` went in the trash: the slot its outcome records, the slot
/// reconciliation found it at, or, when its move couldn't be made durable, the slot its
/// intent named (the trash checks the slot holds the item before moving it back).
fn trashed_slot(run: &StoredRun, item: ItemId) -> Option<TrashSlot> {
    let ItemState::Ended(outcome) = run.state(item) else {
        return None;
    };
    if let ItemOutcome::Indeterminate(Found::Trashed(trashed)) = outcome {
        return Some(trashed.slot.clone());
    }
    if let ItemOutcome::Indeterminate(Found::Unsynced(_)) = outcome {
        return run.records().iter().rev().find_map(|record| {
            if let Record::Intent(intent) = record
                && intent.item == item
            {
                intent.trash.clone()
            } else {
                None
            }
        });
    }
    outcome
        .file_effect()
        .and_then(|effect| effect.trashed.as_ref())
        .map(|trashed| trashed.slot.clone())
}

fn restore_one(
    run: &StoredRun,
    item: ItemId,
    session: &Session<'_>,
    steps: Steps<'_, '_>,
    cancel: &CancelToken,
) -> Restoring {
    let Steps { journal, events } = steps;
    let Some(stored) = run.plan().item(item) else {
        return Restoring::NoSuchItem;
    };
    let Some(slot) = trashed_slot(run, item) else {
        return Restoring::NotTrashed;
    };
    if journal.restored.contains(&item) || run.restored(item) == Some(&RestoreResult::Restored) {
        return Restoring::AlreadyRestored;
    }
    let top = stored.tree().and_then(|tree| tree.top().snapshot());
    let Some(request) =
        top.and_then(|top| RestoreRequest::new(slot.clone(), stored.anchor().clone(), top))
    else {
        return Restoring::NotTrashed;
    };
    let intent = Record::RestoreIntent { item, slot };
    if let Err(error) = journal.record(&intent) {
        return Restoring::Ran(RestoreResult::Failed(error));
    }
    events.restore(RestoreStep::Started, item);
    let result = match session.ports.mutation.trash.restore(&request, cancel) {
        Ok(()) => RestoreResult::Restored,
        Err(error) => RestoreResult::Failed(error),
    };
    let ended = Record::RestoreOutcome {
        item,
        result: result.clone(),
    };
    let result = match journal.record(&ended) {
        Ok(()) => result,
        Err(error) => RestoreResult::Failed(error),
    };
    if result == RestoreResult::Restored {
        journal.restored.push(item);
    }
    events.restore(RestoreStep::Ended(result.clone()), item);
    Restoring::Ran(result)
}

/// A step of a restore.
#[derive(Debug)]
enum RestoreStep {
    Started,
    Ended(RestoreResult),
}

impl RunEvents<'_> {
    fn restore(&mut self, step: RestoreStep, item: ItemId) {
        let run = self.run.clone();
        self.events.event(match step {
            RestoreStep::Started => ApplyEvent::RestoreStarted { run, item },
            RestoreStep::Ended(result) => ApplyEvent::RestoreEnded { run, item, result },
        });
    }
}
