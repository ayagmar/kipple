//! A machine in memory for the apply loop's tests. Include with
//! `#[path = "support/world.rs"] mod world;` and `#[path = "support/codec.rs"] mod codec;`.
//!
//! Its targets are what the freezer reads, the executor deletes from and the trash moves
//! whole, so what one port changes the next one sees, as on a real disk. Its journal keeps
//! the durable records, which the executor and the trash check for the item's intent
//! before changing anything. It is also a run store, keeping each run's bytes as the
//! platform's would, torn tails included. A crash is an unwind out of the journal, the
//! store or the executor at a chosen point, after which the state is what a disk would
//! hold.
#![expect(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    reason = "a fixture that doesn't build fails the test"
)]
#![expect(
    clippy::significant_drop_tightening,
    reason = "each port call holds the state for its whole step, as one disk operation"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

pub(crate) use kipple_core::Record;
use kipple_core::format::{decode_line, encode_line, encode_stored_plan};
use kipple_core::{
    Activity, Allocated, Anchor, ApplyError, CancelToken, Component, DeleteOp, EffectSink,
    EntryKind, EntrySnapshot, ErrorCode, Executor, FileIdentity, FileSystemClass, Finding, Finish,
    Forgotten, Freed, Freeze, Freezer, FrozenTree, GitGap, Intent, ItemId, ItemOutcome, Journal,
    LeftReason, LocalFileSystem, LockState, Locked, NodeId, Plan, RelPath, Rescan, RestoreRequest,
    RunId, RunStore, ScanReport, Scope, SlotState, StoreRefusal, TrackedQuery, TrashBackend,
    TrashDir, TrashOp, TrashSlot, Trashed, TreeBuilder, Volume,
};

use crate::codec::TestCodec;

/// The one volume every target is on.
pub(crate) const VOLUME: Volume = Volume {
    device: 7,
    mount: Some(1),
};

/// One target on the disk: its top (at the empty path) and everything below it.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(crate) scope: Scope,
    pub(crate) parent: Option<FileIdentity>,
    pub(crate) file_system: FileSystemClass,
    pub(crate) entries: BTreeMap<RelPath, EntrySnapshot>,
    /// A directory below can't be listed, so freezing fails.
    pub(crate) unreadable: bool,
    /// Entries the OS refuses to remove.
    pub(crate) denied: BTreeSet<RelPath>,
    /// The run fails on reaching this entry, changing nothing more.
    pub(crate) fails_at: Option<RelPath>,
    /// The executor and the trash fail with this code before removing anything: a
    /// refusal, as when the way to it changed after the freeze, or a trash move that
    /// stranded something swapped in for it.
    pub(crate) refusal: Option<ErrorCode>,
}

impl Target {
    /// A target on a supported file system.
    pub(crate) const fn new(
        scope: Scope,
        parent: Option<FileIdentity>,
        entries: BTreeMap<RelPath, EntrySnapshot>,
    ) -> Self {
        Self {
            scope,
            parent,
            file_system: FileSystemClass::Supported(LocalFileSystem::Ext4),
            entries,
            unreadable: false,
            denied: BTreeSet::new(),
            fails_at: None,
            refusal: None,
        }
    }
}

/// One entry the executor removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Removal {
    pub(crate) item: ItemId,
    pub(crate) target: PathBuf,
    pub(crate) path: RelPath,
}

/// Where the run unwinds, as a crash would stop it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Crash {
    /// Just after the journal write numbered this (from 0) became durable.
    AfterWrite(usize),
    /// Just before the journal write numbered this (from 0), which never lands.
    BeforeWrite(usize),
    /// Just after this many removals.
    AfterRemovals(usize),
    /// The store's write numbered this (from 0) lands only its first this many bytes,
    /// then the process dies: a torn tail.
    Torn {
        /// The write.
        write: usize,
        /// How many of its bytes land.
        bytes: usize,
    },
}

/// One run's files in the store.
#[derive(Debug, Clone, Default)]
pub(crate) struct Files {
    pub(crate) plan: Option<Vec<u8>>,
    pub(crate) journal: Vec<u8>,
}

/// What unwinds out of a crash.
#[derive(Debug)]
pub(crate) struct Crashed;

/// The disk, the trash and the journal's durable records.
#[derive(Debug, Default)]
pub(crate) struct State {
    pub(crate) targets: BTreeMap<PathBuf, Target>,
    pub(crate) trash: BTreeMap<Component, Target>,
    pub(crate) removals: Vec<Removal>,
    /// Items whose operation ran, each time it ran.
    pub(crate) ran: Vec<ItemId>,
    /// Items whose operation ran with no durable intent.
    pub(crate) unjournalled: Vec<ItemId>,
    pub(crate) records: Vec<Record>,
    /// How many freezes were asked for.
    pub(crate) freezes: usize,
    pub(crate) crash: Option<Crash>,
    /// The journal fails this write (from 0), and with `journal_recovers` unset every one
    /// after.
    pub(crate) journal_fails_at: Option<usize>,
    /// The journal fails only the one write, as a store with a passing error does.
    pub(crate) journal_recovers: bool,
    /// The first write that failed.
    pub(crate) failed_write: Option<usize>,
    pub(crate) writes: usize,
    /// The run is cancelled after this many removals.
    pub(crate) cancel_after: Option<usize>,
    pub(crate) trash_unsupported: bool,
    /// The trash records written for slots, by slot name: the `.trashinfo` a move writes
    /// before it renames.
    pub(crate) trash_records: BTreeSet<Component>,
    /// Records reconciliation removed.
    pub(crate) forgotten: Vec<Component>,
    /// What git tracks in each target, by its path, or why it can't say. A target with
    /// none isn't in a repository.
    pub(crate) git: BTreeMap<PathBuf, Result<Vec<RelPath>, GitGap>>,
    /// The store's runs.
    pub(crate) runs: BTreeMap<RunId, Files>,
    /// Another kipple holds the store's lock.
    pub(crate) busy: bool,
}

/// The machine. It is every mutation port and, through [`World::journal`], the journal.
#[derive(Debug, Default)]
pub(crate) struct World {
    state: Mutex<State>,
}

impl World {
    /// The state, even after a crash unwound while it was held.
    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A journal writing to this world's records.
    pub(crate) const fn recorder(&self) -> WorldJournal<'_> {
        WorldJournal(self)
    }

    /// Records that `item`'s operation started, and whether its intent was durable.
    fn started(&self, item: ItemId) {
        let mut state = self.state();
        state.ran.push(item);
        let intended = state
            .records
            .iter()
            .any(|record| matches!(record, Record::Intent(intent) if intent.item == item));
        if !intended {
            state.unjournalled.push(item);
        }
    }
}

/// Unwinds as a crash, once the state is released.
fn crash(state: MutexGuard<'_, State>) -> ! {
    drop(state);
    std::panic::resume_unwind(Box::new(Crashed))
}

impl Freezer for World {
    fn freeze(&self, anchor: &Anchor, cancel: &CancelToken) -> Result<Freeze, ApplyError> {
        let target = {
            let mut state = self.state();
            state.freezes += 1;
            state.targets.get(&anchor.path()).cloned()
        };
        if cancel.is_cancelled() {
            return Err(ApplyError::new(ErrorCode::Cancelled));
        }
        let target = target.ok_or_else(|| ApplyError::new(ErrorCode::TargetMissing))?;
        if target.unreadable {
            return Err(ApplyError::new(ErrorCode::Unreadable));
        }
        if anchor.parent.is_some() && anchor.parent != target.parent {
            return Err(ApplyError::new(ErrorCode::ParentChanged));
        }
        Ok(Freeze {
            tree: tree_of(&target),
            file_system: target.file_system,
            parent: target.parent,
        })
    }
}

/// `target`, frozen.
pub(crate) fn tree_of(target: &Target) -> FrozenTree {
    let top = target.entries.get(&RelPath::default()).unwrap();
    let mut builder = TreeBuilder::new(target.scope, VOLUME, top).unwrap();
    let mut slots = BTreeMap::new();
    slots.insert(RelPath::default(), builder.top());
    for (path, entry) in target.entries.iter().skip(1) {
        let (name, parent) = path.components().split_last().unwrap();
        let parent = slots[&RelPath::new(parent.to_vec())];
        let slot = builder.add(parent, name.clone(), entry).unwrap();
        slots.insert(path.clone(), slot);
    }
    builder.finish().unwrap()
}

impl Executor for World {
    fn delete(
        &self,
        op: &DeleteOp,
        effects: &mut dyn EffectSink,
        cancel: &CancelToken,
    ) -> Result<Finish, ApplyError> {
        let target = op.target();
        self.started(target.item());
        self.refusal(target)?;
        // Every entry before the directory holding it.
        let entries: Vec<_> = target.tree().entries().collect();
        for entry in entries.into_iter().rev() {
            if !target.may_remove(entry.id()) {
                continue;
            }
            if self.state().cancels() {
                cancel.cancel();
            }
            if cancel.is_cancelled() {
                return Ok(Finish::Cancelled);
            }
            let frozen = entry.snapshot().unwrap();
            match self.remove(target, &entry.path(), &frozen)? {
                Err(reason) => effects.left(entry.id(), reason),
                Ok(()) => effects.removed(entry.id(), freed(&frozen)),
            }
        }
        Ok(Finish::Completed)
    }
}

impl State {
    /// Whether the user cancels now.
    fn cancels(&self) -> bool {
        self.cancel_after == Some(self.removals.len())
    }
}

impl World {
    /// The error `target`'s item fails with before anything is removed, if any.
    fn refusal(&self, target: &kipple_core::Authorized) -> Result<(), ApplyError> {
        let refusal = self
            .state()
            .targets
            .get(&target.anchor().path())
            .and_then(|disk| disk.refusal);
        refusal.map_or(Ok(()), |code| Err(ApplyError::new(code)))
    }

    /// Removes the entry frozen as `frozen` at `path` of `target`'s item, or says why it
    /// stays. Crashes after it when a crash is due.
    fn remove(
        &self,
        target: &kipple_core::Authorized,
        path: &RelPath,
        frozen: &EntrySnapshot,
    ) -> Result<Result<(), LeftReason>, ApplyError> {
        let at = target.anchor().path();
        let mut state = self.state();
        let disk = state
            .targets
            .get_mut(&at)
            .ok_or_else(|| ApplyError::new(ErrorCode::TargetMissing))?;
        if disk.fails_at.as_ref() == Some(path) {
            return Err(ApplyError::new(ErrorCode::Io).at(path.clone()));
        }
        if let Err(reason) = removal(disk, path, frozen) {
            return Ok(Err(reason));
        }
        disk.entries.remove(path);
        if disk.entries.is_empty() {
            state.targets.remove(&at);
        }
        state.removals.push(Removal {
            item: target.item(),
            target: at,
            path: path.clone(),
        });
        if state.crash == Some(Crash::AfterRemovals(state.removals.len())) {
            crash(state);
        }
        Ok(Ok(()))
    }
}

/// Whether the entry frozen as `frozen` at `path` may be removed now, as the executor
/// checks it on its handle.
fn removal(disk: &Target, path: &RelPath, frozen: &EntrySnapshot) -> Result<(), LeftReason> {
    let now = disk.entries.get(path).ok_or(LeftReason::Gone)?;
    if let Some(drift) = frozen.drift(now) {
        return Err(match drift {
            kipple_core::Drift::Replaced => LeftReason::Replaced,
            kipple_core::Drift::Changed => LeftReason::Changed,
        });
    }
    if disk.denied.contains(path) {
        return Err(LeftReason::Denied);
    }
    let depth = path.components().len();
    let holds = disk.entries.keys().any(|other| {
        other.components().len() > depth && other.components().starts_with(path.components())
    });
    if frozen.kind == EntryKind::Dir && holds {
        return Err(LeftReason::NotEmpty);
    }
    Ok(())
}

fn freed(snapshot: &EntrySnapshot) -> Freed {
    if snapshot.links > 1 && snapshot.kind != EntryKind::Dir {
        Freed::Shared
    } else {
        Freed::Allocated(Allocated(snapshot.fingerprint.size))
    }
}

/// The world's trash directory.
pub(crate) fn trash_dir() -> TrashDir {
    TrashDir {
        path: PathBuf::from("/home/u/.local/share/Trash/files"),
        identity: FileIdentity {
            volume: 7,
            file: 900,
        },
    }
}

impl TrashBackend for World {
    fn available(&self, _: &Anchor) -> Result<(), ApplyError> {
        self.available_now()
    }

    fn destination(&self, op: &TrashOp) -> Result<TrashSlot, ApplyError> {
        self.available(op.target().anchor())?;
        Ok(TrashSlot {
            trash: trash_dir(),
            name: Component::new(format!("item-{}", op.target().item().0)).unwrap(),
        })
    }

    fn move_whole(
        &self,
        op: &TrashOp,
        slot: &TrashSlot,
        cancel: &CancelToken,
    ) -> Result<Trashed, ApplyError> {
        let target = op.target();
        self.started(target.item());
        if cancel.is_cancelled() {
            return Err(ApplyError::new(ErrorCode::Cancelled));
        }
        self.refusal(target)?;
        let at = target.anchor().path();
        let mut state = self.state();
        let disk = state
            .targets
            .get(&at)
            .ok_or_else(|| ApplyError::new(ErrorCode::TargetMissing))?;
        let differs = target.tree().entries().any(|entry| {
            let now = disk.entries.get(&entry.path());
            now.zip(entry.snapshot())
                .is_none_or(|(now, then)| then.drift(now).is_some())
        });
        if differs {
            return Err(ApplyError::new(ErrorCode::TargetChanged));
        }
        state.trash_records.insert(slot.name.clone());
        let moved = state.targets.remove(&at).unwrap();
        state.trash.insert(slot.name.clone(), moved);
        Ok(Trashed { slot: slot.clone() })
    }

    fn find(&self, slot: &TrashSlot, target: &EntrySnapshot) -> Result<SlotState, ApplyError> {
        let state = self.state();
        let top = state
            .trash
            .get(&slot.name)
            .and_then(|moved| moved.entries.get(&RelPath::default()));
        Ok(match top {
            None => SlotState::Empty,
            Some(top) if top.identity == target.identity => SlotState::Holds,
            Some(_) => SlotState::Other,
        })
    }

    fn restore(&self, request: &RestoreRequest, cancel: &CancelToken) -> Result<(), ApplyError> {
        if cancel.is_cancelled() {
            return Err(ApplyError::new(ErrorCode::Cancelled));
        }
        let mut state = self.state();
        let name = &request.slot().name;
        let top = state
            .trash
            .get(name)
            .and_then(|moved| moved.entries.get(&RelPath::default()))
            .ok_or_else(|| ApplyError::new(ErrorCode::TrashMissing))?;
        if top.identity != request.target().identity {
            return Err(ApplyError::new(ErrorCode::TrashChanged));
        }
        let original = request.anchor().path();
        if state.targets.contains_key(&original) {
            return Err(ApplyError::new(ErrorCode::DestinationExists));
        }
        let moved = state.trash.remove(name).unwrap();
        state.trash_records.remove(name);
        state.targets.insert(original, moved);
        Ok(())
    }

    fn prepare_home(&self) -> Result<(), ApplyError> {
        self.available_now()
    }

    fn forget_unmoved(&self, slot: &TrashSlot, _: &Anchor) -> Result<Forgotten, ApplyError> {
        let mut state = self.state();
        if state.trash_records.remove(&slot.name) {
            state.forgotten.push(slot.name.clone());
            Ok(Forgotten::Removed)
        } else {
            Ok(Forgotten::Absent)
        }
    }
}

impl World {
    fn available_now(&self) -> Result<(), ApplyError> {
        if self.state().trash_unsupported {
            return Err(ApplyError::new(ErrorCode::Unsupported));
        }
        Ok(())
    }
}

impl TrackedQuery for World {
    fn count(&self, _: &Path, item: &Path, _: &CancelToken) -> Result<u64, GitGap> {
        match self.state().git.get(item) {
            Some(Ok(tracked)) => Ok(tracked.len() as u64),
            Some(Err(gap)) => Err(*gap),
            None => Err(GitGap::Unsupported),
        }
    }

    fn entries(
        &self,
        _: &Path,
        anchor: &Anchor,
        tree: &FrozenTree,
        _: &CancelToken,
    ) -> Result<Vec<NodeId>, GitGap> {
        match self.state().git.get(&anchor.path()) {
            Some(Ok(tracked)) => Ok(tracked.iter().filter_map(|path| tree.find(path)).collect()),
            Some(Err(gap)) => Err(*gap),
            None => Err(GitGap::Unsupported),
        }
    }
}

/// The scan apply runs, with the activity it judges each item by now: the scan's own
/// when `activity` is unset.
#[derive(Debug, Clone)]
pub(crate) struct Rescanned {
    pub(crate) scan: ScanReport,
    pub(crate) activity: Option<Activity>,
}

impl Rescan for Rescanned {
    fn scan(&self, _: &CancelToken) -> ScanReport {
        self.scan.clone()
    }

    fn activity(&self, _: &ScanReport, finding: &Finding) -> Activity {
        self.activity.clone().unwrap_or_else(|| {
            finding.facts.as_ref().map_or(
                Activity::Unknown(kipple_core::ActivityGap::Unsupported),
                |facts| facts.activity.clone(),
            )
        })
    }
}

impl World {
    /// Counts one store write, crashing or failing it as the state says. `Ok` with the
    /// bytes of `line` that land.
    fn store_write(&self, line: &[u8]) -> Result<usize, ApplyError> {
        let mut state = self.state();
        let attempt = state.writes;
        state.writes += 1;
        if state.crash == Some(Crash::BeforeWrite(attempt)) {
            crash(state);
        }
        let fails = match state.journal_fails_at {
            Some(at) if state.journal_recovers => attempt == at,
            Some(at) => attempt >= at,
            None => false,
        };
        if fails {
            state.failed_write.get_or_insert(attempt);
            return Err(ApplyError::new(ErrorCode::Journal));
        }
        Ok(match state.crash {
            Some(Crash::Torn { write, bytes }) if write == attempt => bytes.min(line.len()),
            _ => line.len(),
        })
    }

    /// Ends a store write: crashes when it was torn or a crash is due after it.
    fn store_wrote(&self, landed: usize, line: &[u8]) {
        let state = self.state();
        let attempt = state.writes - 1;
        if landed < line.len() || state.crash == Some(Crash::AfterWrite(attempt)) {
            crash(state);
        }
    }

    /// Stores `plan` with the journal records written so far as a run, as a store would
    /// have kept them, and returns its ID.
    pub(crate) fn store(&self, plan: &Plan) -> RunId {
        let mut state = self.state();
        let journal = state
            .records
            .iter()
            .flat_map(|record| encode_line(record, &TestCodec))
            .collect();
        let id = RunId::at(UNIX_EPOCH + Duration::from_secs(state.runs.len() as u64), 0);
        let files = Files {
            plan: Some(encode_stored_plan(plan, &TestCodec).unwrap()),
            journal,
        };
        state.runs.insert(id.clone(), files);
        id
    }
}

impl RunStore for World {
    fn lock(&self, _: &CancelToken) -> Result<Locked, StoreRefusal> {
        if self.state().busy {
            return Err(StoreRefusal::Busy);
        }
        Ok(Locked::new("world"))
    }

    fn lock_state(&self) -> Result<LockState, StoreRefusal> {
        Ok(if self.state().busy {
            LockState::Held
        } else {
            LockState::Free
        })
    }

    fn runs(&self) -> Result<Vec<RunId>, ApplyError> {
        Ok(self.state().runs.keys().cloned().collect())
    }

    fn header(&self, run: &RunId) -> Result<Option<Vec<u8>>, ApplyError> {
        let plan = self.plan(run)?;
        Ok(plan.map(|plan| {
            let end = plan
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(plan.len(), |at| at + 1);
            plan[..end].to_vec()
        }))
    }

    fn plan(&self, run: &RunId) -> Result<Option<Vec<u8>>, ApplyError> {
        Ok(self
            .state()
            .runs
            .get(run)
            .and_then(|files| files.plan.clone()))
    }

    fn journal(&self, run: &RunId) -> Result<Vec<u8>, ApplyError> {
        Ok(self
            .state()
            .runs
            .get(run)
            .map(|files| files.journal.clone())
            .unwrap_or_default())
    }

    fn create(&self, _: &Locked, plan: &[u8], first: &[u8]) -> Result<RunId, StoreRefusal> {
        let landed = self.store_write(first).map_err(StoreRefusal::Write)?;
        let mut state = self.state();
        let id = RunId::at(UNIX_EPOCH + Duration::from_secs(state.runs.len() as u64), 0);
        let files = Files {
            plan: Some(plan.to_vec()),
            journal: first[..landed].to_vec(),
        };
        state.runs.insert(id.clone(), files);
        if landed == first.len() {
            let record = decode_line(&first[..first.len() - 1], &TestCodec).unwrap();
            state.records.push(record);
        }
        drop(state);
        self.store_wrote(landed, first);
        Ok(id)
    }

    fn append(&self, _: &Locked, run: &RunId, at: u64, line: &[u8]) -> Result<u64, ApplyError> {
        let landed = self.store_write(line)?;
        let mut state = self.state();
        let files = state
            .runs
            .get_mut(run)
            .ok_or_else(|| ApplyError::new(ErrorCode::Journal))?;
        let at = usize::try_from(at).unwrap();
        if files.journal.len() < at {
            return Err(ApplyError::new(ErrorCode::Journal));
        }
        files.journal.truncate(at);
        files.journal.extend_from_slice(&line[..landed]);
        let length = files.journal.len() as u64;
        if landed == line.len() {
            let record = decode_line(&line[..line.len() - 1], &TestCodec).unwrap();
            state.records.push(record);
        }
        drop(state);
        self.store_wrote(landed, line);
        Ok(length)
    }
}

/// The world's journal.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WorldJournal<'world>(&'world World);

impl WorldJournal<'_> {
    fn write(self, record: Record) -> Result<(), ApplyError> {
        let mut state = self.0.state();
        let attempt = state.writes;
        state.writes += 1;
        if state.crash == Some(Crash::BeforeWrite(attempt)) {
            crash(state);
        }
        let fails = match state.journal_fails_at {
            Some(at) if state.journal_recovers => attempt == at,
            Some(at) => attempt >= at,
            None => false,
        };
        if fails {
            state.failed_write.get_or_insert(attempt);
            return Err(ApplyError::new(ErrorCode::Journal));
        }
        state.records.push(record);
        if state.crash == Some(Crash::AfterWrite(attempt)) {
            crash(state);
        }
        Ok(())
    }
}

impl Journal for WorldJournal<'_> {
    fn plan(&mut self, plan: &Plan) -> Result<(), ApplyError> {
        self.write(Record::Plan {
            digest: plan.digest(),
        })
    }

    fn record(&mut self, record: &Record) -> Result<(), ApplyError> {
        self.write(record.clone())
    }
}

impl State {
    /// Why the records break the journal's order, if they do: the plan first, then per
    /// item at most one intent and one outcome, an outcome without an intent only for a
    /// skip, and nothing after an item's outcome.
    pub(crate) fn order_problem(&self) -> Option<String> {
        let mut intents = BTreeSet::new();
        let mut outcomes = BTreeSet::new();
        for (index, record) in self.records.iter().enumerate() {
            let problem = match record {
                Record::Plan { .. } if index == 0 => None,
                Record::Plan { .. } => Some(format!("a second plan at {index}")),
                _ if index == 0 => Some("a record before the plan".to_owned()),
                Record::Intent(intent) if outcomes.contains(&intent.item) => {
                    Some(format!("item {} intended after it ended", intent.item.0))
                }
                Record::Intent(intent) if !intents.insert(intent.item) => {
                    Some(format!("item {} intended twice", intent.item.0))
                }
                Record::Outcome { item, outcome }
                    if !intents.contains(item) && !matches!(outcome, ItemOutcome::Skipped(_)) =>
                {
                    Some(format!("item {} ended with no intent", item.0))
                }
                Record::Outcome { item, .. } if !outcomes.insert(*item) => {
                    Some(format!("item {} ended twice", item.0))
                }
                Record::Intent(_)
                | Record::Outcome { .. }
                | Record::RestoreIntent { .. }
                | Record::RestoreOutcome { .. }
                | Record::TrashRecordRemoved { .. } => None,
            };
            if problem.is_some() {
                return problem;
            }
        }
        None
    }

    /// The intents with no outcome, in the order they were recorded.
    pub(crate) fn pending(&self) -> Vec<Intent> {
        let ended: BTreeSet<ItemId> = self
            .records
            .iter()
            .filter_map(|record| {
                if let Record::Outcome { item, .. } = record {
                    Some(*item)
                } else {
                    None
                }
            })
            .collect();
        self.records
            .iter()
            .filter_map(|record| {
                if let Record::Intent(intent) = record
                    && !ended.contains(&intent.item)
                {
                    Some(intent.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}
