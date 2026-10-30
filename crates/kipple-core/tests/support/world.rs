//! A machine in memory for the apply loop's tests. Include with
//! `#[path = "support/world.rs"] mod world;`.
//!
//! Its targets are what the freezer reads, the executor deletes from and the trash moves
//! whole, so what one port changes the next one sees, as on a real disk. Its journal keeps
//! the durable records, which the executor and the trash check for the item's intent
//! before changing anything. A crash is an unwind out of the journal or the executor at a
//! chosen point, after which the state is what a disk would hold.
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
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use kipple_core::{
    Allocated, Anchor, ApplyError, CancelToken, Component, DeleteOp, Digest, EffectSink, EntryKind,
    EntrySnapshot, ErrorCode, Executor, FileIdentity, FileSystemClass, Finish, Freed, Freeze,
    Freezer, Intent, ItemId, ItemOutcome, Journal, LeftReason, LocalFileSystem, Plan, RelPath,
    RestoreRequest, Scope, SlotState, TrashBackend, TrashDir, TrashOp, TrashSlot, Trashed,
    TreeBuilder, Volume,
};

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
        }
    }
}

/// One durable journal record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Record {
    Plan(Digest),
    Intent(Intent),
    Outcome(ItemId, ItemOutcome),
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

    /// The journal, writing to this world's records.
    pub(crate) const fn journal(&self) -> WorldJournal<'_> {
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
pub(crate) fn tree_of(target: &Target) -> kipple_core::FrozenTree {
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
        if self.state().trash_unsupported {
            return Err(ApplyError::new(ErrorCode::Unsupported));
        }
        Ok(())
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

    fn restore(&self, _: &RestoreRequest, _: &CancelToken) -> Result<(), ApplyError> {
        Err(ApplyError::new(ErrorCode::Unsupported))
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
        self.write(Record::Plan(plan.digest()))
    }

    fn intent(&mut self, intent: &Intent) -> Result<(), ApplyError> {
        self.write(Record::Intent(intent.clone()))
    }

    fn outcome(&mut self, item: ItemId, outcome: &ItemOutcome) -> Result<(), ApplyError> {
        self.write(Record::Outcome(item, outcome.clone()))
    }
}

impl State {
    /// Why the records break the journal's order, if they do: the plan first, then per
    /// item at most one intent, and at most one outcome after it.
    pub(crate) fn order_problem(&self) -> Option<String> {
        let mut intents = BTreeSet::new();
        let mut outcomes = BTreeSet::new();
        for (index, record) in self.records.iter().enumerate() {
            match record {
                Record::Plan(_) if index == 0 => {}
                Record::Plan(_) => return Some(format!("a second plan at {index}")),
                _ if index == 0 => return Some("a record before the plan".to_owned()),
                Record::Intent(intent) if !intents.insert(intent.item) => {
                    return Some(format!("item {} intended twice", intent.item.0));
                }
                Record::Outcome(item, _) if !intents.contains(item) => {
                    return Some(format!("item {} ended with no intent", item.0));
                }
                Record::Outcome(item, _) if !outcomes.insert(*item) => {
                    return Some(format!("item {} ended twice", item.0));
                }
                Record::Intent(_) | Record::Outcome(..) => {}
            }
        }
        None
    }

    /// The intents with no outcome, in the order they were recorded.
    pub(crate) fn pending(&self) -> Vec<Intent> {
        let ended: BTreeSet<ItemId> = self
            .records
            .iter()
            .filter_map(|record| match record {
                Record::Outcome(item, _) => Some(*item),
                Record::Plan(_) | Record::Intent(_) => None,
            })
            .collect();
        self.records
            .iter()
            .filter_map(|record| match record {
                Record::Intent(intent) if !ended.contains(&intent.item) => Some(intent.clone()),
                Record::Plan(_) | Record::Intent(_) | Record::Outcome(..) => None,
            })
            .collect()
    }
}
