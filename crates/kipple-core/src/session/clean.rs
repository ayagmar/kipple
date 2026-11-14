//! `clean` and `apply FILE`: planning, showing and confirming a plan, up to the shared
//! run in [`execute`].

use std::fmt;
use std::path::PathBuf;

use super::{Ending, EventSink, Finished, Session, execute};
use crate::authorize::{Authorization, ReviewedPlan};
use crate::cancel::CancelToken;
use crate::engine::ScanReport;
use crate::imported::{DroppedItem, ImportedPlan, ReplanRequest, replan};
use crate::outcome::Skip;
use crate::plan::{Plan, Selection};
use crate::planner::{Judge, Merged, PlanMode, PlanRequest, Refused, plan};
use crate::profile::ProfileGrant;

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
    /// `--profile NAME --yes`: the Safe items the profile's check granted, by delete or
    /// trash, authorized by that grant.
    Profile(ProfileGrant),
}

impl fmt::Debug for Consent<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Yes => "Yes",
            Self::Ask(_) => "Ask",
            Self::Profile(_) => "Profile",
        })
    }
}

impl Consent<'_> {
    /// What a plan confirmed this way may hold.
    const fn mode(&self) -> PlanMode {
        match self {
            Self::Yes => PlanMode::SafeOnly,
            Self::Ask(_) => PlanMode::Safe,
            Self::Profile(_) => PlanMode::SafeFiles,
        }
    }

    /// Who confirmed `review`'s plan, or how the command ends without running: declined
    /// when the person said no, cancelled when `cancel` was set meanwhile, whatever they
    /// answered.
    fn confirm(self, review: &Review<'_>, cancel: &CancelToken) -> Result<Confirmed, Ending> {
        let confirmed = match self {
            Self::Yes => Ok(Confirmed::Automation),
            Self::Profile(grant) => Ok(Confirmed::Profile(grant)),
            Self::Ask(confirm) => match confirm.confirm(review) {
                Answer::Yes => Ok(Confirmed::Person),
                Answer::No => Err(Ending::Declined),
            },
        };
        if cancel.is_cancelled() {
            return Err(Ending::Cancelled);
        }
        confirmed
    }
}

/// Who confirmed a [`Prepared`] clean, which decides what of it may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmed {
    /// A person who saw the prepared plan, at the prompt or in the TUI's review: exactly
    /// that plan, by its own digest ([`Authorization::Reviewed`]).
    Person,
    /// Automation (`--only` or every Safe finding, with `--yes`): its Safe deletes
    /// ([`Authorization::SafeOnly`]).
    Automation,
    /// A profile with `--yes`: the Safe items its grant admits
    /// ([`Authorization::Profile`]).
    Profile(ProfileGrant),
}

impl Confirmed {
    /// The authorization this confirmation gives `plan`.
    fn authorization(self, plan: &Plan) -> Authorization {
        match self {
            Self::Person => Authorization::Reviewed(ReviewedPlan::of(plan)),
            Self::Automation => Authorization::SafeOnly,
            Self::Profile(grant) => Authorization::Profile(grant),
        }
    }
}

/// A selected finding `clean` didn't plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedItem {
    /// Its path, as the scan reported it.
    pub path: PathBuf,
    /// Why it wasn't planned.
    pub skip: Skip,
}

impl RefusedItem {
    /// `refused`, each with its finding's path in `scan`, the scan it was planned from.
    #[must_use]
    pub fn list(scan: &ScanReport, refused: &[Refused]) -> Vec<Self> {
        refused
            .iter()
            .filter_map(|refused| {
                let finding = scan
                    .findings
                    .iter()
                    .find(|finding| finding.id == refused.finding)?;
                Some(Self {
                    path: finding.path.clone(),
                    skip: refused.skip.clone(),
                })
            })
            .collect()
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

/// A `clean` planned and not yet run: its plan, frozen, and what planning left out.
/// Made only by [`prepare_clean`]; a frontend shows it, has it confirmed and passes the
/// same value to [`run_prepared`].
#[derive(Debug)]
pub struct Prepared {
    plan: Plan,
    merged: Vec<Merged>,
    refused: Vec<Refused>,
    refused_items: Vec<RefusedItem>,
    mode: PlanMode,
    early: Option<Ending>,
}

impl Prepared {
    /// The plan. Confirming it as a person authorizes exactly this plan.
    #[must_use]
    pub const fn plan(&self) -> &Plan {
        &self.plan
    }

    /// Selected findings merged into another's item.
    #[must_use]
    pub fn merged(&self) -> &[Merged] {
        &self.merged
    }

    /// Selected findings not planned, and why.
    #[must_use]
    pub fn refused(&self) -> &[Refused] {
        &self.refused
    }

    /// Those findings with their paths in the scan, as [`Finished::refused`] lists them.
    #[must_use]
    pub fn refused_items(&self) -> &[RefusedItem] {
        &self.refused_items
    }

    /// Who it was planned for.
    #[must_use]
    pub const fn mode(&self) -> PlanMode {
        self.mode
    }

    /// How a clean of it ends without asking anyone: nothing to clean, none ran (every
    /// selected finding refused), or cancelled while planning. `None` when there is a
    /// plan to confirm.
    #[must_use]
    pub const fn ending(&self) -> Option<&Ending> {
        self.early.as_ref()
    }

    /// What a person is asked to confirm.
    #[must_use]
    pub fn review(&self) -> Review<'_> {
        Review {
            plan: &self.plan,
            merged: &self.merged,
            refused: &self.refused,
            dropped: &[],
        }
    }
}

/// Plans `request` under the session's judge for `mode`, without taking the lock or
/// writing anything. A frontend shows the result and has it confirmed.
#[must_use]
pub fn prepare_clean(
    request: &CleanRequest<'_>,
    session: &Session<'_>,
    mode: PlanMode,
    cancel: &CancelToken,
) -> Prepared {
    let planned = plan(
        &PlanRequest {
            scan: request.scan,
            selection: request.selection,
            mode,
        },
        &session.judge,
        session.ports.mutation,
        cancel,
    );
    let early = if cancel.is_cancelled() {
        Some(Ending::Cancelled)
    } else if planned.plan.entries().is_empty() {
        Some(if planned.refused.is_empty() {
            Ending::NothingToClean
        } else {
            Ending::NoneRan
        })
    } else {
        None
    };
    Prepared {
        refused_items: RefusedItem::list(request.scan, &planned.refused),
        plan: planned.plan,
        merged: planned.merged,
        refused: planned.refused,
        mode,
        early,
    }
}

/// Runs `prepared` as `confirmed` allows.
///
/// It takes the lock, reconciles, stores the run and applies it. A prepared clean with
/// nothing to confirm ends as it says ([`Prepared::ending`]); a cancel before the lock
/// ends it cancelled.
pub fn run_prepared(
    prepared: &Prepared,
    confirmed: Confirmed,
    session: &Session<'_>,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
) -> Finished {
    let refused = prepared.refused_items.clone();
    let early = if cancel.is_cancelled() {
        Some(Ending::Cancelled)
    } else {
        prepared.early.clone()
    };
    if let Some(ending) = early {
        return Finished {
            refused,
            ..Finished::before(ending)
        };
    }
    let authorization = confirmed.authorization(&prepared.plan);
    Finished {
        refused,
        ..execute(&prepared.plan, authorization, session, events, cancel)
    }
}

/// `clean`: plans `request` under the session's judge as `consent` allows, has it
/// confirmed, then runs it.
pub fn run_clean(
    request: &CleanRequest<'_>,
    session: &Session<'_>,
    consent: Consent<'_>,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
) -> Finished {
    let prepared = prepare_clean(request, session, consent.mode(), cancel);
    if prepared.early.is_some() {
        return run_prepared(&prepared, Confirmed::Automation, session, events, cancel);
    }
    match consent.confirm(&prepared.review(), cancel) {
        Ok(confirmed) => run_prepared(&prepared, confirmed, session, events, cancel),
        Err(ending) => Finished {
            refused: prepared.refused_items,
            ..Finished::before(ending)
        },
    }
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
    consent: Consent<'_>,
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
    let authorization = match consent.confirm(&review, cancel) {
        Ok(confirmed) => confirmed.authorization(&replanned.plan),
        Err(ending) => {
            return Finished {
                dropped: replanned.dropped,
                ..Finished::before(ending)
            };
        }
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
