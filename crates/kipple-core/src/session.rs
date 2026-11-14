//! Mutating commands from start to end: `clean`, `apply FILE` and `restore` (spec M3b
//! pinned 1, 2, 10, 12, 17; D-067).
//!
//! Each one plans, has the plan confirmed, then under the store's lock reconciles every
//! run a crash left pending, stores a new run and applies it, telling its frontend each
//! step as an [`ApplyEvent`]. The lock is taken only after confirmation, so a prompt
//! never keeps a scheduled `clean` waiting, and apply scans again under it anyway.
//!
//! How the plan is confirmed decides what it may hold: `--yes` plans and authorizes Safe
//! deletes only ([`Authorization::SafeOnly`]); a profile with `--yes` authorizes the Safe
//! items its grant admits ([`Authorization::Profile`]); a person who answers the prompt
//! or the TUI's review authorizes exactly the plan shown, by its digest
//! ([`Authorization::Reviewed`]).
//!
//! `clean` is [`prepare_clean`] then [`run_prepared`], so the plan confirmed is the plan
//! run: a [`Prepared`] is made only by planning and holds its plan unchanged, and
//! [`Confirmed::Person`] authorizes that plan's own digest. Whatever changes on disk while
//! it waits is apply's revalidation to catch, which only subtracts.
//!
//! ```compile_fail,E0451
//! // A frontend can't make a prepared clean without planning one.
//! fn forge(plan: kipple_core::Plan) -> kipple_core::Prepared {
//!     kipple_core::Prepared { plan, merged: Vec::new(), refused: Vec::new(),
//!         refused_items: Vec::new(), mode: kipple_core::PlanMode::Review, early: None }
//! }
//! ```

mod clean;
mod events;
mod restore;

pub use clean::{
    Answer, ApplyFile, CleanRequest, Confirm, Confirmed, Consent, Prepared, RefusedItem, Review,
    prepare_clean, run_apply, run_clean, run_prepared,
};
pub use events::{ApplyEvent, EventSink};
pub use restore::{RestoreAsk, RestoreFinished, Restored, Restoring, run_restore};

use std::fmt;

use crate::apply::{ApplyPorts, ApplyRequest, ItemEvent, ItemSink, Recorders, apply, reconcile};
use crate::authorize::Authorization;
use crate::cancel::CancelToken;
use crate::digest::Digest;
use crate::format::{encode_line, encode_stored_plan};
use crate::frozen::NameCodec;
use crate::imported::DroppedItem;
use crate::journal::{Journal, Record};
use crate::outcome::{ApplyError, ErrorCode, ItemOutcome, Receipt, Skip, Status, Stop};
use crate::plan::{ItemId, Plan};
use crate::planner::Judge;
use crate::profile::ProfileRun;
use crate::run::{Depth, RunRead, StoredRun, read_run};
use crate::store::{Locked, RunId, RunStore, StoreRefusal};

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

/// How a mutating command ended, which decides its exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// Every item was done or skipped, and at least one was done, or a restore moved an
    /// item back and none failed. Exit 0.
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
    /// For `clean`, the selected findings planning refused, with why; empty for
    /// `apply FILE`, whose refusals are among `dropped`.
    pub refused: Vec<RefusedItem>,
    /// For `clean --profile`, the profile it ran and what of it was left out; the
    /// frontend sets it from the profile's check.
    pub profile: Option<ProfileRun>,
}

impl Finished {
    fn before(ending: Ending) -> Self {
        Self {
            ending,
            run: None,
            receipt: Receipt::default(),
            dropped: Vec::new(),
            refused: Vec::new(),
            profile: None,
        }
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
    // stored run doesn't say so, and the next command reconciles it. A cancel no item
    // noticed (it came during the last one, or killed a command an item asked) still
    // cancels the command, unless something failed.
    let ran = Ending::of(&receipt);
    let ending = if journal.failed || ran == Ending::Failed {
        Ending::Failed
    } else if cancel.is_cancelled() {
        Ending::Cancelled
    } else {
        ran
    };
    Finished {
        ending,
        run: Some(run),
        receipt,
        dropped: Vec::new(),
        refused: Vec::new(),
        profile: None,
    }
}

/// Takes the lock and reconciles every run a crash left pending. Cancelling stops it,
/// leaving the rest pending.
fn prepare(
    session: &Session<'_>,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
) -> Result<Locked, Ending> {
    let lock = session.store.lock(cancel).map_err(|refusal| {
        // A cancel ends the wait for another's lock: the command was cancelled.
        if refusal == StoreRefusal::Busy && cancel.is_cancelled() {
            Ending::Cancelled
        } else {
            Ending::Refused(refusal)
        }
    })?;
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

/// Tells `events` each item event of `run`.
struct RunEvents<'events> {
    run: RunId,
    events: &'events mut dyn EventSink,
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
