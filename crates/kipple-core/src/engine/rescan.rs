//! What apply reads the machine through again: a scan of the roots configured now, and
//! what an item rests on right before it runs (spec M3b pinned 4, 5; M4 pinned 13).

use std::cell::RefCell;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

use super::{Engine, Finding, Identities, ScanReport, ScanRequest, project_dir};
use crate::activity::{ActivitySubject, Processes, Scope};
use crate::cancel::CancelToken;
use crate::discovery::Walks;
use crate::event::event_channel;
use crate::facts::{Evaluated, unmet};
use crate::integration::{IntegrationId, ScanContext};
use crate::outcome::Skip;
use crate::planner::Unjudged;
use crate::policy::{Activity, ActivityGap, Hold, Unmet};
use crate::retention::Settlement;
use crate::root::ResolvedRoot;
use crate::size::SpaceEstimate;
use crate::supplied::{Subject, Supplied};

/// How long one process snapshot serves the items after the first that used it.
pub const FRESH: Duration = Duration::from_secs(1);

/// What apply reads the machine through besides the mutation ports. [`Engine::rescan`]
/// gives one; tests fake it.
pub trait Rescan: fmt::Debug + Sync {
    /// Scans the roots configured now, for this apply. Apply calls it once, at its start;
    /// a scan is never passed in or kept from an earlier one.
    fn scan(&self, cancel: &CancelToken) -> ScanReport;

    /// What `finding` of `scan` rests on now, after its locks: its integration reads it
    /// again, then its activity and predicates are judged from that.
    ///
    /// Activity is judged the way the scan judges it (D-050, D-052; a partial listing is
    /// unknown). A finding that isn't [`Subject::Plain`] takes a new process snapshot, so
    /// none predates its lock; a plain one takes one under [`FRESH`] old.
    ///
    /// # Errors
    /// [`Skip::RevisitMissing`] when its integration reads nothing again and it isn't
    /// plain, and a hold when that reading failed: the set of items only shrinks.
    fn now(&self, scan: &ScanReport, finding: &Finding, cancel: &CancelToken) -> Result<Now, Skip>;

    /// What `path` holds now, `None` when it can't be measured at all.
    fn size(&self, path: &Path, cancel: &CancelToken) -> Option<SpaceEstimate>;
}

/// What an item rests on right before it runs.
#[derive(Debug, Clone)]
pub struct Now {
    /// Whether a program uses it.
    pub activity: Activity,
    /// What its integration read.
    pub supplied: Supplied,
    /// What retention decides for it.
    pub settlement: Settlement,
    /// Every predicate of its rule it doesn't satisfy now.
    pub unmet: Vec<Unmet>,
}

/// How old a process snapshot may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Snapshot {
    /// Under [`FRESH`] old.
    Recent,
    /// Taken now.
    New,
}

/// The engine's [`Rescan`] for one apply.
#[derive(Debug)]
pub struct EngineRescan<'engine> {
    engine: &'engine Engine,
    request: &'engine ScanRequest,
    fresh: Mutex<Option<Fresh>>,
}

/// One process snapshot, with the identities read for it and when it was taken.
#[derive(Debug)]
struct Fresh {
    processes: Processes,
    identities: Identities,
    taken: SystemTime,
}

impl Engine {
    /// What one apply reads the machine through: scans of `request`'s roots, and the
    /// processes through this engine's ports.
    #[must_use]
    pub const fn rescan<'engine>(
        &'engine self,
        request: &'engine ScanRequest,
    ) -> EngineRescan<'engine> {
        EngineRescan {
            engine: self,
            request,
            fresh: Mutex::new(None),
        }
    }

    /// What `finding`'s integration reads again now, `None` when it reads nothing. One that
    /// fails or panics holds the item: an unread fact is never taken for a good one.
    fn revisit(
        &self,
        request: &ScanRequest,
        scan: &ScanReport,
        finding: &Finding,
        cancel: &CancelToken,
    ) -> Result<Option<Supplied>, Skip> {
        let unread = || Skip::Protected(vec![Hold::FactUnknown]);
        let registered = self
            .integrations
            .iter()
            .find(|registered| registered.descriptor.id == finding.integration)
            .ok_or_else(unread)?;
        let walks = Walks::new(&request.exclude);
        let cx = ScanContext {
            roots: &opened_roots(request, scan, finding.integration),
            rules: &registered.rules,
            fs: self.ports.fs.as_ref(),
            tools: self.ports.tools.as_ref(),
            worktrees: self.ports.worktrees.as_ref(),
            cancel,
            plan: &self.plan,
            walks: &walks,
            walked: RefCell::default(),
        };
        let read = panic::catch_unwind(AssertUnwindSafe(|| {
            registered.integration.revisit(&cx, finding)
        }));
        match read {
            Ok(Ok(supplied)) => Ok(supplied),
            Ok(Err(_)) | Err(_) => Err(unread()),
        }
    }

    /// Every predicate of `finding`'s rule it doesn't satisfy, where its facts are as they
    /// are now. Its own list stands when its rule can't be found.
    fn unmet_now(&self, finding: &Finding) -> Option<Vec<Unmet>> {
        let facts = finding.facts.as_ref()?;
        let registered = self
            .integrations
            .iter()
            .find(|registered| registered.descriptor.id == finding.integration)?;
        let rule = registered
            .rules
            .iter()
            .find(|rule| rule.id == finding.rule)?;
        let evaluated = Evaluated {
            evidence: &finding.evidence,
            size: finding.size,
            facts,
        };
        Some(unmet(rule, registered.descriptor.facts, evaluated))
    }

    /// Whether a program uses `finding` of `scan`, from `fresh`'s processes.
    fn activity_now(
        &self,
        fresh: &Fresh,
        request: &ScanRequest,
        scan: &ScanReport,
        finding: &Finding,
    ) -> Activity {
        let (Some(registered), Some(facts)) = (
            self.integrations
                .iter()
                .find(|registered| registered.descriptor.id == finding.integration),
            finding.facts.as_ref(),
        ) else {
            // Apply judges only a measured finding of this engine's scan: anything else
            // can't be shown idle.
            return Activity::Unknown(ActivityGap::Unsupported);
        };
        let scope = Scope {
            fs: self.ports.fs.as_ref(),
            identities: &fresh.identities,
            home: self.home(&request.roots),
            roots: &request.roots,
        };
        let roots = scan
            .integrations
            .iter()
            .filter(|report| report.id == finding.integration)
            .flat_map(|report| &report.opened)
            .map(|opened| opened.path.as_path());
        let project = project_dir(finding, roots);
        let subject = ActivitySubject {
            path: &finding.path,
            lineage: &facts.lineage,
            owner: &registered.descriptor,
            evidence: facts.supplied.activity,
            project: &project,
        };
        fresh
            .processes
            .activity(self.ports.processes.as_ref(), scope, subject)
    }
}

impl Rescan for EngineRescan<'_> {
    fn scan(&self, cancel: &CancelToken) -> ScanReport {
        let channel = event_channel(256, cancel);
        let (tx, rx) = (channel.tx, channel.rx);
        thread::scope(|scope| {
            // Nothing shows this scan's events: they are drained so it never waits.
            scope.spawn(move || rx.for_each(drop));
            let report = self.engine.scan(self.request, &tx, cancel);
            drop(tx);
            report
        })
    }

    fn now(&self, scan: &ScanReport, finding: &Finding, cancel: &CancelToken) -> Result<Now, Skip> {
        let facts = finding
            .facts
            .as_ref()
            .ok_or(Skip::Unjudged(Unjudged::Unmeasured))?;
        let plain = matches!(facts.supplied.subject, Subject::Plain);
        let supplied = match self.engine.revisit(self.request, scan, finding, cancel)? {
            Some(supplied) => supplied,
            None if plain => facts.supplied.clone(),
            None => return Err(Skip::RevisitMissing),
        };
        let snapshot = if plain && matches!(supplied.subject, Subject::Plain) {
            Snapshot::Recent
        } else {
            Snapshot::New
        };
        let mut current = finding.clone();
        let mut judged = facts.clone();
        judged.supplied = supplied;
        current.facts = Some(judged.clone());
        judged.activity = self.activity(scan, &current, snapshot);
        // Nothing decides a release yet, which holds it.
        judged.settlement = Settlement::Unsettled;
        current.facts = Some(judged.clone());
        Ok(Now {
            unmet: self.engine.unmet_now(&current).unwrap_or(judged.unmet),
            activity: judged.activity,
            supplied: judged.supplied,
            settlement: judged.settlement,
        })
    }

    fn size(&self, path: &Path, cancel: &CancelToken) -> Option<SpaceEstimate> {
        let measured = self.engine.ports.sizer.size(path, cancel).ok()?;
        Some(measured.size)
    }
}

impl EngineRescan<'_> {
    /// Whether a program uses `finding` of `scan` now, from a snapshot of that age.
    fn activity(&self, scan: &ScanReport, finding: &Finding, snapshot: Snapshot) -> Activity {
        let now = self.engine.ports.clock.now();
        let mut fresh = self.fresh.lock().unwrap_or_else(PoisonError::into_inner);
        let young = snapshot == Snapshot::Recent
            && fresh
                .as_ref()
                .is_some_and(|fresh| now.duration_since(fresh.taken).is_ok_and(|age| age < FRESH));
        if !young {
            *fresh = Some(Fresh {
                processes: Processes::default(),
                identities: Identities::default(),
                taken: now,
            });
        }
        fresh
            .as_ref()
            .map_or(Activity::Unknown(ActivityGap::Unsupported), |fresh| {
                self.engine.activity_now(fresh, self.request, scan, finding)
            })
    }
}

/// The roots `scan` opened for `integration`, as `request` resolved them.
fn opened_roots(
    request: &ScanRequest,
    scan: &ScanReport,
    integration: IntegrationId,
) -> Vec<ResolvedRoot> {
    scan.integrations
        .iter()
        .filter(|report| report.id == integration)
        .flat_map(|report| &report.opened)
        .flat_map(|opened| {
            request
                .roots
                .all(opened.root)
                .iter()
                .flatten()
                .filter(|resolved| resolved.path == opened.path)
        })
        .cloned()
        .collect()
}
