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
use crate::integration::{
    Integration, IntegrationError, IntegrationId, Listed, Reread, ScanContext,
};
use crate::outcome::Skip;
use crate::planner::Unjudged;
use crate::policy::{Activity, ActivityGap, Facts, Hold, Unmet};
use crate::retention::{GroupGap, ReleaseState, Retained, Settlement};
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

    /// What `at.finding`'s integration reads again now, `None` when it reads nothing. One
    /// that fails or panics holds the item: an unread fact is never taken for a good one.
    fn revisit(&self, at: &Reading<'_>) -> Result<Option<Supplied>, Skip> {
        self.read_again(at, &|integration, cx| integration.revisit(cx, at.finding))
    }

    /// Everything `at.finding`'s integration reads again now that the finding is decided
    /// among, in one reading ([`Integration::revisit_group`]), with the same holds as
    /// [`revisit`].
    ///
    /// [`revisit`]: Self::revisit
    fn revisit_group(&self, at: &Reading<'_>) -> Result<Option<Vec<Reread>>, Skip> {
        self.read_again(at, &|integration, cx| {
            integration.revisit_group(cx, at.finding)
        })
    }

    /// What `read` gives through `at.finding`'s integration, with the context the apply
    /// scan opened for it. A failure or a panic holds the item.
    fn read_again<T>(
        &self,
        at: &Reading<'_>,
        read: &dyn Fn(&dyn Integration, &ScanContext<'_>) -> Result<T, IntegrationError>,
    ) -> Result<T, Skip> {
        let Reading {
            request,
            scan,
            finding,
            cancel,
        } = *at;
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
        let integration = registered.integration.as_ref();
        let done = panic::catch_unwind(AssertUnwindSafe(|| read(integration, &cx)));
        match done {
            Ok(Ok(done)) => Ok(done),
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
        if matches!(facts.supplied.subject, Subject::Release(_)) {
            return self.release_now(scan, finding, cancel);
        }
        let plain = matches!(facts.supplied.subject, Subject::Plain);
        let supplied = match self.engine.revisit(&self.reading(scan, finding, cancel))? {
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
    /// Where `finding` of `scan` is read again from.
    const fn reading<'run>(
        &'run self,
        scan: &'run ScanReport,
        finding: &'run Finding,
        cancel: &'run CancelToken,
    ) -> Reading<'run> {
        Reading {
            request: self.request,
            scan,
            finding,
            cancel,
        }
    }

    /// What the release `finding` of `scan` is now: its integration reads its component
    /// again in one reading, under the item's locks, and the release is settled among every
    /// entry that reading lists, judged by one new process snapshot, as a scan of the store
    /// at this moment settles it. A release this run removed is no longer listed, and one
    /// that appeared since counts. An entry the apply scan didn't measure as it is now (new,
    /// replaced, or listed without facts) leaves its group unsettled and its component's
    /// activity unknown. The release itself must be listed at its path, by its rule, with
    /// the identity the scan read, or it holds as `changed`; listed without facts, it is
    /// unsettled.
    fn release_now(
        &self,
        scan: &ScanReport,
        finding: &Finding,
        cancel: &CancelToken,
    ) -> Result<Now, Skip> {
        let facts = finding
            .facts
            .as_ref()
            .ok_or(Skip::Unjudged(Unjudged::Unmeasured))?;
        let group = self
            .engine
            .revisit_group(&self.reading(scan, finding, cancel))?
            .ok_or(Skip::RevisitMissing)?;
        let ours = group
            .iter()
            .find(|read| read.rule == finding.rule && read.path == finding.path)
            .filter(|_| self.unchanged(finding));
        let Some(ours) = ours else {
            return Ok(changed(facts));
        };
        let mut members = Vec::new();
        let mut unmeasured = Vec::new();
        for read in &group {
            let Subject::Release(release) = &read.supplied.subject else {
                continue;
            };
            let measured = scan.findings.iter().find(|found| {
                read.listed == Listed::Read
                    && found.integration == finding.integration
                    && found.rule == read.rule
                    && found.path == read.path
                    && found.facts.is_some()
                    && self.unchanged(found)
            });
            match measured {
                Some(found) => members.push(read_now(found, read)),
                None => unmeasured.push(release),
            }
        }
        let fresh = Fresh {
            processes: Processes::default(),
            identities: Identities::default(),
            taken: self.engine.ports.clock.now(),
        };
        for member in &mut members {
            let activity = self.engine.activity_now(&fresh, self.request, scan, member);
            if let Some(member_facts) = member.facts.as_mut() {
                member_facts.activity = activity;
            }
        }
        let scope = Scope {
            fs: self.engine.ports.fs.as_ref(),
            identities: &fresh.identities,
            home: self.engine.home(&self.request.roots),
            roots: &self.request.roots,
        };
        self.engine
            .settle_releases(&mut members, &unmeasured, &fresh.processes, scope);
        let current = members
            .into_iter()
            .find(|member| member.path == finding.path && member.rule == finding.rule);
        let Some((current, judged)) =
            current.and_then(|member| member.facts.clone().map(|judged| (member, judged)))
        else {
            return Ok(unsettled(facts, ours));
        };
        Ok(Now {
            unmet: self.engine.unmet_now(&current).unwrap_or(judged.unmet),
            activity: judged.activity,
            supplied: ours.supplied.clone(),
            settlement: judged.settlement,
        })
    }

    /// Whether the entry at `found`'s path is the one the apply scan read there: its
    /// identity is read again without following a link, and one that can't be read on
    /// either side is not the same.
    fn unchanged(&self, found: &Finding) -> bool {
        let then = found.facts.as_ref().and_then(|facts| facts.lineage.own());
        let now = self
            .engine
            .ports
            .fs
            .metadata(&found.path)
            .ok()
            .and_then(|meta| meta.identity);
        then.is_some() && then == now
    }

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

/// What a release that its integration no longer lists as it was is now: held as
/// `changed`, which keeps its whole component.
fn changed(facts: &Facts) -> Now {
    let mut supplied = facts.supplied.clone();
    if let Subject::Release(release) = &mut supplied.subject {
        release.state = ReleaseState::Blocked(GroupGap::Changed);
    }
    Now {
        activity: Activity::Unknown(ActivityGap::Unsupported),
        supplied,
        settlement: Settlement::Retained(Retained::GroupBlocked(GroupGap::Changed)),
        unmet: facts.unmet.clone(),
    }
}

/// One item read again: the request and scan that found it, and the run's cancel token.
#[derive(Clone, Copy)]
struct Reading<'run> {
    request: &'run ScanRequest,
    scan: &'run ScanReport,
    finding: &'run Finding,
    cancel: &'run CancelToken,
}

/// The apply scan's `found` with what its integration supplies for it now.
fn read_now(found: &Finding, read: &Reread) -> Finding {
    let mut member = found.clone();
    if let Some(facts) = member.facts.as_mut() {
        facts.supplied = read.supplied.clone();
    }
    member
}

/// What a release is now when it is still there as the scan read it, but the scan didn't
/// measure it as it is now or its facts can't be read: it is not settled.
fn unsettled(facts: &Facts, ours: &Reread) -> Now {
    Now {
        activity: Activity::Unknown(ActivityGap::Unsupported),
        supplied: ours.supplied.clone(),
        settlement: Settlement::Unsettled,
        unmet: facts.unmet.clone(),
    }
}
