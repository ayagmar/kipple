//! `kipple.apply-event/v1`: one JSON line per step of a mutating command, in the order
//! it happened, after the scan's `kipple.event/v1` lines (`--events jsonl`). Each line
//! names its format, so a reader can tell the two apart.

use schemars::JsonSchema;
use serde::Serialize;

use super::APPLY_EVENT;
use super::journal::RestoreOut;
use super::outcome::OutcomeOut;
use crate::apply::{ItemEvent, Reconciled};
use crate::frozen::NameCodec;
use crate::run::Unreadable;
use crate::session::ApplyEvent;

/// One line of the stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ApplyEventOut {
    /// `kipple.apply-event/v1`.
    pub schema: String,
    /// Its position in the stream, counted on from the scan's lines.
    pub seq: u64,
    /// What happened.
    #[serde(flatten)]
    pub event: EventKindOut,
}

/// What happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum EventKindOut {
    /// An item of an earlier run a crash interrupted was reconciled.
    Reconciled {
        /// The run.
        run: String,
        /// The item.
        item: u32,
        /// The outcome recorded: always indeterminate.
        outcome: OutcomeOut,
    },
    /// A restore of an earlier run a crash interrupted was reconciled.
    RestoreReconciled {
        /// The run.
        run: String,
        /// The item.
        item: u32,
        /// How it ended.
        result: RestoreOut,
    },
    /// An earlier run can't be read; nothing in it counts as removed.
    UnreadableRun {
        /// The run.
        run: String,
        /// What can't be read.
        part: UnreadablePart,
    },
    /// This command's run was stored and its items start.
    RunStarted {
        /// The run.
        run: String,
        /// How many items it has.
        items: u64,
    },
    /// An item's intent is durable and its operation starts.
    ItemStarted {
        /// The run.
        run: String,
        /// The item.
        item: u32,
    },
    /// An item ended, and its outcome is recorded.
    ItemEnded {
        /// The run.
        run: String,
        /// The item.
        item: u32,
        /// How it ended.
        outcome: OutcomeOut,
    },
    /// A restore's intent is durable and the item moves back.
    RestoreStarted {
        /// The run.
        run: String,
        /// The item.
        item: u32,
    },
    /// A restore ended, and is recorded.
    RestoreEnded {
        /// The run.
        run: String,
        /// The item.
        item: u32,
        /// How it ended.
        result: RestoreOut,
    },
}

/// What of a run can't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum UnreadablePart {
    /// Its stored plan.
    Plan,
    /// Its journal, past a corrupt line.
    Journal,
    /// Its records' order.
    Order,
    /// Its files.
    Files,
}

impl ApplyEventOut {
    /// `event` at position `seq`.
    #[must_use]
    pub fn of(event: &ApplyEvent, seq: u64, codec: &dyn NameCodec) -> Self {
        Self {
            schema: APPLY_EVENT.to_owned(),
            seq,
            event: kind(event, codec),
        }
    }
}

fn kind(event: &ApplyEvent, codec: &dyn NameCodec) -> EventKindOut {
    match event {
        ApplyEvent::Reconciled { run, reconciled } => match reconciled {
            Reconciled::Item { item, outcome } => EventKindOut::Reconciled {
                run: run.as_str().to_owned(),
                item: item.0,
                outcome: OutcomeOut::of(outcome, codec),
            },
            Reconciled::Restore { item, result } => EventKindOut::RestoreReconciled {
                run: run.as_str().to_owned(),
                item: item.0,
                result: RestoreOut::of(result, codec),
            },
        },
        ApplyEvent::Unreadable { run, why } => EventKindOut::UnreadableRun {
            run: run.as_str().to_owned(),
            part: match why {
                Unreadable::Plan(_) => UnreadablePart::Plan,
                Unreadable::Journal(_) => UnreadablePart::Journal,
                Unreadable::Order => UnreadablePart::Order,
                Unreadable::Store(_) => UnreadablePart::Files,
            },
        },
        ApplyEvent::RunStarted { run, items } => EventKindOut::RunStarted {
            run: run.as_str().to_owned(),
            items: *items as u64,
        },
        ApplyEvent::Item { run, event } => match event {
            ItemEvent::Started { item } => EventKindOut::ItemStarted {
                run: run.as_str().to_owned(),
                item: item.0,
            },
            ItemEvent::Ended { item, outcome } => EventKindOut::ItemEnded {
                run: run.as_str().to_owned(),
                item: item.0,
                outcome: OutcomeOut::of(outcome, codec),
            },
        },
        ApplyEvent::RestoreStarted { run, item } => EventKindOut::RestoreStarted {
            run: run.as_str().to_owned(),
            item: item.0,
        },
        ApplyEvent::RestoreEnded { run, item, result } => EventKindOut::RestoreEnded {
            run: run.as_str().to_owned(),
            item: item.0,
            result: RestoreOut::of(result, codec),
        },
    }
}
