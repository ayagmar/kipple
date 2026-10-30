//! What applying a plan did, per item and in total (`docs/04-safety-model.md` §7, §8).
//!
//! An item's outcome is measured against the frozen tree the user reviewed, not against
//! what was left of it after revalidation: an entry dropped before the run is counted as
//! left, with its reason, so such an item is never `done`. Only removed names are
//! confirmed. Bytes are an estimate of the allocated storage removed: an upper bound,
//! since APFS clones and btrfs and XFS reflinks share storage that removing one name
//! doesn't free, and a file with another name left frees nothing.
//!
//! Every field here is structured, so a receipt can be stored whole: errors carry kipple's
//! code and the OS error kind and number, never the OS's message (04 §8a). The message
//! stays in the in-memory error, for the session that saw it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::frozen::{FrozenTree, Mark, Marks, NodeId, RelPath, Unauthorized};
use crate::mutation::{EffectSink, Finish, Unsupported};
use crate::plan::{ItemId, JudgementField};
use crate::policy::{Caution, Hold};
use crate::rule::Method;
use crate::size::{Allocated, Apparent, SpaceEstimate};
use crate::trash::TrashSlot;

/// How many paths a receipt keeps for each reason an entry was left: the first in the
/// tree's canonical order. Receipts never list every leaf.
pub const SAMPLE: usize = 10;

named! {
    /// Why an entry the user reviewed wasn't removed.
    LeftReason {
        /// It was gone before kipple reached it.
        Gone = "gone",
        /// Another file is there now.
        Replaced = "replaced",
        /// It changed in place since it was frozen.
        Changed = "changed",
        /// It is to be kept now, such as a file git tracks since the plan was made.
        Excluded = "excluded",
        /// A directory still holding entries at the end: new ones, or ones kept. Not a
        /// failure on its own.
        NotEmpty = "not-empty",
        /// The OS refused: permission denied.
        Denied = "denied",
        /// The OS refused: the file is open or locked.
        Busy = "busy",
        /// It is something kipple doesn't remove, such as a cloud placeholder found at
        /// apply.
        Unsupported = "unsupported",
        /// Removing it failed for another reason.
        Failed = "failed",
        /// The run stopped before reaching it.
        NotReached = "not-reached",
    }
}

/// What removing one entry freed, as the executor read it from the open handle just
/// before removing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freed {
    /// The only name of a file allocating this much, estimated: storage shared with a
    /// clone or reflink isn't freed.
    Allocated(Allocated),
    /// A name of a file that keeps another name, so nothing was freed.
    Shared,
    /// The handle didn't say how much it allocated.
    Unmeasured,
}

/// What an item removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Removed {
    /// Names removed: files, links and directories. Confirmed: each removal succeeded.
    pub names: u64,
    /// Estimated allocated storage removed, an upper bound (see the module docs).
    pub estimated: Allocated,
    /// Removed names whose file kept another name, freeing nothing.
    pub shared: u64,
    /// Removed names whose allocation wasn't known, left out of `estimated`.
    pub unmeasured: u64,
}

impl Removed {
    /// Adds `other` to this.
    pub const fn add(&mut self, other: &Self) {
        self.names += other.names;
        self.estimated = Allocated(self.estimated.0.saturating_add(other.estimated.0));
        self.shared += other.shared;
        self.unmeasured += other.unmeasured;
    }

    const fn record(&mut self, freed: Freed) {
        self.names += 1;
        match freed {
            Freed::Allocated(bytes) => {
                self.estimated = Allocated(self.estimated.0.saturating_add(bytes.0));
            }
            Freed::Shared => self.shared += 1,
            Freed::Unmeasured => self.unmeasured += 1,
        }
    }
}

/// How many reviewed entries were left for one reason, with the first few of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeftCount {
    /// How many.
    pub count: u64,
    /// At most [`SAMPLE`] of their paths below the target, in canonical order.
    pub sample: Vec<RelPath>,
}

/// The reviewed entries an item didn't remove, by reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Left(pub BTreeMap<LeftReason, LeftCount>);

impl Left {
    /// How many were left, for every reason.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.0.values().map(|left| left.count).sum()
    }

    /// Whether anything was left that makes the item short of done: anything but a
    /// directory still holding entries.
    #[must_use]
    pub fn is_short(&self) -> bool {
        self.0
            .iter()
            .any(|(reason, left)| *reason != LeftReason::NotEmpty && left.count > 0)
    }
}

/// An item moved to the trash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trashed {
    /// Where it went, which is what restores it.
    pub slot: TrashSlot,
}

/// What a run of an item did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Effect {
    /// What it removed.
    pub removed: Removed,
    /// Where it moved the item, for a trash item that moved.
    pub trashed: Option<Trashed>,
    /// The reviewed entries it didn't remove.
    pub left: Left,
    /// Entries the plan left out from the start (`excluded`), kept as reviewed.
    pub excluded: u64,
}

/// Why an item was dropped before anything of it was touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    /// The authorization isn't for this plan.
    NotReviewed,
    /// Something it was judged under changed since the plan (D-063): the policy, its
    /// rule, level, recent window, method or the kept and excluded paths.
    Rejudged(JudgementField),
    /// It is Protected now, for these reasons.
    Protected(Vec<Hold>),
    /// It carries these cautions now, which it wasn't authorized with: Review when it was
    /// Safe, new cautions on a Review item, or any caution under Safe-only automation.
    Riskier(Vec<Caution>),
    /// Its rule doesn't offer its method any more.
    MethodNotOffered(Method),
    /// The fresh freeze failed.
    Freeze(ApplyError),
    /// It is on a file system kipple doesn't change.
    FileSystem(Unsupported),
    /// Revalidation dropped every reviewed entry.
    AllDropped(Left),
    /// A trash move takes the directory whole, with whatever is in it at apply time
    /// (04 §3.5), which needs that whole-directory move reviewed. Safe-only automation
    /// has no such review until a profile records one (M3b).
    WholeNotReviewed,
    /// Moving to the trash needs every entry of the item still authorized, and some
    /// weren't. Never turned into a delete.
    NotWhole {
        /// What revalidation dropped.
        left: Left,
        /// How many entries the plan left out.
        excluded: u64,
    },
    /// The apply stopped before it started.
    NotStarted(Stop),
}

named! {
    /// Why an apply stopped before an item.
    Stop {
        /// It was cancelled.
        Cancelled = "cancelled",
        /// The journal couldn't record an intent or outcome, so nothing more runs.
        JournalFailed = "journal-failed",
    }
}

/// What reconciliation found of an item whose run a crash interrupted. Never attributed
/// to kipple: something else may have changed it too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// Every entry is as frozen: nothing was removed.
    Unchanged,
    /// The target is gone.
    Gone,
    /// Another file is at the target.
    Replaced,
    /// Some of its entries are gone.
    PartlyGone {
        /// How many of the reviewed entries are gone.
        gone: u64,
        /// How many there were.
        of: u64,
    },
    /// It couldn't be checked.
    Unreadable(ApplyError),
}

named! {
    /// An item's outcome, without its detail.
    Status {
        /// Every reviewed entry was removed, or the item moved to the trash.
        Done = "done",
        /// It ran to the end, but left reviewed entries.
        Partial = "partial",
        /// It was dropped before anything of it was touched.
        Skipped = "skipped",
        /// It stopped on an error.
        Failed = "failed",
        /// The apply was cancelled while it ran.
        Cancelled = "cancelled",
        /// A crash interrupted it, and what happened can't be told.
        Indeterminate = "indeterminate",
    }
}

/// What happened to one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemOutcome {
    /// Every reviewed entry was removed, or the item moved to the trash. A directory kept
    /// because entries appeared in it during the run is a [`LeftReason::NotEmpty`] note.
    Done(Effect),
    /// The run ended, but reviewed entries were left: dropped at revalidation, changed,
    /// or refused by the OS.
    Partial(Effect),
    /// Dropped before anything of it was touched.
    Skipped(Skip),
    /// It stopped on an error; what it removed until then counts.
    Failed {
        /// The error.
        error: ApplyError,
        /// What it did until then.
        effect: Effect,
    },
    /// The apply was cancelled during it; what it removed until then counts.
    Cancelled(Effect),
    /// A crash interrupted it, and reconciliation found this.
    Indeterminate(Found),
}

impl ItemOutcome {
    /// Its status.
    #[must_use]
    pub const fn status(&self) -> Status {
        match self {
            Self::Done(_) => Status::Done,
            Self::Partial(_) => Status::Partial,
            Self::Skipped(_) => Status::Skipped,
            Self::Failed { .. } => Status::Failed,
            Self::Cancelled(_) => Status::Cancelled,
            Self::Indeterminate(_) => Status::Indeterminate,
        }
    }

    /// What it did, where it ran.
    #[must_use]
    pub const fn effect(&self) -> Option<&Effect> {
        match self {
            Self::Done(effect)
            | Self::Partial(effect)
            | Self::Cancelled(effect)
            | Self::Failed { effect, .. } => Some(effect),
            Self::Skipped(_) | Self::Indeterminate(_) => None,
        }
    }
}

named! {
    /// kipple's own code for what went wrong.
    ErrorCode {
        /// The root isn't the directory the scan opened.
        RootChanged = "root-changed",
        /// The root can't be opened.
        RootUnreadable = "root-unreadable",
        /// The target isn't there.
        TargetMissing = "target-missing",
        /// Another file is at the target.
        TargetChanged = "target-changed",
        /// The target's directory isn't the one recorded.
        ParentChanged = "parent-changed",
        /// The target's directory isn't there.
        ParentMissing = "parent-missing",
        /// A component on the way is a link or junction.
        ThroughLink = "through-link",
        /// A directory on the way or inside is another mount.
        MountBoundary = "mount-boundary",
        /// The file system isn't one kipple changes.
        UnsupportedFileSystem = "unsupported-file-system",
        /// An entry is of a kind kipple can't judge, such as a cloud placeholder.
        UnknownEntry = "unknown-entry",
        /// A directory inside can't be listed.
        Unreadable = "unreadable",
        /// Too many entries for one tree.
        TooLarge = "too-large",
        /// This backend can't do it on this OS or file system.
        Unsupported = "unsupported",
        /// The move would leave the file system: it is refused, never copied.
        CrossFileSystem = "cross-file-system",
        /// Something is already at the destination, and nothing is overwritten.
        DestinationExists = "destination-exists",
        /// The trash entry is gone: the trash was emptied or the entry removed.
        TrashMissing = "trash-missing",
        /// The trash entry isn't the item that was moved there.
        TrashChanged = "trash-changed",
        /// The executor reported an entry the run wasn't authorized to touch: one the
        /// plan left out, one revalidation dropped, or one not in the tree.
        Unauthorized = "unauthorized",
        /// The journal couldn't record it.
        Journal = "journal",
        /// The operation was cancelled before it changed anything.
        Cancelled = "cancelled",
        /// Another OS error.
        Io = "io",
    }
}

named! {
    /// The kind of an OS error, stored instead of its message.
    OsErrorKind {
        /// Not found.
        NotFound = "not-found",
        /// Permission denied.
        PermissionDenied = "permission-denied",
        /// Already exists.
        AlreadyExists = "already-exists",
        /// A directory not empty.
        DirectoryNotEmpty = "directory-not-empty",
        /// Busy or locked.
        Busy = "busy",
        /// A read-only file system.
        ReadOnly = "read-only",
        /// Across devices.
        CrossesDevices = "crosses-devices",
        /// Not a directory.
        NotADirectory = "not-a-directory",
        /// An invalid name.
        InvalidName = "invalid-name",
        /// Unsupported by the OS or file system.
        Unsupported = "unsupported",
        /// Out of space.
        StorageFull = "storage-full",
        /// Interrupted.
        Interrupted = "interrupted",
        /// Anything else.
        Other = "other",
    }
}

const KINDS: &[(io::ErrorKind, OsErrorKind)] = &[
    (io::ErrorKind::NotFound, OsErrorKind::NotFound),
    (
        io::ErrorKind::PermissionDenied,
        OsErrorKind::PermissionDenied,
    ),
    (io::ErrorKind::AlreadyExists, OsErrorKind::AlreadyExists),
    (
        io::ErrorKind::DirectoryNotEmpty,
        OsErrorKind::DirectoryNotEmpty,
    ),
    (io::ErrorKind::ResourceBusy, OsErrorKind::Busy),
    (io::ErrorKind::ReadOnlyFilesystem, OsErrorKind::ReadOnly),
    (io::ErrorKind::CrossesDevices, OsErrorKind::CrossesDevices),
    (io::ErrorKind::NotADirectory, OsErrorKind::NotADirectory),
    (io::ErrorKind::InvalidFilename, OsErrorKind::InvalidName),
    (io::ErrorKind::Unsupported, OsErrorKind::Unsupported),
    (io::ErrorKind::StorageFull, OsErrorKind::StorageFull),
    (io::ErrorKind::Interrupted, OsErrorKind::Interrupted),
];

/// An OS error as stored: its kind and its number, never its message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsError {
    /// Its kind.
    pub kind: OsErrorKind,
    /// The OS's own number (`errno`, or the Windows error or NTSTATUS), where it has one.
    pub code: Option<i32>,
}

impl OsError {
    /// The stored form of `error`.
    #[must_use]
    pub fn of(error: &io::Error) -> Self {
        let kind = KINDS
            .iter()
            .find(|(io, _)| *io == error.kind())
            .map_or(OsErrorKind::Other, |(_, kind)| *kind);
        Self {
            kind,
            code: error.raw_os_error(),
        }
    }
}

/// Something that went wrong while freezing, removing, moving, restoring or journalling.
///
/// The structured fields are what is stored and compared. `detail` keeps the upstream
/// error, with the OS's message, for the session that saw it; it is never stored.
#[derive(Debug, Clone, thiserror::Error)]
pub struct ApplyError {
    /// kipple's code.
    pub code: ErrorCode,
    /// The OS error, if one caused it.
    pub os: Option<OsError>,
    /// Where below the target, if it is about one entry.
    pub at: Option<RelPath>,
    /// The upstream error, never stored.
    #[source]
    pub detail: Option<Arc<io::Error>>,
}

impl ApplyError {
    /// An error with `code` alone.
    #[must_use]
    pub const fn new(code: ErrorCode) -> Self {
        Self {
            code,
            os: None,
            at: None,
            detail: None,
        }
    }

    /// An error with `code`, caused by the OS's `error`.
    #[must_use]
    pub fn io(code: ErrorCode, error: io::Error) -> Self {
        Self {
            code,
            os: Some(OsError::of(&error)),
            at: None,
            detail: Some(Arc::new(error)),
        }
    }

    /// This error, about the entry at `path`.
    #[must_use]
    pub fn at(mut self, path: RelPath) -> Self {
        self.at = Some(path);
        self
    }
}

/// Equal when the stored fields are: the detail isn't compared.
impl PartialEq for ApplyError {
    fn eq(&self, other: &Self) -> bool {
        (self.code, self.os, &self.at) == (other.code, other.os, &other.at)
    }
}

impl Eq for ApplyError {}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code.name())?;
        if let Some(path) = &self.at {
            write!(f, " at {path}")?;
        }
        if let Some(detail) = &self.detail {
            write!(f, ": {detail}")?;
        }
        Ok(())
    }
}

/// Where each reviewed entry stands during a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Authorized and not reported yet.
    Pending,
    /// Not authorized for this run: left out by the plan, its top, or dropped at
    /// revalidation. A report about it is a fault.
    Out,
    /// Reported.
    Reported,
}

/// Counts what a delete run does to an item's reviewed tree, entry by entry, and turns it
/// into the item's outcome. The apply loop hands it to the executor as its
/// [`EffectSink`].
///
/// It starts with what revalidation dropped already counted as left. Each entry counts
/// once: a second report about it is ignored. A report about an entry the run wasn't
/// authorized to touch, or one not in the tree, is a fault ([`ErrorCode::Unauthorized`])
/// that fails the item, and a removal it reports still counts as removed.
#[derive(Debug)]
pub struct Tally<'run> {
    tree: &'run FrozenTree,
    states: Vec<State>,
    removed: Removed,
    left: BTreeMap<LeftReason, Counted>,
    excluded: u64,
    fault: Option<ApplyError>,
}

/// A reason's count and the smallest node IDs, which are the first in canonical order.
#[derive(Debug, Default)]
struct Counted {
    count: u64,
    sample: BTreeSet<NodeId>,
}

impl<'run> Tally<'run> {
    /// A tally of a run over `tree` with what `marks` still authorize.
    #[must_use]
    pub fn new(tree: &'run FrozenTree, marks: &Marks) -> Self {
        let mut tally = Self {
            tree,
            states: Vec::with_capacity(tree.entry_count()),
            removed: Removed::default(),
            left: BTreeMap::new(),
            excluded: 0,
            fault: None,
        };
        for entry in tree.entries() {
            let state = match marks.get(entry.id()) {
                Mark::Remove => State::Pending,
                Mark::Dropped(reason) => {
                    tally.count(entry.id(), reason);
                    State::Out
                }
                Mark::Unauthorized(Unauthorized::Excluded) => {
                    tally.excluded += 1;
                    State::Out
                }
                Mark::Unauthorized(Unauthorized::Top) => State::Out,
            };
            tally.states.push(state);
        }
        tally
    }

    fn count(&mut self, id: NodeId, reason: LeftReason) {
        let counted = self.left.entry(reason).or_default();
        counted.count += 1;
        counted.sample.insert(id);
        if counted.sample.len() > SAMPLE {
            counted.sample.pop_last();
        }
    }

    /// What to make of a report about `id`: the first about an entry this run may touch
    /// is taken, a second about any entry ignored, and the first about any other entry,
    /// or one not in the tree, is a fault.
    fn take(&mut self, id: NodeId) -> Report {
        let Some(state) = self.states.get_mut(id.index()) else {
            self.fault(None);
            return Report::Unauthorized;
        };
        let report = match *state {
            State::Pending => Report::Taken,
            State::Reported => return Report::Again,
            State::Out => Report::Unauthorized,
        };
        *state = State::Reported;
        if report == Report::Unauthorized {
            self.fault(self.tree.entry(id).map(|entry| entry.path()));
        }
        report
    }

    /// Records the first report about an entry the run wasn't authorized to touch.
    fn fault(&mut self, at: Option<RelPath>) {
        if self.fault.is_none() {
            let error = ApplyError::new(ErrorCode::Unauthorized);
            self.fault = Some(match at {
                Some(at) => error.at(at),
                None => error,
            });
        }
    }

    /// The effect so far, every authorized entry not reported counted as not reached.
    fn effect(mut self) -> Effect {
        let pending: Vec<NodeId> = self
            .tree
            .entries()
            .filter(|entry| self.states.get(entry.id().index()) == Some(&State::Pending))
            .map(|entry| entry.id())
            .collect();
        for id in pending {
            self.count(id, LeftReason::NotReached);
        }
        self.into_effect()
    }

    fn into_effect(self) -> Effect {
        let tree = self.tree;
        let left = self
            .left
            .into_iter()
            .map(|(reason, counted)| {
                let sample = counted
                    .sample
                    .into_iter()
                    .filter_map(|id| tree.entry(id).map(|entry| entry.path()))
                    .collect();
                let count = LeftCount {
                    count: counted.count,
                    sample,
                };
                (reason, count)
            })
            .collect();
        Effect {
            removed: self.removed,
            trashed: None,
            left: Left(left),
            excluded: self.excluded,
        }
    }

    /// What revalidation dropped and what the plan left out, before anything ran.
    /// Entries still authorized aren't counted.
    #[must_use]
    pub fn before_run(self) -> Effect {
        self.into_effect()
    }

    /// The item's outcome once the executor returned `run`.
    ///
    /// A report about an entry the run wasn't authorized to touch makes it failed, with
    /// that fault, whatever the run returned. Otherwise an error makes it failed and
    /// cancellation cancelled, with what was removed until then. A run that ended is done
    /// only when nothing reviewed was left but directories still holding entries;
    /// otherwise it is partial.
    #[must_use]
    pub fn finish(mut self, run: Result<Finish, ApplyError>) -> ItemOutcome {
        let fault = self.fault.take();
        let effect = self.effect();
        match fault.map_or(run, Err) {
            Err(error) => ItemOutcome::Failed { error, effect },
            Ok(Finish::Cancelled) => ItemOutcome::Cancelled(effect),
            Ok(Finish::Completed) if effect.left.is_short() => ItemOutcome::Partial(effect),
            Ok(Finish::Completed) => ItemOutcome::Done(effect),
        }
    }
}

/// What a [`Tally`] made of one report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Report {
    /// The first about an entry the run may touch.
    Taken,
    /// A second about one.
    Again,
    /// About an entry the run wasn't authorized to touch.
    Unauthorized,
}

impl EffectSink for Tally<'_> {
    fn removed(&mut self, id: NodeId, freed: Freed) {
        match self.take(id) {
            Report::Taken | Report::Unauthorized => self.removed.record(freed),
            Report::Again => {}
        }
    }

    fn left(&mut self, id: NodeId, reason: LeftReason) {
        if self.take(id) == Report::Taken {
            self.count(id, reason);
        }
    }
}

/// One item of a receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptItem {
    /// Its place in the plan.
    pub item: ItemId,
    /// The finding's path, as it was shown.
    pub path: PathBuf,
    /// How it was to be removed.
    pub method: Method,
    /// What the scan estimated removing it would free.
    pub estimate: Option<SpaceEstimate>,
    /// What happened.
    pub outcome: ItemOutcome,
}

/// What applying a plan did, item by item, in plan order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Receipt {
    /// Every item of the plan.
    pub items: Vec<ReceiptItem>,
}

/// What moved to the trash, still on disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrashedTotal {
    /// Items moved.
    pub items: u64,
    /// Their apparent size, as the scan estimated it.
    pub apparent: Apparent,
    /// Their allocated size as the scan estimated it, where every one of them had one.
    pub allocated: Option<Allocated>,
}

/// A receipt's totals, the one place they are counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    /// What every item removed, failed and cancelled ones included: only what each run
    /// reported removed.
    pub removed: Removed,
    /// What moved to the trash.
    pub trashed: TrashedTotal,
    /// How many items ended in each status.
    pub items: BTreeMap<Status, u64>,
}

impl Receipt {
    /// Its totals.
    #[must_use]
    pub fn totals(&self) -> Totals {
        let mut totals = Totals {
            trashed: TrashedTotal {
                allocated: Some(Allocated(0)),
                ..TrashedTotal::default()
            },
            ..Totals::default()
        };
        for item in &self.items {
            *totals.items.entry(item.outcome.status()).or_default() += 1;
            let Some(effect) = item.outcome.effect() else {
                continue;
            };
            totals.removed.add(&effect.removed);
            if effect.trashed.is_some() {
                let trashed = &mut totals.trashed;
                trashed.items += 1;
                let estimate = item.estimate.as_ref();
                trashed.apparent.0 += estimate.map_or(0, |size| size.apparent.0);
                trashed.allocated = trashed
                    .allocated
                    .zip(estimate.and_then(|size| size.allocated))
                    .map(|(sum, size)| Allocated(sum.0 + size.0));
            }
        }
        totals
    }
}
