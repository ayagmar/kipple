//! Applying a reviewed plan: revalidation, then intent, effect and outcome, item by item
//! (`docs/04-safety-model.md` §3.3, §3.6, §7; spec v2 pinned 2, 7, 8; v2.1 C, F, G, H).
//!
//! Each item, in plan order:
//!
//! 1. Its root is compared with the identity the scan that planned it recorded (G), before
//!    anything else.
//! 2. It is judged again from a scan run for this apply, through the same engine facts,
//!    protections and policy as at planning, under the current config: layout, activity
//!    and every other fact are fresh, never taken from the planning scan (H).
//! 3. It is frozen again, and [`admit`] makes its operation, or says why not: revalidation
//!    only subtracts (pinned 2).
//! 4. Its intent is made durable, then the operation runs, then its outcome is recorded
//!    (pinned 8, F). A journal write that fails stops the apply: nothing runs after it.
//!
//! Cancellation is checked between items, and by the executor between entries.

mod reconcile;

pub use reconcile::{Pending, Reconciled, reconcile};

use crate::authorize::{Authorization, AuthorizedOp, DeleteOp, Recheck, TrashOp, admit};
use crate::cancel::CancelToken;
use crate::engine::ScanReport;
use crate::journal::{Intent, Journal};
use crate::mutation::Finish;
use crate::outcome::{ErrorCode, ItemOutcome, Receipt, ReceiptItem, Skip, Stop, Tally};
use crate::plan::{Plan, PlanEntry, VerifiedRoot};
use crate::planner::{Judge, Judged, MutationPorts, Scene, Unjudged};
use crate::trash::TrashSlot;

/// What to apply.
#[derive(Debug, Clone, Copy)]
pub struct ApplyRequest<'run> {
    /// The plan, as reviewed.
    pub plan: &'run Plan,
    /// What the user authorized.
    pub authorization: Authorization,
    /// The report of a scan run for this apply, after the review: every item is judged
    /// again from it.
    pub rescan: &'run ScanReport,
}

/// Applies `request` under the current `judge`, through `ports`, recording each step in
/// `journal`. Every item of the plan ends in exactly one outcome, in plan order.
#[must_use]
pub fn apply(
    request: &ApplyRequest<'_>,
    judge: &Judge<'_>,
    ports: &MutationPorts<'_>,
    journal: &mut dyn Journal,
    cancel: &CancelToken,
) -> Receipt {
    let plan = request.plan;
    let mut stop = journal.plan(plan).err().map(|_| Stop::JournalFailed);
    let scene = Scene::new(request.rescan, judge);
    let run = Run {
        request,
        judge,
        ports,
        scene: &scene,
        cancel,
    };
    let mut receipt = Receipt::default();
    for entry in plan.entries() {
        if stop.is_none() && cancel.is_cancelled() {
            stop = Some(Stop::Cancelled);
        }
        let ran = stop.map_or_else(|| run.item(entry, journal), Ran::stopped);
        stop = ran.stop;
        let outcome = ran.outcome;
        receipt.items.push(ReceiptItem {
            item: entry.item(),
            path: entry.path().clone(),
            method: entry.method(),
            estimate: entry.estimate().copied(),
            outcome,
        });
    }
    receipt
}

/// What one apply shares across its items.
#[derive(Debug, Clone, Copy)]
struct Run<'run> {
    request: &'run ApplyRequest<'run>,
    judge: &'run Judge<'run>,
    ports: &'run MutationPorts<'run>,
    scene: &'run Scene<'run>,
    cancel: &'run CancelToken,
}

/// How one item ended, and whether the apply stops after it.
#[derive(Debug)]
struct Ran {
    outcome: ItemOutcome,
    stop: Option<Stop>,
}

impl Ran {
    const fn skipped(skip: Skip) -> Self {
        Self {
            outcome: ItemOutcome::Skipped(skip),
            stop: None,
        }
    }

    const fn stopped(why: Stop) -> Self {
        Self {
            outcome: ItemOutcome::Skipped(Skip::NotStarted(why)),
            stop: Some(why),
        }
    }
}

impl Run<'_> {
    /// Revalidates `entry`, then runs it between its intent and its outcome.
    fn item(&self, entry: &PlanEntry, journal: &mut dyn Journal) -> Ran {
        let op = match self.authorize(entry) {
            Ok(op) => op,
            Err(Skip::NotStarted(why)) => return Ran::stopped(why),
            Err(skip) => return Ran::skipped(skip),
        };
        let ran = match &op {
            AuthorizedOp::Delete(delete) => journal
                .intent(&self.intent(&op, None))
                .ok()
                .map(|()| self.delete(delete)),
            AuthorizedOp::Trash(trash) => match self.ports.trash.destination(trash) {
                Ok(slot) => journal
                    .intent(&self.intent(&op, Some(slot.clone())))
                    .ok()
                    .map(|()| self.trash(trash, &slot)),
                Err(error) => return Ran::skipped(Skip::TrashUnavailable(error)),
            },
        };
        let Some(outcome) = ran else {
            return Ran::stopped(Stop::JournalFailed);
        };
        let stop = if journal.outcome(entry.item(), &outcome).is_err() {
            Some(Stop::JournalFailed)
        } else {
            matches!(outcome, ItemOutcome::Cancelled(_)).then_some(Stop::Cancelled)
        };
        Ran { outcome, stop }
    }

    /// What `op` is about to do, with the trash `slot` it goes to.
    fn intent(&self, op: &AuthorizedOp, slot: Option<TrashSlot>) -> Intent {
        let target = op.target();
        Intent {
            plan: self.request.plan.digest(),
            item: target.item(),
            method: op.method(),
            reviewed: target.tree().digest(),
            authorized: target.digest(),
            entries: target.marks().authorized(),
            trash: slot,
        }
    }

    /// The operation revalidation allows for `entry` now, or why there is none.
    fn authorize(&self, entry: &PlanEntry) -> Result<AuthorizedOp, Skip> {
        let judged = self.current(entry)?;
        let freeze = self
            .ports
            .freezer
            .freeze(entry.anchor(), self.cancel)
            .map_err(|error| {
                if error.code == ErrorCode::Cancelled {
                    Skip::NotStarted(Stop::Cancelled)
                } else {
                    Skip::Freeze(error)
                }
            })?;
        let recheck = Recheck {
            verdict: &judged.verdict,
            proposal: &judged.proposal,
            judgement: &judged.judgement,
            freeze,
        };
        admit(
            self.request.plan,
            entry.item(),
            self.request.authorization,
            recheck,
        )
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

    /// Runs `op`, after its intent, and turns what it did into the item's outcome.
    fn delete(&self, op: &DeleteOp) -> ItemOutcome {
        let target = op.target();
        let mut tally = Tally::new(target.tree(), target.marks());
        let run = self.ports.executor.delete(op, &mut tally, self.cancel);
        tally.finish(run)
    }

    /// Moves `op`'s item to `slot`, after its intent. Nothing is removed: the item is
    /// done once it moved, and nothing moved when the move failed.
    fn trash(&self, op: &TrashOp, slot: &TrashSlot) -> ItemOutcome {
        let target = op.target();
        let tally = Tally::new(target.tree(), target.marks());
        match self.ports.trash.move_whole(op, slot, self.cancel) {
            Ok(trashed) => {
                let mut effect = tally.before_run();
                effect.trashed = Some(trashed);
                ItemOutcome::Done(effect)
            }
            Err(error) if error.code == ErrorCode::Cancelled => tally.finish(Ok(Finish::Cancelled)),
            Err(error) => tally.finish(Err(error)),
        }
    }
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
