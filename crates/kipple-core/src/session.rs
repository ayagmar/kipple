//! Mutating commands from start to end: `clean`, `apply FILE` and `restore` (spec M3b
//! pinned 1, 2, 10, 12, 17; D-067).
//!
//! Each one plans, has the plan confirmed, then under the store's lock reconciles every
//! run a crash left pending, stores a new run and applies it, telling its frontend each
//! step as an [`ApplyEvent`]. The lock is taken only after confirmation, so a prompt
//! never keeps a scheduled `clean` waiting, and apply scans again under it anyway.
//!
//! How the plan is confirmed decides what it may hold: `--yes` plans and authorizes Safe
//! deletes only ([`Authorization::SafeOnly`]); a person who answers the prompt authorizes
//! exactly the plan shown, by its digest ([`Authorization::Reviewed`]), and the plan
//! holds Safe items only until the TUI's review (M3c).

use std::fmt;

use crate::apply::{
    ApplyPorts, ApplyRequest, ItemEvent, ItemSink, Reconciled, Recorders, apply, reconcile,
};
use crate::authorize::Authorization;
use crate::cancel::CancelToken;
use crate::digest::Digest;
use crate::engine::ScanReport;
use crate::format::{encode_line, encode_stored_plan};
use crate::frozen::NameCodec;
use crate::imported::{DroppedItem, ImportedPlan, ReplanRequest, replan};
use crate::journal::{Journal, Record, RestoreResult};
use crate::outcome::{ApplyError, ErrorCode, Found, ItemOutcome, Receipt, Skip, Status, Stop};
use crate::plan::{ItemId, Plan, Selection};
use crate::planner::{Judge, Merged, PlanMode, PlanRequest, Refused, plan};
use crate::run::{Depth, ItemState, RunRead, StoredItem, StoredRun, Unreadable, read_run};
use crate::store::{Locked, RunId, RunStore, StoreRefusal};
use crate::trash::{RestoreRequest, TrashSlot};

/// What a mutating command works under and through.
#[derive(Debug, Clone, Copy)]
pub struct Session<'session> {
    /// The current config: what judges and removes each finding now.
    pub judge: Judge<'session>,
    /// Where runs are kept.
    pub store: &'session dyn RunStore,
    /// What scans again, freezes, deletes, trashes and asks git.
    pub ports: ApplyPorts<'session>,
    /// How names are stored.
    pub codec: &'session dyn NameCodec,
}

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

/// What a person is asked to confirm: the plan as it will be authorized, and what was
/// left out of it.
#[derive(Debug, Clone, Copy)]
pub struct Review<'review> {
    /// The plan. Confirming authorizes exactly this plan, by its digest.
    pub plan: &'review Plan,
    /// Selected findings merged into another's item.
    pub merged: &'review [Merged],
    /// Selected findings not planned, and why.
    pub refused: &'review [Refused],
    /// For `apply FILE`, the file's items left out, and why.
    pub dropped: &'review [DroppedItem],
}

named! {
    /// A person's answer to the prompt.
    Answer {
        /// Run it.
        Yes = "yes",
        /// Don't.
        No = "no",
    }
}

/// Asks a person to confirm a plan.
pub trait Confirm {
    /// Shows `review` and asks whether to run it.
    fn confirm(&mut self, review: &Review<'_>) -> Answer;
}

/// How a plan is confirmed.
pub enum Consent<'consent> {
    /// `--yes`: Safe deletes only, planned and authorized as automation.
    Yes,
    /// A person is asked, and authorizes the plan shown.
    Ask(&'consent mut dyn Confirm),
}

impl fmt::Debug for Consent<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Yes => "Yes",
            Self::Ask(_) => "Ask",
        })
    }
}

impl Consent<'_> {
    /// What a plan confirmed this way may hold.
    const fn mode(&self) -> PlanMode {
        match self {
            Self::Yes => PlanMode::SafeOnly,
            Self::Ask(_) => PlanMode::Safe,
        }
    }

    /// The authorization for `plan`, or `None` when the person said no.
    fn authorize(&mut self, review: &Review<'_>) -> Option<Authorization> {
        match self {
            Self::Yes => Some(Authorization::SafeOnly),
            Self::Ask(confirm) => match confirm.confirm(review) {
                Answer::Yes => Some(Authorization::Reviewed {
                    plan: review.plan.digest(),
                }),
                Answer::No => None,
            },
        }
    }
}

/// How a mutating command ended, which decides its exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// Every item was done or skipped, and at least one was done, or every restore asked
    /// for restored the item. Exit 0.
    Done,
    /// The plan is empty: nothing to clean. Exit 0.
    NothingToClean,
    /// Items were selected or planned and none ran: every one was refused or skipped, or
    /// nothing could be restored. Exit 3.
    NoneRan,
    /// The person said no. Nothing ran. Exit 3.
    Declined,
    /// Every item of a plan file dropped when planned again because the disk changed
    /// since (`Dropped::stale`): the plan is stale, make a new one. Exit 3.
    Stale,
    /// An item is partial, failed, indeterminate or stranded, or a restore failed.
    /// Exit 1.
    Failed,
    /// The store refused: nothing ran. Exit 1.
    Refused(StoreRefusal),
    /// It was cancelled: what finished stays finished. Exit 130.
    Cancelled,
}

impl Ending {
    /// The process's exit code.
    #[must_use]
    pub const fn code(&self) -> u8 {
        match self {
            Self::Done | Self::NothingToClean => 0,
            Self::Failed | Self::Refused(_) => 1,
            Self::NoneRan | Self::Declined | Self::Stale => 3,
            Self::Cancelled => 130,
        }
    }

    /// How a run with `receipt` ended.
    fn of(receipt: &Receipt) -> Self {
        let statuses: Vec<Status> = receipt
            .items
            .iter()
            .map(|item| item.outcome.status())
            .collect();
        let stopped = receipt.items.iter().any(|item| {
            matches!(
                item.outcome,
                ItemOutcome::Cancelled(_) | ItemOutcome::Skipped(Skip::NotStarted(Stop::Cancelled))
            )
        });
        let bad = statuses.iter().any(|status| {
            matches!(
                status,
                Status::Partial | Status::Failed | Status::Indeterminate
            )
        });
        let journal = receipt.items.iter().any(|item| {
            matches!(
                item.outcome,
                ItemOutcome::Skipped(Skip::NotStarted(Stop::JournalFailed))
            )
        });
        if bad || journal {
            Self::Failed
        } else if stopped {
            Self::Cancelled
        } else if statuses.contains(&Status::Done) {
            Self::Done
        } else {
            Self::NoneRan
        }
    }
}

/// How a mutating command finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// How it ended.
    pub ending: Ending,
    /// The run it stored, if it got that far.
    pub run: Option<RunId>,
    /// What its run did, item by item; empty when nothing ran.
    pub receipt: Receipt,
    /// The plan file's items left out of the fresh plan, with why; empty for `clean`.
    pub dropped: Vec<DroppedItem>,
}

impl Finished {
    fn before(ending: Ending) -> Self {
        Self {
            ending,
            run: None,
            receipt: Receipt::default(),
            dropped: Vec::new(),
        }
    }
}

/// What `clean` cleans: the findings selected from the scan the person saw.
#[derive(Debug, Clone, Copy)]
pub struct CleanRequest<'scan> {
    /// The scan.
    pub scan: &'scan ScanReport,
    /// The findings selected (`--only`, or every Safe finding).
    pub selection: &'scan Selection,
}

/// `clean`: plans `request` under the session's judge as `consent` allows, has it
/// confirmed, then runs it.
pub fn run_clean(
    request: &CleanRequest<'_>,
    session: &Session<'_>,
    mut consent: Consent<'_>,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
) -> Finished {
    let planned = plan(
        &PlanRequest {
            scan: request.scan,
            selection: request.selection,
            mode: consent.mode(),
        },
        &session.judge,
        session.ports.mutation,
        cancel,
    );
    if cancel.is_cancelled() {
        return Finished::before(Ending::Cancelled);
    }
    if planned.plan.entries().is_empty() {
        let ending = if planned.refused.is_empty() {
            Ending::NothingToClean
        } else {
            Ending::NoneRan
        };
        return Finished::before(ending);
    }
    let review = Review {
        plan: &planned.plan,
        merged: &planned.merged,
        refused: &planned.refused,
        dropped: &[],
    };
    let Some(authorization) = consent.authorize(&review) else {
        return Finished::before(Ending::Declined);
    };
    execute(&planned.plan, authorization, session, events, cancel)
}

/// What `apply FILE` applies: a plan file, and a fresh scan of the roots the current
/// config resolves.
#[derive(Debug, Clone, Copy)]
pub struct ApplyFile<'file> {
    /// The plan file, read.
    pub imported: &'file ImportedPlan,
    /// The fresh scan.
    pub scan: &'file ScanReport,
}

/// `apply FILE`: plans the file again under the session's judge, has the fresh plan
/// confirmed, then runs it. A file none of whose items survive is stale.
pub fn run_apply(
    file: &ApplyFile<'_>,
    session: &Session<'_>,
    mut consent: Consent<'_>,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
) -> Finished {
    let imported = file.imported;
    let request = ReplanRequest {
        imported,
        scan: file.scan,
        mode: consent.mode(),
    };
    let replanned = replan(&request, &session.judge, session.ports.mutation, cancel);
    if cancel.is_cancelled() {
        return Finished::before(Ending::Cancelled);
    }
    if replanned.plan.entries().is_empty() {
        let ending = if imported.is_empty() {
            Ending::NothingToClean
        } else if replanned.dropped.iter().all(|item| item.why.stale()) {
            Ending::Stale
        } else {
            Ending::NoneRan
        };
        return Finished {
            dropped: replanned.dropped,
            ..Finished::before(ending)
        };
    }
    let review = Review {
        plan: &replanned.plan,
        merged: &[],
        refused: &[],
        dropped: &replanned.dropped,
    };
    let Some(authorization) = consent.authorize(&review) else {
        return Finished::before(Ending::Declined);
    };
    let session = Session {
        judge: Judge {
            methods: &replanned.methods,
            ..session.judge
        },
        ..*session
    };
    Finished {
        dropped: replanned.dropped,
        ..execute(&replanned.plan, authorization, &session, events, cancel)
    }
}

/// Takes the lock, reconciles every pending run, stores `plan` as a new run and applies
/// it.
fn execute(
    plan: &Plan,
    authorization: Authorization,
    session: &Session<'_>,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
) -> Finished {
    let Ok(stored) = encode_stored_plan(plan, session.codec) else {
        return Finished::before(Ending::Refused(StoreRefusal::TooLarge));
    };
    let lock = match prepare(session, events, cancel) {
        Ok(lock) => lock,
        Err(ending) => return Finished::before(ending),
    };
    let first = encode_line(
        &Record::Plan {
            digest: plan.digest(),
        },
        session.codec,
    );
    let run = match session.store.create(&lock, &stored, &first) {
        Ok(run) => run,
        Err(refusal) => return Finished::before(Ending::Refused(refusal)),
    };
    events.event(ApplyEvent::RunStarted {
        run: run.clone(),
        items: plan.entries().len(),
    });
    let mut journal = StoreJournal {
        store: session.store,
        lock: &lock,
        run: run.clone(),
        at: first.len() as u64,
        plan: plan.digest(),
        codec: session.codec,
        restored: Vec::new(),
        failed: false,
    };
    let mut items = RunEvents {
        run: run.clone(),
        events,
    };
    let to = Recorders {
        journal: &mut journal,
        events: &mut items,
    };
    let receipt = apply(
        &ApplyRequest {
            plan,
            authorization,
        },
        &session.judge,
        &session.ports,
        to,
        cancel,
    );
    // A record that couldn't be written fails the run even when its item ended: the
    // stored run doesn't say so, and the next command reconciles it.
    let ending = if journal.failed {
        Ending::Failed
    } else {
        Ending::of(&receipt)
    };
    Finished {
        ending,
        run: Some(run),
        receipt,
        dropped: Vec::new(),
    }
}

/// Takes the lock and reconciles every run a crash left pending. Cancelling stops it,
/// leaving the rest pending.
fn prepare(
    session: &Session<'_>,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
) -> Result<Locked, Ending> {
    let lock = session.store.lock(cancel).map_err(Ending::Refused)?;
    let runs = session
        .store
        .runs()
        .map_err(|error| Ending::Refused(StoreRefusal::Write(error)))?;
    for id in runs {
        if cancel.is_cancelled() {
            return Err(Ending::Cancelled);
        }
        let headers = read_run(session.store, &id, Depth::Headers, session.codec);
        let read = if matches!(&headers, RunRead::Read(run) if pending(run)) {
            read_run(session.store, &id, Depth::Trees, session.codec)
        } else {
            headers
        };
        match read {
            RunRead::Read(run) if pending(&run) => {
                reconcile_run(&run, &lock, session, events, cancel)?;
            }
            RunRead::Read(_) | RunRead::NeverStarted(_) => {}
            RunRead::Unreadable(run, why) => events.event(ApplyEvent::Unreadable { run, why }),
        }
    }
    if cancel.is_cancelled() {
        return Err(Ending::Cancelled);
    }
    Ok(lock)
}

/// Reconciles, under the lock, every run a crash left pending, as each mutating command
/// does first. `Done` when nothing stays pending.
pub fn run_reconcile(
    session: &Session<'_>,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
) -> Ending {
    match prepare(session, events, cancel) {
        Ok(_) => Ending::Done,
        Err(ending) => ending,
    }
}

fn pending(run: &StoredRun) -> bool {
    !run.pending().is_empty() || !run.pending_restores().is_empty()
}

fn reconcile_run(
    run: &StoredRun,
    lock: &Locked,
    session: &Session<'_>,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
) -> Result<(), Ending> {
    let mut journal = StoreJournal::after(run, lock, session);
    let reconciled = reconcile(run, session.ports.mutation, &mut journal, cancel)
        .map_err(|error| Ending::Refused(StoreRefusal::Write(error)))?;
    for one in reconciled {
        events.event(ApplyEvent::Reconciled {
            run: run.id().clone(),
            reconciled: one,
        });
    }
    if cancel.is_cancelled() {
        return Err(Ending::Cancelled);
    }
    Ok(())
}

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
        ending: restore_ending(&items, ask),
        items,
    }
}

/// How restoring `items` for `ask` ended: done when something was restored and, for
/// items named one by one, every one was.
fn restore_ending(items: &[Restored], ask: &RestoreAsk) -> Ending {
    let every = ask.items.is_empty();
    let ran = |restoring: &Restoring| matches!(restoring, Restoring::Ran(_));
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
    } else if items.iter().any(|item| ran(&item.restoring))
        && (every || items.iter().all(|item| ran(&item.restoring)))
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
        .effect()
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

/// Tells `events` each item event of `run`.
struct RunEvents<'events> {
    run: RunId,
    events: &'events mut dyn EventSink,
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

impl ItemSink for RunEvents<'_> {
    fn item(&mut self, event: ItemEvent) {
        self.events.event(ApplyEvent::Item {
            run: self.run.clone(),
            event,
        });
    }
}

/// The journal of one stored run, appended under the lock.
struct StoreJournal<'run> {
    store: &'run dyn RunStore,
    lock: &'run Locked,
    run: RunId,
    /// Where its sound records end.
    at: u64,
    /// Its stored plan's digest.
    plan: Digest,
    codec: &'run dyn NameCodec,
    /// Items this command restored.
    restored: Vec<ItemId>,
    /// Whether a record couldn't be written.
    failed: bool,
}

impl<'run> StoreJournal<'run> {
    /// The journal of `run` as read, appended after its sound records.
    fn after(run: &StoredRun, lock: &'run Locked, session: &Session<'run>) -> Self {
        Self {
            store: session.store,
            lock,
            run: run.id().clone(),
            at: run.sound,
            plan: run.plan().digest(),
            codec: session.codec,
            restored: Vec::new(),
            failed: false,
        }
    }
}

impl fmt::Debug for StoreJournal<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreJournal")
            .field("run", &self.run)
            .field("at", &self.at)
            .finish_non_exhaustive()
    }
}

impl Journal for StoreJournal<'_> {
    /// The run was stored with its plan when it was created: this only checks that
    /// `plan` is that plan.
    fn plan(&mut self, plan: &Plan) -> Result<(), ApplyError> {
        if plan.digest() == self.plan {
            Ok(())
        } else {
            Err(ApplyError::new(ErrorCode::Journal))
        }
    }

    fn record(&mut self, record: &Record) -> Result<(), ApplyError> {
        let line = encode_line(record, self.codec);
        match self.store.append(self.lock, &self.run, self.at, &line) {
            Ok(at) => {
                self.at = at;
                Ok(())
            }
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }
}
