//! What apply reads the machine through again: a scan of the roots configured now, and a
//! look at the processes right before each item runs (spec M3b pinned 4, 5).

use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

use super::{Engine, Finding, Identities, ScanReport, ScanRequest, project_dir};
use crate::activity::{Processes, Scope, Subject};
use crate::cancel::CancelToken;
use crate::event::event_channel;
use crate::policy::{Activity, ActivityGap};
use crate::probe::FileIdentity;

/// How long one process snapshot serves the items after the first that used it.
pub const FRESH: Duration = Duration::from_secs(1);

/// What apply reads the machine through besides the mutation ports. [`Engine::rescan`]
/// gives one; tests fake it.
pub trait Rescan: fmt::Debug + Sync {
    /// Scans the roots configured now, for this apply. Apply calls it once, at its start;
    /// a scan is never passed in or kept from an earlier one.
    fn scan(&self, cancel: &CancelToken) -> ScanReport;

    /// Whether a program uses `finding` of `scan` now. Judged the way the scan judges it
    /// (D-050, D-052; a partial listing is unknown), from a process snapshot taken less
    /// than [`FRESH`] ago.
    fn activity(&self, scan: &ScanReport, finding: &Finding) -> Activity;
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

    /// Whether a program uses `finding` of `scan`, from `fresh`'s processes.
    fn activity_now(
        &self,
        fresh: &Fresh,
        home: Option<FileIdentity>,
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
            home,
        };
        let roots = scan
            .integrations
            .iter()
            .filter(|report| report.id == finding.integration)
            .flat_map(|report| &report.opened)
            .map(|opened| opened.path.as_path());
        let project = project_dir(finding, roots);
        let subject = Subject {
            path: &finding.path,
            lineage: &facts.lineage,
            evidence: registered.evidence,
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

    fn activity(&self, scan: &ScanReport, finding: &Finding) -> Activity {
        let now = self.engine.ports.clock.now();
        let mut fresh = self.fresh.lock().unwrap_or_else(PoisonError::into_inner);
        let young = fresh
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
                let home = self.engine.home(&self.request.roots);
                self.engine.activity_now(fresh, home, scan, finding)
            })
    }
}
