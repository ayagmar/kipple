//! What a mutating command tells its frontend.

use crate::apply::{ItemEvent, Reconciled};
use crate::journal::RestoreResult;
use crate::plan::ItemId;
use crate::run::Unreadable;
use crate::store::RunId;

/// What a mutating command tells its frontend, in the order it happens
/// (`kipple.apply-event/v1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyEvent {
    /// An item or restore of an earlier run a crash interrupted was reconciled.
    Reconciled {
        /// The run.
        run: RunId,
        /// What was recorded.
        reconciled: Reconciled,
    },
    /// An earlier run can't be read: nothing in it counts as removed, and it doesn't
    /// block this one.
    Unreadable {
        /// The run.
        run: RunId,
        /// Why.
        why: Unreadable,
    },
    /// This command's run was stored, and its items start.
    RunStarted {
        /// The run.
        run: RunId,
        /// How many items it has.
        items: usize,
    },
    /// An item of this run started or ended.
    Item {
        /// The run.
        run: RunId,
        /// What happened.
        event: ItemEvent,
    },
    /// A restore of an item is about to move it back: its intent is durable.
    RestoreStarted {
        /// The run.
        run: RunId,
        /// The item.
        item: ItemId,
    },
    /// A restore of an item ended, and is recorded.
    RestoreEnded {
        /// The run.
        run: RunId,
        /// The item.
        item: ItemId,
        /// How it ended.
        result: RestoreResult,
    },
}

/// Where a mutating command reports what it does.
pub trait EventSink {
    /// `event` happened.
    fn event(&mut self, event: ApplyEvent);
}

/// Ignores every event.
impl EventSink for () {
    fn event(&mut self, _: ApplyEvent) {}
}
