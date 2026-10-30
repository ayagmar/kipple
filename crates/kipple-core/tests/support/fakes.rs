//! Fakes of the mutation ports, for tests of the contract and of the apply loop. Include
//! with `#[path = "support/fakes.rs"] mod fakes;`.
//!
//! They model the platform's documented behaviour, not more: the executor removes only
//! what an operation authorizes, children before their directory, and keeps a directory
//! that still holds anything; the journal checks the order the apply loop must keep.
#![expect(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    reason = "a fake whose lock is poisoned or whose fixture is invalid fails the test"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Mutex;

use kipple_core::{
    Allocated, Anchor, ApplyError, CancelToken, Component, DeleteOp, Digest, EffectSink, EntryKind,
    EntrySnapshot, ErrorCode, Executor, FileIdentity, Finish, Freed, Freeze, Freezer, FrozenTree,
    Intent, ItemId, ItemOutcome, Journal, LeftReason, Marks, NameCodec, Plan, RelPath,
    RestoreRequest, SlotState, TrashBackend, TrashDir, TrashOp, TrashSlot, Trashed,
};

/// Stores names as the OS does: bytes on Unix, UTF-16 code units on Windows, where a
/// name holding `:` is refused as `NameCodec` requires.
#[derive(Debug)]
pub(crate) struct TestCodec;

impl NameCodec for TestCodec {
    #[cfg(unix)]
    fn encode(&self, name: &OsStr) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt as _;
        name.as_bytes().to_vec()
    }

    #[cfg(unix)]
    fn decode(&self, bytes: &[u8]) -> Option<OsString> {
        use std::os::unix::ffi::OsStrExt as _;
        Some(OsStr::from_bytes(bytes).to_owned())
    }

    #[cfg(windows)]
    fn encode(&self, name: &OsStr) -> Vec<u8> {
        use std::os::windows::ffi::OsStrExt as _;
        name.encode_wide().flat_map(u16::to_le_bytes).collect()
    }

    #[cfg(windows)]
    fn decode(&self, bytes: &[u8]) -> Option<OsString> {
        use std::os::windows::ffi::OsStringExt as _;
        let (units, rest) = bytes.as_chunks::<2>();
        if !rest.is_empty() {
            return None;
        }
        let wide: Vec<u16> = units.iter().map(|unit| u16::from_le_bytes(*unit)).collect();
        if wide.contains(&u16::from(b':')) {
            return None;
        }
        Some(OsString::from_wide(&wide))
    }
}

/// Freezes from a table of anchors, which a test changes between plan and apply.
#[derive(Debug, Default)]
pub(crate) struct FakeFreezer {
    pub(crate) trees: Mutex<BTreeMap<PathBuf, Result<Freeze, ApplyError>>>,
    pub(crate) calls: Mutex<Vec<PathBuf>>,
}

impl FakeFreezer {
    /// Makes `anchor` freeze as `freeze` from now on.
    pub(crate) fn set(&self, anchor: &Anchor, freeze: Result<Freeze, ApplyError>) {
        self.trees.lock().unwrap().insert(anchor.path(), freeze);
    }
}

impl Freezer for FakeFreezer {
    fn freeze(&self, anchor: &Anchor, cancel: &CancelToken) -> Result<Freeze, ApplyError> {
        self.calls.lock().unwrap().push(anchor.path());
        if cancel.is_cancelled() {
            return Err(ApplyError::new(ErrorCode::Cancelled));
        }
        self.trees
            .lock()
            .unwrap()
            .get(&anchor.path())
            .cloned()
            .unwrap_or_else(|| Err(ApplyError::new(ErrorCode::TargetMissing)))
    }
}

/// What a fake delete meets, by path below the target.
#[derive(Debug, Default, Clone)]
pub(crate) struct Script {
    /// Entries the OS refuses to remove.
    pub(crate) denied: BTreeSet<RelPath>,
    /// Directories that gain an entry during the run, so they stay.
    pub(crate) appeared: BTreeSet<RelPath>,
    /// The run fails, changing nothing more, on reaching this entry.
    pub(crate) fails_at: Option<RelPath>,
    /// The run is cancelled after this many removals.
    pub(crate) cancel_after: Option<usize>,
}

/// Deletes in memory, as `Script` says, and records each operation it ran.
#[derive(Debug, Default)]
pub(crate) struct FakeExecutor {
    pub(crate) script: Script,
    /// The digest of every operation run, in order.
    pub(crate) ran: Mutex<Vec<Digest>>,
}

impl FakeExecutor {
    /// Runs over `tree` with what `marks` authorize, the way `delete` does.
    pub(crate) fn run(
        &self,
        tree: &FrozenTree,
        marks: &Marks,
        effects: &mut dyn EffectSink,
        cancel: &CancelToken,
    ) -> Result<Finish, ApplyError> {
        let mut gone = BTreeSet::new();
        let mut removed = 0;
        // Reverse canonical order: every entry before the directory holding it.
        let entries: Vec<_> = tree.entries().collect();
        for entry in entries.into_iter().rev() {
            if !marks.is_authorized(entry.id()) {
                continue;
            }
            if cancel.is_cancelled() || self.script.cancel_after == Some(removed) {
                return Ok(Finish::Cancelled);
            }
            let path = entry.path();
            if self.script.fails_at.as_ref() == Some(&path) {
                return Err(ApplyError::new(ErrorCode::Io).at(path));
            }
            if self.script.denied.contains(&path) {
                effects.left(entry.id(), LeftReason::Denied);
                continue;
            }
            let holds = entry.children().any(|child| !gone.contains(&child.id()))
                || self.script.appeared.contains(&path);
            if entry.kind() == EntryKind::Dir && holds {
                effects.left(entry.id(), LeftReason::NotEmpty);
                continue;
            }
            effects.removed(entry.id(), freed(entry.snapshot()));
            gone.insert(entry.id());
            removed += 1;
        }
        Ok(Finish::Completed)
    }
}

fn freed(snapshot: Option<EntrySnapshot>) -> Freed {
    match snapshot {
        Some(snapshot) if snapshot.links > 1 && snapshot.kind != EntryKind::Dir => Freed::Shared,
        Some(snapshot) => Freed::Allocated(Allocated(snapshot.fingerprint.size)),
        None => Freed::Unmeasured,
    }
}

impl Executor for FakeExecutor {
    fn delete(
        &self,
        op: &DeleteOp,
        effects: &mut dyn EffectSink,
        cancel: &CancelToken,
    ) -> Result<Finish, ApplyError> {
        let target = op.target();
        self.ran.lock().unwrap().push(target.digest());
        self.run(target.tree(), target.marks(), effects, cancel)
    }
}

/// A trash in memory: slots hold the identity of what moved there.
#[derive(Debug)]
pub(crate) struct FakeTrash {
    pub(crate) dir: TrashDir,
    /// Refuses everything with this, as an unsupported backend does.
    pub(crate) unsupported: bool,
    pub(crate) slots: Mutex<BTreeMap<Component, FileIdentity>>,
    /// Original paths something now occupies.
    pub(crate) occupied: Mutex<BTreeSet<PathBuf>>,
    /// Parents that were replaced since the move, by the anchor's path.
    pub(crate) replaced_parents: Mutex<BTreeSet<PathBuf>>,
}

impl FakeTrash {
    pub(crate) fn new() -> Self {
        Self {
            dir: TrashDir {
                path: PathBuf::from("/trash/files"),
                identity: FileIdentity {
                    volume: 7,
                    file: 900,
                },
            },
            unsupported: false,
            slots: Mutex::default(),
            occupied: Mutex::default(),
            replaced_parents: Mutex::default(),
        }
    }

    const fn check(&self) -> Result<(), ApplyError> {
        if self.unsupported {
            return Err(ApplyError::new(ErrorCode::Unsupported));
        }
        Ok(())
    }
}

impl TrashBackend for FakeTrash {
    fn available(&self, _: &Anchor) -> Result<(), ApplyError> {
        self.check()
    }

    fn destination(&self, op: &TrashOp) -> Result<TrashSlot, ApplyError> {
        self.check()?;
        let name = format!("item-{}", op.target().item().0);
        Ok(TrashSlot {
            trash: self.dir.clone(),
            name: Component::new(name).unwrap(),
        })
    }

    fn move_whole(
        &self,
        op: &TrashOp,
        slot: &TrashSlot,
        cancel: &CancelToken,
    ) -> Result<Trashed, ApplyError> {
        self.check()?;
        if cancel.is_cancelled() {
            return Err(ApplyError::new(ErrorCode::Cancelled));
        }
        let identity = op.target().tree().top().snapshot().unwrap().identity;
        let mut slots = self.slots.lock().unwrap();
        if slots.contains_key(&slot.name) {
            return Err(ApplyError::new(ErrorCode::DestinationExists));
        }
        slots.insert(slot.name.clone(), identity);
        drop(slots);
        Ok(Trashed { slot: slot.clone() })
    }

    fn find(&self, slot: &TrashSlot, target: &EntrySnapshot) -> Result<SlotState, ApplyError> {
        Ok(match self.slots.lock().unwrap().get(&slot.name) {
            None => SlotState::Empty,
            Some(identity) if *identity == target.identity => SlotState::Holds,
            Some(_) => SlotState::Other,
        })
    }

    fn restore(&self, request: &RestoreRequest, _: &CancelToken) -> Result<(), ApplyError> {
        let mut slots = self.slots.lock().unwrap();
        match slots.get(&request.slot().name) {
            None => return Err(ApplyError::new(ErrorCode::TrashMissing)),
            Some(identity) if *identity != request.target().identity => {
                return Err(ApplyError::new(ErrorCode::TrashChanged));
            }
            Some(_) => {}
        }
        let original = request.anchor().path();
        if self.replaced_parents.lock().unwrap().contains(&original) {
            return Err(ApplyError::new(ErrorCode::ParentChanged));
        }
        if !self.occupied.lock().unwrap().insert(original) {
            return Err(ApplyError::new(ErrorCode::DestinationExists));
        }
        slots.remove(&request.slot().name);
        drop(slots);
        Ok(())
    }
}

/// One durable record.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Record {
    Plan(Digest),
    Intent(Intent),
    Outcome(ItemId, ItemOutcome),
}

/// Records in memory, failing the write numbered `fails_at` (from 0) and every one after,
/// as a store whose disk filled up does.
#[derive(Debug, Default)]
pub(crate) struct FakeJournal {
    pub(crate) records: Vec<Record>,
    pub(crate) fails_at: Option<usize>,
    pub(crate) writes: usize,
}

impl FakeJournal {
    fn write(&mut self, record: Record) -> Result<(), ApplyError> {
        let attempt = self.writes;
        self.writes += 1;
        if self.fails_at.is_some_and(|fails_at| attempt >= fails_at) {
            return Err(ApplyError::new(ErrorCode::Journal));
        }
        self.records.push(record);
        Ok(())
    }

    /// Why the records break the journal's order, if they do: the plan first, then per
    /// item one intent before its one outcome, every intent naming the stored plan.
    pub(crate) fn order_problem(&self) -> Option<String> {
        let mut plan = None;
        let mut intents = BTreeSet::new();
        let mut outcomes = BTreeSet::new();
        for (index, record) in self.records.iter().enumerate() {
            match record {
                Record::Plan(digest) if index == 0 => plan = Some(*digest),
                Record::Plan(_) => return Some(format!("a second plan at {index}")),
                Record::Intent(intent) if Some(intent.plan) != plan => {
                    return Some(format!("an intent for another plan at {index}"));
                }
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
}

impl Journal for FakeJournal {
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
