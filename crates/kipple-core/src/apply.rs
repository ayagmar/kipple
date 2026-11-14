//! Applying a reviewed plan: revalidation, then intent, effect and outcome, item by item
//! (`docs/04-safety-model.md` §3.3, §3.6, §7; spec M3a pinned 2, 7, 8; M3b pinned 3–5).
//!
//! Apply first scans the roots configured now, through its [`Rescan`]: a scan is never
//! passed in or kept from the review. Then each item, in plan order:
//!
//! 1. Its root is compared with the identity the scan that planned it recorded (G), before
//!    anything else.
//! 2. It is judged again from that scan, through the same engine facts, protections and
//!    policy as at planning, under the current config: layout, git and every other fact
//!    are fresh, never taken from the planning scan (H).
//! 3. A release's tool locks are taken, without waiting. They are held until the item's
//!    outcome is recorded.
//! 4. It is frozen again, and git is asked again which entries of the fresh tree it
//!    tracks, which are left out.
//! 5. [`Rescan::now`] reads again what it rests on, and judges its activity and predicates
//!    from that: in use or unknown skips it as Protected.
//! 6. [`admit`] makes its operation, or says why not: revalidation only subtracts.
//! 7. Its intent is made durable, then the operation runs, the platform makes its effect
//!    durable, and its outcome is recorded (pinned 8, F).
//!
//! Every item that ends gets an outcome record, skips included. A journal write that fails
//! stops the apply: nothing runs after it. Cancellation is checked between items, and by
//! the executor between entries.

mod reconcile;

pub use reconcile::{Reconciled, reconcile};

use std::cell::RefCell;

use crate::authorize::{Authorization, AuthorizedOp, DeleteOp, Recheck, TrashOp, admit};
use crate::cancel::CancelToken;
use crate::engine::{Rescan, ScanReport};
use crate::journal::{Intent, Journal, Record};
use crate::mutation::{Finish, Freeze};
use crate::outcome::{
    ApplyError, Effect, ErrorCode, ItemOutcome, Receipt, ReceiptItem, Skip, Stop, Tally,
};
use crate::plan::{ItemId, ItemMethod, Plan, PlanEntry, VerifiedRoot};
use crate::planner::{Judge, Judged, MutationPorts, Scene, Unjudged, untracked};
use crate::policy::{Caution, Verdict};
use crate::store_lock::{HeldLocks, LockGap};
use crate::supplied::Subject;
use crate::trash::TrashSlot;

/// What to apply.
#[derive(Debug, Clone)]
pub struct ApplyRequest<'run> {
    /// The plan, as reviewed.
    pub plan: &'run Plan,
    /// What the user authorized.
    pub authorization: Authorization,
}

/// What apply tells its frontend as it goes, in the order it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemEvent {
    /// The item's intent is durable and its operation starts.
    Started {
        /// The item.
        item: ItemId,
    },
    /// The item ended, with this outcome, which is recorded.
    Ended {
        /// The item.
        item: ItemId,
        /// How it ended.
        outcome: Box<ItemOutcome>,
    },
}

/// Where apply reports each item as it goes.
pub trait ItemSink {
    /// `event` happened.
    fn item(&mut self, event: ItemEvent);
}

/// Ignores every event.
impl ItemSink for () {
    fn item(&mut self, _: ItemEvent) {}
}

/// Where one apply records and reports each step.
pub struct Recorders<'run> {
    /// Records each step durably, before the next.
    pub journal: &'run mut dyn Journal,
    /// Hears each item start and end.
    pub events: &'run mut dyn ItemSink,
}

impl std::fmt::Debug for Recorders<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorders")
            .field("journal", &self.journal)
            .finish_non_exhaustive()
    }
}

/// What one apply reads, changes and records the machine through.
#[derive(Debug, Clone, Copy)]
pub struct ApplyPorts<'run> {
    /// Scans the configured roots again and looks at the processes again.
    pub rescan: &'run dyn Rescan,
    /// Freezes, deletes, trashes and asks git.
    pub mutation: &'run MutationPorts<'run>,
}

/// Applies `request` under the current `judge`, through `ports`, recording each step in
/// the journal and telling the events of `to`. Every item of the plan ends in exactly one
/// outcome, in plan order.
#[must_use]
pub fn apply(
    request: &ApplyRequest<'_>,
    judge: &Judge<'_>,
    ports: &ApplyPorts<'_>,
    to: Recorders<'_>,
    cancel: &CancelToken,
) -> Receipt {
    let Recorders { journal, events } = to;
    let plan = request.plan;
    if journal.plan(plan).is_err() {
        return stopped_before(plan, events);
    }
    let scan = ports.rescan.scan(cancel);
    let scene = Scene::new(&scan, judge);
    let mut stop = None;
    let run = Run {
        request,
        judge,
        ports,
        scene: &scene,
        cancel,
        home_trash: RefCell::new(None),
    };
    let mut receipt = Receipt::default();
    for entry in plan.entries() {
        if stop.is_none() && cancel.is_cancelled() {
            stop = Some(Stop::Cancelled);
        }
        let ran = match stop {
            None => run.item(entry, journal, events),
            Some(why) => Ran::stopped(entry, why, journal),
        };
        stop = ran.stop;
        events.item(ItemEvent::Ended {
            item: entry.item(),
            outcome: Box::new(ran.outcome.clone()),
        });
        receipt.items.push(ReceiptItem {
            item: entry.item(),
            path: entry.path().clone(),
            method: entry.method(),
            estimate: entry.estimate().copied(),
            outcome: ran.outcome,
            restored: None,
        });
    }
    receipt
}

/// The receipt of `plan` when its plan couldn't be stored: nothing started.
fn stopped_before(plan: &Plan, events: &mut dyn ItemSink) -> Receipt {
    let mut receipt = Receipt::default();
    for entry in plan.entries() {
        let outcome = ItemOutcome::Skipped(Skip::NotStarted(Stop::JournalFailed));
        events.item(ItemEvent::Ended {
            item: entry.item(),
            outcome: Box::new(outcome.clone()),
        });
        receipt.items.push(ReceiptItem {
            item: entry.item(),
            path: entry.path().clone(),
            method: entry.method(),
            estimate: entry.estimate().copied(),
            outcome,
            restored: None,
        });
    }
    receipt
}

/// An item's operation, with the locks it runs under: they go when this does.
#[derive(Debug)]
struct Admitted {
    op: AuthorizedOp,
    _locks: Option<HeldLocks>,
}

/// What one apply shares across its items.
#[derive(Debug)]
struct Run<'run> {
    request: &'run ApplyRequest<'run>,
    judge: &'run Judge<'run>,
    ports: &'run ApplyPorts<'run>,
    scene: &'run Scene<'run>,
    cancel: &'run CancelToken,
    /// Whether the home trash was prepared, once a trash item first needed it.
    home_trash: RefCell<Option<Result<(), ApplyError>>>,
}

/// An operation with what it needs to run.
#[derive(Debug)]
enum Ready<'op> {
    Delete(&'op DeleteOp),
    Trash(&'op TrashOp, TrashSlot),
}

/// How one item ended, and whether the apply stops after it.
#[derive(Debug)]
struct Ran {
    outcome: ItemOutcome,
    stop: Option<Stop>,
}

impl Ran {
    /// `entry` skipped for `skip`, which is recorded. A skip that stops the apply stops
    /// it, and so does a record that can't be written.
    fn skipped(entry: &PlanEntry, skip: Skip, journal: &mut dyn Journal) -> Self {
        let stop = if let Skip::NotStarted(why) = &skip {
            Some(*why)
        } else {
            None
        };
        let outcome = ItemOutcome::Skipped(skip);
        let record = Record::Outcome {
            item: entry.item(),
            outcome: outcome.clone(),
        };
        let stop = if journal.record(&record).is_err() {
            Some(Stop::JournalFailed)
        } else {
            stop
        };
        Self { outcome, stop }
    }

    /// `entry` not started because the apply stopped for `why`. Recorded unless the
    /// journal is what failed.
    fn stopped(entry: &PlanEntry, why: Stop, journal: &mut dyn Journal) -> Self {
        let skip = Skip::NotStarted(why);
        match why {
            Stop::JournalFailed | Stop::Interrupted => Self {
                outcome: ItemOutcome::Skipped(skip),
                stop: Some(why),
            },
            Stop::Cancelled => Self::skipped(entry, skip, journal),
        }
    }
}

impl Run<'_> {
    const fn mutation(&self) -> &MutationPorts<'_> {
        self.ports.mutation
    }

    /// Revalidates `entry`, then runs it between its intent and its outcome.
    fn item(&self, entry: &PlanEntry, journal: &mut dyn Journal, events: &mut dyn ItemSink) -> Ran {
        if entry.method() == ItemMethod::Trash {
            // Its result is read again when the slot is chosen.
            self.prepare_home().ok();
        }
        let admitted = match self.authorize(entry) {
            Ok(admitted) => admitted,
            Err(skip) => return Ran::skipped(entry, skip, journal),
        };
        let op = &admitted.op;
        let ready = match op {
            AuthorizedOp::Delete(delete) => Ready::Delete(delete),
            AuthorizedOp::Trash(trash) => match self.destination(trash) {
                Ok(slot) => Ready::Trash(trash, slot),
                Err(error) => return Ran::skipped(entry, Skip::TrashUnavailable(error), journal),
            },
        };
        let slot = match &ready {
            Ready::Delete(_) => None,
            Ready::Trash(_, slot) => Some(slot.clone()),
        };
        if journal
            .record(&Record::Intent(self.intent(op, slot)))
            .is_err()
        {
            return Ran {
                outcome: ItemOutcome::Skipped(Skip::NotStarted(Stop::JournalFailed)),
                stop: Some(Stop::JournalFailed),
            };
        }
        events.item(ItemEvent::Started { item: entry.item() });
        let outcome = match ready {
            Ready::Delete(delete) => self.delete(delete),
            Ready::Trash(trash, to) => self.trash(trash, &to),
        };
        let record = Record::Outcome {
            item: entry.item(),
            outcome: outcome.clone(),
        };
        let stop = if journal.record(&record).is_err() {
            Some(Stop::JournalFailed)
        } else {
            matches!(outcome, ItemOutcome::Cancelled(_)).then_some(Stop::Cancelled)
        };
        Ran { outcome, stop }
    }

    /// Prepares the home trash once, for the first trash item: created when it is
    /// missing, under the lock, before that item's revalidation, so its activity
    /// snapshot is still fresh when its intent is written (pinned 4, 11).
    fn prepare_home(&self) -> Result<(), ApplyError> {
        let trash = self.mutation().trash;
        self.home_trash
            .borrow_mut()
            .get_or_insert_with(|| trash.prepare_home())
            .clone()
    }

    /// The slot `op` will move to. A trash that couldn't be prepared leaves the slot to
    /// say why.
    fn destination(&self, op: &TrashOp) -> Result<TrashSlot, ApplyError> {
        let prepared = self.prepare_home();
        self.mutation()
            .trash
            .destination(op)
            .map_err(|error| prepared.err().unwrap_or(error))
    }

    /// What `op` is about to do, with the trash `slot` it goes to.
    fn intent(&self, op: &AuthorizedOp, slot: Option<TrashSlot>) -> Intent {
        let target = op.target();
        Intent {
            plan: self.request.plan.digest(),
            item: target.item(),
            method: op.method().into(),
            reviewed: target.tree().digest(),
            authorized: target.digest(),
            entries: target.marks().authorized(),
            trash: slot,
        }
    }

    /// The operation revalidation allows for `entry` now, or why there is none.
    fn authorize(&self, entry: &PlanEntry) -> Result<Admitted, Skip> {
        let judged = self.current(entry)?;
        let locks = self.locks(&judged)?;
        let freeze = self.freeze(&judged)?;
        let verdict = self.active(&judged)?;
        let recheck = Recheck {
            verdict: &verdict,
            proposal: &judged.proposal,
            judgement: &judged.judgement,
            own_git: judged.own_git(),
            freeze,
        };
        let op = admit(
            self.request.plan,
            entry.item(),
            &self.request.authorization,
            recheck,
        )?;
        Ok(Admitted { op, _locks: locks })
    }

    /// The tool's locks a release is removed under. It is removed under all of them or
    /// not at all, and a release that names none has nothing to hold them by.
    fn locks(&self, judged: &Judged<'_>) -> Result<Option<HeldLocks>, Skip> {
        let Subject::Release(release) = &judged.facts.supplied.subject else {
            return Ok(None);
        };
        if release.locks.is_empty() {
            return Err(Skip::InstallerBusy(LockGap::Missing));
        }
        self.mutation()
            .locks
            .try_hold(&release.locks)
            .map(Some)
            .map_err(Skip::InstallerBusy)
    }

    /// `entry`'s finding, judged again from the scan run for this apply. Its root comes
    /// first: it must have opened with the identity the plan recorded (G).
    fn current(&self, entry: &PlanEntry) -> Result<Judged<'_>, Skip> {
        let root = &entry.anchor().root;
        if !opened_unchanged(self.scene.scan(), root) {
            return Err(Skip::RootChanged);
        }
        let index = self
            .scene
            .find(entry.path(), &entry.judgement().rule)
            .ok_or(Skip::Unjudged(Unjudged::NotFound))?;
        let judged = self.scene.judge(index, self.judge)?;
        if judged.anchor.root != *root {
            return Err(Skip::RootChanged);
        }
        Ok(judged)
    }

    /// `judged`'s target frozen now, with what git tracks in it now left out.
    fn freeze(&self, judged: &Judged<'_>) -> Result<Freeze, Skip> {
        let mut freeze = self
            .mutation()
            .freezer
            .freeze(&judged.anchor, self.cancel)
            .map_err(|error| {
                if error.code == ErrorCode::Cancelled {
                    Skip::NotStarted(Stop::Cancelled)
                } else {
                    Skip::Freeze(error)
                }
            })?;
        for id in untracked(judged, &freeze.tree, self.mutation(), self.cancel)? {
            freeze.tree.exclude(id);
        }
        Ok(freeze)
    }

    /// `judged`'s verdict with what it rests on read again now: in use or unknown holds it
    /// (pinned 4), and whatever else that reading gives it counts with its verdict.
    fn active(&self, judged: &Judged<'_>) -> Result<Verdict, Skip> {
        let finding = judged.finding;
        let now = self
            .ports
            .rescan
            .now(self.scene.scan(), finding, self.cancel)?;
        let mut facts = judged.facts.clone();
        facts.activity = now.activity;
        facts.supplied = now.supplied;
        facts.settlement = now.settlement;
        facts.unmet = now.unmet;
        match self
            .judge
            .policy
            .verdict(&finding.rule, &finding.path, &facts)
        {
            Verdict::Protected(holds) => Err(Skip::Protected(holds)),
            Verdict::Safe => Ok(judged.verdict.clone()),
            Verdict::Review(cautions) => Ok(with_cautions(&judged.verdict, cautions)),
        }
    }

    /// Runs `op`, after its intent, and turns what it did into the item's outcome.
    fn delete(&self, op: &DeleteOp) -> ItemOutcome {
        let target = op.target();
        let mut tally = Tally::new(target.tree(), target.marks());
        let run = self.mutation().executor.delete(op, &mut tally, self.cancel);
        tally.finish(run)
    }

    /// Moves `op`'s item to `slot`, after its intent. Nothing is removed: the item is
    /// done once it moved, and nothing moved when the move failed.
    fn trash(&self, op: &TrashOp, slot: &TrashSlot) -> ItemOutcome {
        let target = op.target();
        let tally = Tally::new(target.tree(), target.marks());
        match self.mutation().trash.move_whole(op, slot, self.cancel) {
            Ok(trashed) => {
                let mut effect = tally.before_run();
                effect.trashed = Some(trashed);
                ItemOutcome::Done(Effect::Files(effect))
            }
            Err(error) if error.code == ErrorCode::Cancelled => tally.finish(Ok(Finish::Cancelled)),
            Err(error) => tally.finish(Err(error)),
        }
    }
}

/// `verdict` with `cautions` too, each once.
fn with_cautions(verdict: &Verdict, mut cautions: Vec<Caution>) -> Verdict {
    if let Verdict::Review(standing) = verdict {
        cautions.extend(standing.iter().copied());
    }
    cautions.sort_unstable();
    cautions.dedup();
    Verdict::Review(cautions)
}

/// Whether `scan` opened `root` with the identity the plan recorded.
/// Every time it opened, and it opened at least once.
fn opened_unchanged(scan: &ScanReport, root: &VerifiedRoot) -> bool {
    let mut opened = scan
        .integrations
        .iter()
        .flat_map(|report| &report.opened)
        .filter(|opened| opened.root == root.root && opened.path == root.path)
        .peekable();
    opened.peek().is_some() && opened.all(|opened| opened.identity == Some(root.identity))
}
