//! Runs as stored: a plan and its records, read back (spec M3b pinned 3, 6, 8, 9, 12).
//!
//! A [`StoredRun`] is never a [`Plan`]: it authorizes nothing. Reconciliation, `history`
//! and `restore` read it; apply never takes one.
//!
//! ```compile_fail,E0308
//! // A stored run can't be applied: apply takes a plan the planner made.
//! fn rerun(stored: &kipple_core::StoredRun) -> kipple_core::ApplyRequest<'_> {
//!     kipple_core::ApplyRequest { plan: stored, authorization: kipple_core::Authorization::SafeOnly }
//! }
//! ```
//!
//! Its journal is read up to its last sound record. A last line that is torn, fails to
//! parse or fails its digest is what a crash leaves mid-write (a long record's last page
//! can reach the disk before an earlier one), so it is ignored, and the next append
//! starts where it began. Only a bad line followed by a good one makes the run
//! unreadable: nothing in it counts as removed, and it never blocks a new apply.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::digest::Digest;
use crate::engine::FindingId;
use crate::format::{FormatError, decode_line, decode_stored_plan};
use crate::frozen::{FrozenTree, NameCodec};
use crate::journal::{Intent, Record, RestoreResult};
use crate::mutation::LocalFileSystem;
use crate::outcome::{ApplyError, Found, ItemOutcome, Receipt, ReceiptItem, Skip, Stop};
#[cfg(doc)]
use crate::plan::Plan;
use crate::plan::{Anchor, ItemId, Judgement, Standing};
use crate::rule::Method;
use crate::size::SpaceEstimate;
use crate::store::{RunId, RunStore};
use crate::trash::TrashSlot;

/// A run's plan as stored. Never a [`Plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPlan {
    pub(crate) digest: Digest,
    pub(crate) items: Vec<StoredItem>,
}

impl StoredPlan {
    /// The plan's digest as it was written, which its intents name.
    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }

    /// Its items, in plan order.
    #[must_use]
    pub fn items(&self) -> &[StoredItem] {
        &self.items
    }

    /// Its item `item`.
    #[must_use]
    pub fn item(&self, item: ItemId) -> Option<&StoredItem> {
        self.items
            .get(item.0 as usize)
            .filter(|stored| stored.item == item)
    }
}

/// One item of a stored plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredItem {
    pub(crate) item: ItemId,
    pub(crate) finding: FindingId,
    pub(crate) path: PathBuf,
    pub(crate) anchor: Anchor,
    pub(crate) method: Method,
    pub(crate) standing: Standing,
    pub(crate) judgement: Judgement,
    pub(crate) estimate: Option<SpaceEstimate>,
    pub(crate) file_system: LocalFileSystem,
    pub(crate) tree_digest: Digest,
    pub(crate) entries: u64,
    pub(crate) tree: Option<Arc<FrozenTree>>,
}

impl StoredItem {
    /// Its place in the plan.
    #[must_use]
    pub const fn item(&self) -> ItemId {
        self.item
    }

    /// The finding's path, as it was shown.
    #[must_use]
    pub const fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Where its target was.
    #[must_use]
    pub const fn anchor(&self) -> &Anchor {
        &self.anchor
    }

    /// How it was to be removed.
    #[must_use]
    pub const fn method(&self) -> Method {
        self.method
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

    /// Its stored tree's digest, which its intent names.
    #[must_use]
    pub const fn tree_digest(&self) -> Digest {
        self.tree_digest
    }

    /// Its frozen tree, when the run was read with its trees.
    #[must_use]
    pub fn tree(&self) -> Option<&FrozenTree> {
        self.tree.as_deref()
    }
}

/// How much of a run to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// The plan's headers and the journal: what `history` needs.
    Headers,
    /// The trees too: what reconciliation and restore need.
    Trees,
}

/// Why a run can't be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unreadable {
    /// Its stored plan isn't a valid stored plan.
    #[error("its plan: {0}")]
    Plan(FormatError),
    /// A journal line that doesn't hold together is followed by one that does.
    #[error("its journal is corrupt at byte {0}")]
    Journal(u64),
    /// Its records don't follow the journal's order, or name another plan.
    #[error("its records are out of order")]
    Order,
    /// Its files couldn't be read.
    #[error("its files can't be read")]
    Store(ApplyError),
}

/// What reading one run found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunRead {
    /// It has no plan: it was created and never started. Harmless.
    NeverStarted(RunId),
    /// It can't be read: nothing in it counts as removed.
    Unreadable(RunId, Unreadable),
    /// It was read.
    Read(StoredRun),
}

/// A run as stored: its plan and its sound records. Read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRun {
    pub(crate) id: RunId,
    pub(crate) plan: StoredPlan,
    pub(crate) records: Vec<Record>,
    /// Where the sound records end: where the next append goes.
    pub(crate) sound: u64,
    /// Whether a torn or untrusted tail follows them.
    pub(crate) torn: bool,
}

/// A restore begun without an outcome: running, or a crash interrupted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingRestore<'run> {
    /// The item restored.
    pub item: ItemId,
    /// The trash slot it was restored from.
    pub slot: &'run TrashSlot,
}

/// Where one item of a stored run stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemState<'run> {
    /// Nothing was recorded for it: the apply stopped before it.
    NotReached,
    /// Its intent is recorded and its outcome isn't: running, or interrupted.
    Pending(&'run Intent),
    /// It ended.
    Ended(&'run ItemOutcome),
}

impl StoredRun {
    /// Its ID.
    #[must_use]
    pub const fn id(&self) -> &RunId {
        &self.id
    }

    /// Its plan.
    #[must_use]
    pub const fn plan(&self) -> &StoredPlan {
        &self.plan
    }

    /// Its sound records, in order.
    #[must_use]
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Whether a torn or untrusted last line follows its records.
    #[must_use]
    pub const fn torn(&self) -> bool {
        self.torn
    }

    /// Where item `item` stands.
    #[must_use]
    pub fn state(&self, item: ItemId) -> ItemState<'_> {
        let mut state = ItemState::NotReached;
        for record in &self.records {
            if let Record::Intent(intent) = record
                && intent.item == item
            {
                state = ItemState::Pending(intent);
            } else if let Record::Outcome { item: of, outcome } = record
                && *of == item
            {
                state = ItemState::Ended(outcome);
            }
        }
        state
    }

    /// The intents without an outcome, in the order they were recorded.
    #[must_use]
    pub fn pending(&self) -> Vec<&Intent> {
        self.plan
            .items
            .iter()
            .filter_map(|item| match self.state(item.item) {
                ItemState::Pending(intent) => Some(intent),
                ItemState::NotReached | ItemState::Ended(_) => None,
            })
            .collect()
    }

    /// The restores begun without an outcome, by item.
    #[must_use]
    pub fn pending_restores(&self) -> Vec<PendingRestore<'_>> {
        let mut open: BTreeMap<ItemId, &TrashSlot> = BTreeMap::new();
        for record in &self.records {
            if let Record::RestoreIntent { item, slot } = record {
                open.insert(*item, slot);
            } else if let Record::RestoreOutcome { item, .. } = record {
                open.remove(item);
            }
        }
        open.into_iter()
            .map(|(item, slot)| PendingRestore { item, slot })
            .collect()
    }

    /// The last restore of `item` that ended, if any.
    #[must_use]
    pub fn restored(&self, item: ItemId) -> Option<&RestoreResult> {
        self.records.iter().rev().find_map(|record| {
            if let Record::RestoreOutcome { item: of, result } = record
                && *of == item
            {
                Some(result)
            } else {
                None
            }
        })
    }

    /// The receipt the records give: each item's recorded outcome. An item with an intent
    /// and no outcome is `indeterminate` until reconciled, one with nothing recorded was
    /// never reached, and a restored item says so. It never claims more than was
    /// recorded.
    #[must_use]
    pub fn receipt(&self) -> Receipt {
        let items = self
            .plan
            .items
            .iter()
            .map(|item| {
                let outcome = match self.state(item.item) {
                    ItemState::NotReached => {
                        ItemOutcome::Skipped(Skip::NotStarted(Stop::Interrupted))
                    }
                    ItemState::Pending(_) => ItemOutcome::Indeterminate(Found::Pending),
                    ItemState::Ended(outcome) => outcome.clone(),
                };
                ReceiptItem {
                    item: item.item,
                    path: item.path.clone(),
                    method: item.method,
                    estimate: item.estimate,
                    outcome,
                    restored: self.restored(item.item).cloned(),
                }
            })
            .collect();
        Receipt { items }
    }
}

/// Reads run `id` from `store`, to `depth`.
#[must_use]
pub fn read_run(store: &dyn RunStore, id: &RunId, depth: Depth, codec: &dyn NameCodec) -> RunRead {
    match read(store, id, depth, codec) {
        Ok(Some(run)) => RunRead::Read(run),
        Ok(None) => RunRead::NeverStarted(id.clone()),
        Err(why) => RunRead::Unreadable(id.clone(), why),
    }
}

fn read(
    store: &dyn RunStore,
    id: &RunId,
    depth: Depth,
    codec: &dyn NameCodec,
) -> Result<Option<StoredRun>, Unreadable> {
    let bytes = match depth {
        Depth::Headers => store.header(id),
        Depth::Trees => store.plan(id),
    }
    .map_err(Unreadable::Store)?;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    // The header is the first line; the trees follow it.
    let end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or(Unreadable::Plan(FormatError::Digest(
            crate::format::STORED_PLAN,
        )))?;
    let (header, rest) = bytes.split_at(end);
    let trees = (depth == Depth::Trees).then(|| rest.get(1..).unwrap_or_default());
    let plan = decode_stored_plan(header, trees, codec).map_err(Unreadable::Plan)?;
    let journal = store.journal(id).map_err(Unreadable::Store)?;
    let (records, sound) = sound_records(&journal, codec)?;
    let run = StoredRun {
        id: id.clone(),
        plan,
        records,
        sound,
        torn: sound < journal.len() as u64,
    };
    check_order(&run)?;
    Ok(Some(run))
}

/// The records of `journal` up to its last sound line, and where they end.
///
/// A bad line (torn, unparsable or failing its digest) with nothing sound after it is a
/// tail a crash left, and is ignored; a bad line with a sound one after it makes the
/// journal unreadable.
fn sound_records(journal: &[u8], codec: &dyn NameCodec) -> Result<(Vec<Record>, u64), Unreadable> {
    let mut records = Vec::new();
    let mut at = 0;
    let mut bad: Option<u64> = None;
    for piece in journal.split_inclusive(|byte| *byte == b'\n') {
        let start = at;
        at += piece.len() as u64;
        let record = piece
            .strip_suffix(b"\n")
            .and_then(|line| decode_line(line, codec).ok());
        match (record, bad) {
            (Some(_), Some(first)) => return Err(Unreadable::Journal(first)),
            (Some(record), None) => records.push(record),
            (None, None) => bad = Some(start),
            (None, Some(_)) => {}
        }
    }
    Ok((records, bad.unwrap_or(at)))
}

/// Fails unless `run`'s records follow the journal's order: the plan record first, naming
/// the stored plan; at most one intent and one outcome per item, the outcome last; every
/// intent naming the stored plan; restores only of items that ended, each restore's
/// outcome after its intent.
fn check_order(run: &StoredRun) -> Result<(), Unreadable> {
    let mut records = run.records.iter();
    match records.next() {
        None => return Ok(()),
        Some(Record::Plan { digest }) if *digest == run.plan.digest => {}
        Some(_) => return Err(Unreadable::Order),
    }
    let count = run.plan.items.len();
    let mut intents = vec![false; count];
    let mut ended = vec![false; count];
    let mut restoring = vec![false; count];
    for record in records {
        let ok = match record {
            Record::Plan { .. } => false,
            Record::Intent(intent) => {
                intent.plan == run.plan.digest
                    && flip(&mut intents, intent.item)
                    && !seen(&ended, intent.item)
            }
            Record::Outcome { item, .. } => flip(&mut ended, *item),
            Record::RestoreIntent { item, .. } => {
                seen(&ended, *item) && flip(&mut restoring, *item)
            }
            Record::RestoreOutcome { item, .. } => unflip(&mut restoring, *item),
            Record::TrashRecordRemoved { item, .. } => {
                seen(&intents, *item) && !seen(&ended, *item)
            }
        };
        if !ok {
            return Err(Unreadable::Order);
        }
    }
    Ok(())
}

/// Sets `item`'s flag, failing for an unknown item or one already set.
fn flip(flags: &mut [bool], item: ItemId) -> bool {
    flags
        .get_mut(item.0 as usize)
        .is_some_and(|flag| !std::mem::replace(flag, true))
}

/// Clears `item`'s flag, failing for an unknown item or one not set.
fn unflip(flags: &mut [bool], item: ItemId) -> bool {
    flags
        .get_mut(item.0 as usize)
        .is_some_and(|flag| std::mem::replace(flag, false))
}

fn seen(flags: &[bool], item: ItemId) -> bool {
    flags.get(item.0 as usize).copied().unwrap_or(false)
}

/// What one run of `history` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    /// The run.
    pub id: RunId,
    /// Where it stands.
    pub state: RunState,
    /// Its receipt from what was recorded; empty unless it was read.
    pub receipt: Receipt,
}

named! {
    /// Where a run stands.
    RunState {
        /// Every item ended.
        Complete = "complete",
        /// Some items have an intent and no outcome, and another kipple holds the lock: it
        /// is running now.
        Running = "running",
        /// Some items have no outcome and it isn't running: a crash or a failed record
        /// stopped it. The next mutating command reconciles the items it had started; the
        /// others never ran, and it stays interrupted.
        Interrupted = "interrupted",
        /// It was created and never started.
        NeverStarted = "never-started",
        /// It can't be read: nothing in it counts as removed.
        Unreadable = "unreadable",
    }
}

/// Every run in `store`, oldest first, read to their headers. A run with pending intents
/// is `running` while another kipple holds the lock (`held`), else `interrupted`.
///
/// # Errors
/// When the runs can't be listed.
pub fn history(
    store: &dyn RunStore,
    held: crate::store::LockState,
    codec: &dyn NameCodec,
) -> Result<Vec<RunSummary>, ApplyError> {
    let runs = store.runs()?;
    Ok(runs
        .iter()
        .map(|id| match read_run(store, id, Depth::Headers, codec) {
            RunRead::NeverStarted(id) => RunSummary {
                id,
                state: RunState::NeverStarted,
                receipt: Receipt::default(),
            },
            RunRead::Unreadable(id, _) => RunSummary {
                id,
                state: RunState::Unreadable,
                receipt: Receipt::default(),
            },
            RunRead::Read(run) => {
                let unreached = run
                    .plan
                    .items
                    .iter()
                    .any(|item| run.state(item.item) == ItemState::NotReached);
                let pending = !run.pending().is_empty() || !run.pending_restores().is_empty();
                let state = match (pending, unreached, held) {
                    (false, false, _) => RunState::Complete,
                    (true, _, crate::store::LockState::Held) => RunState::Running,
                    (true, _, crate::store::LockState::Free) | (false, true, _) => {
                        RunState::Interrupted
                    }
                };
                RunSummary {
                    receipt: run.receipt(),
                    id: run.id,
                    state,
                }
            }
        })
        .collect())
}
