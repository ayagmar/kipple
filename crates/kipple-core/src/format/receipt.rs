//! `kipple.receipt/v1` and `kipple.history/v1`: what `clean`, `apply` and `history` print
//! with `--json`. Written, never read back: a run's receipt is always computed again from
//! its stored plan and journal.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;

use super::journal::RestoreOut;
use super::outcome::{EstimateOut, OutcomeOut, RemovedOut};
use super::{HISTORY, RECEIPT, TextOut, path_out};
use crate::frozen::NameCodec;
use crate::outcome::{Receipt, ReceiptItem, Status};
use crate::rule::Method;
use crate::run::{RunState, RunSummary};
use crate::session::Ending;
use crate::store::RunId;

/// What a run did, item by item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReceiptOut {
    /// `kipple.receipt/v1`.
    pub schema: String,
    /// The run, when one was stored.
    pub run: Option<String>,
    /// How the command ended.
    pub ending: EndingOut,
    /// Its exit code.
    pub exit: u8,
    /// Every item of the plan, in plan order.
    pub items: Vec<ReceiptItemOut>,
    /// The totals.
    pub totals: TotalsOut,
}

/// How a command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EndingOut {
    /// Something was done, and nothing failed.
    Done,
    /// The plan was empty.
    NothingToClean,
    /// Items were planned and none ran.
    NoneRan,
    /// The person said no.
    Declined,
    /// Every item of a plan file dropped.
    Stale,
    /// Something failed or can't be told.
    Failed,
    /// The store refused.
    Refused,
    /// It was cancelled.
    Cancelled,
}

impl EndingOut {
    const fn of(ending: &Ending) -> Self {
        match ending {
            Ending::Done => Self::Done,
            Ending::NothingToClean => Self::NothingToClean,
            Ending::NoneRan => Self::NoneRan,
            Ending::Declined => Self::Declined,
            Ending::Stale => Self::Stale,
            Ending::Failed => Self::Failed,
            Ending::Refused(_) => Self::Refused,
            Ending::Cancelled => Self::Cancelled,
        }
    }
}

/// One item of a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReceiptItemOut {
    /// Its place in the plan, from 0.
    pub item: u32,
    /// The finding's path, as shown.
    pub path: TextOut,
    /// How it was to be removed.
    pub method: Method,
    /// What the scan estimated removing it frees.
    pub estimate: Option<EstimateOut>,
    /// What happened.
    pub outcome: OutcomeOut,
    /// How its last restore ended, if it was restored.
    pub restored: Option<RestoreOut>,
}

/// A receipt's totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TotalsOut {
    /// What every item removed, as each run reported it.
    pub removed: RemovedOut,
    /// Items moved to the trash.
    pub trashed_items: u64,
    /// Their apparent size, as the scan estimated it.
    pub trashed_apparent_bytes: u64,
    /// Their allocated size, where every one of them had one.
    pub trashed_allocated_bytes: Option<u64>,
    /// How many items ended in each status.
    pub statuses: BTreeMap<Status, u64>,
}

/// Every run kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct HistoryOut {
    /// `kipple.history/v1`.
    pub schema: String,
    /// Every run, oldest first.
    pub runs: Vec<RunOut>,
}

/// One run of the history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RunOut {
    /// Its ID.
    pub id: String,
    /// Where it stands.
    pub state: RunState,
    /// Its items, as recorded; none when it never started or can't be read.
    pub items: Vec<ReceiptItemOut>,
    /// Their totals.
    pub totals: TotalsOut,
}

impl ReceiptOut {
    /// `receipt`, of `run` when one was stored, which ended as `ending`.
    #[must_use]
    pub fn of(
        receipt: &Receipt,
        run: Option<&RunId>,
        ending: &Ending,
        codec: &dyn NameCodec,
    ) -> Self {
        Self {
            schema: RECEIPT.to_owned(),
            run: run.map(|run| run.as_str().to_owned()),
            ending: EndingOut::of(ending),
            exit: ending.code(),
            items: items(receipt, codec),
            totals: totals(receipt),
        }
    }
}

impl HistoryOut {
    /// `runs`, as [`history`](crate::history) read them.
    #[must_use]
    pub fn of(runs: &[RunSummary], codec: &dyn NameCodec) -> Self {
        Self {
            schema: HISTORY.to_owned(),
            runs: runs
                .iter()
                .map(|run| RunOut {
                    id: run.id.as_str().to_owned(),
                    state: run.state,
                    items: items(&run.receipt, codec),
                    totals: totals(&run.receipt),
                })
                .collect(),
        }
    }
}

fn items(receipt: &Receipt, codec: &dyn NameCodec) -> Vec<ReceiptItemOut> {
    receipt
        .items
        .iter()
        .map(|item| item_out(item, codec))
        .collect()
}

fn item_out(item: &ReceiptItem, codec: &dyn NameCodec) -> ReceiptItemOut {
    ReceiptItemOut {
        item: item.item.0,
        path: path_out(&item.path, codec),
        method: item.method,
        estimate: item.estimate.as_ref().map(EstimateOut::of),
        outcome: OutcomeOut::of(&item.outcome, codec),
        restored: item
            .restored
            .as_ref()
            .map(|result| RestoreOut::of(result, codec)),
    }
}

fn totals(receipt: &Receipt) -> TotalsOut {
    let totals = receipt.totals();
    TotalsOut {
        removed: RemovedOut::of(&totals.removed),
        trashed_items: totals.trashed.items,
        trashed_apparent_bytes: totals.trashed.apparent.0,
        trashed_allocated_bytes: totals.trashed.allocated.map(|bytes| bytes.0),
        statuses: totals.items,
    }
}
