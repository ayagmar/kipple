//! The engine every frontend calls (`docs/05-architecture.md` §2). Scanning is
//! read-only: integrations propose findings, the engine sizes them and reports.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::thread;

use crate::cancel::CancelToken;
use crate::discovery::{WalkPlan, Walks};
use crate::event::{EventTx, ScanEvent, Stopped};
use crate::facts::{Processes, Repositories};
use crate::integration::{
    Advice, Evidence, FindingSink, Integration, IntegrationDescriptor, IntegrationError,
    IntegrationId, ScanContext,
};
use crate::policy::{Consequence, Facts, Level, Modified, Proposal, Verdict, classify};
use crate::probe::{Clock, EntryKind, EntryMeta, FsProbe, ProbeError, Sizer};
use crate::process::ProcessProbe;
use crate::root::{ResolvedRoot, Roots, SymbolicRoot, UnresolvedRoot};
use crate::rule::{Category, Lens, Operation, Rule, RuleId};
use crate::size::{Completeness, Measurement, SpaceEstimate};

/// A finding's ID within one scan. IDs are references, never authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FindingId(pub u64);

impl fmt::Display for FindingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Something an integration found.
#[derive(Debug, Clone)]
pub struct Finding {
    /// Its ID within this scan.
    pub id: FindingId,
    /// The integration that found it.
    pub integration: IntegrationId,
    /// The rule it was found by.
    pub rule: RuleId,
    /// What it is, from the rule.
    pub category: Category,
    /// Where it is.
    pub path: PathBuf,
    /// Why the integration thinks it is what the rule says.
    pub evidence: Vec<Evidence>,
    /// What removing it would free, once sized.
    pub size: Option<SpaceEstimate>,
    /// What the host learned about it once it was measured, which its bucket is derived
    /// from ([`classify`](crate::classify)).
    pub facts: Option<Facts>,
}

impl Finding {
    /// Its bucket at `level`, once measured.
    #[must_use]
    pub fn verdict(&self, level: Level) -> Option<Verdict> {
        self.facts.as_ref().map(|facts| classify(facts, level))
    }
}

/// Something a scan could not cover, and why. Omissions make a report partial; they
/// never make anything look safe.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Omission {
    /// A declared root has no path.
    #[error("{root} is unknown: {reason}")]
    RootUnresolved {
        /// The root.
        root: SymbolicRoot,
        /// Why it has no path.
        reason: UnresolvedRoot,
    },
    /// A declared root exists but can't be read.
    #[error("{root} can't be read: {error}")]
    RootUnreadable {
        /// The root.
        root: SymbolicRoot,
        /// The upstream error.
        error: ProbeError,
    },
    /// A declared root is not a directory. A link is never followed, even here.
    #[error("{root} at {} is not a directory ({kind:?})", path.display())]
    RootNotDirectory {
        /// The root.
        root: SymbolicRoot,
        /// Its path.
        path: PathBuf,
        /// What is there instead.
        kind: EntryKind,
    },
    /// A root resolved through a link now leads to a different directory than when it
    /// was resolved (D-041), so it is not read.
    #[error("{root} at {} is no longer the directory it was resolved to", path.display())]
    RootChanged {
        /// The root.
        root: SymbolicRoot,
        /// The path it was resolved to.
        path: PathBuf,
    },
    /// A directory below a root could not be listed, so nothing inside it was found.
    #[error("{error}")]
    Unlisted {
        /// The upstream error, with the directory's path.
        error: ProbeError,
    },
    /// The integration stopped on a probe failure.
    #[error("discovery stopped early: {error}")]
    DiscoveryFailed {
        /// The upstream error.
        error: ProbeError,
    },
    /// A finding could not be sized.
    #[error("finding {finding} could not be sized: {error}")]
    SizingFailed {
        /// The finding.
        finding: FindingId,
        /// The upstream error.
        error: ProbeError,
    },
    /// The integration, or the sizing of its findings, panicked. When discovery panicked,
    /// whatever it found before is kept.
    #[error("the integration crashed")]
    Crashed,
}

/// Whether a scan ran to the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanOutcome {
    /// Every integration finished and every finding was sized.
    Completed,
    /// The scan was cancelled, or its consumer went away. The report holds what was
    /// found until then.
    Cancelled,
}

/// One integration's part of a report.
#[derive(Debug, Clone)]
pub struct IntegrationReport {
    /// The integration.
    pub id: IntegrationId,
    /// What it could not cover.
    pub omissions: Vec<Omission>,
    /// Native settings and commands it suggests.
    pub advice: Vec<Advice>,
}

/// The result of a scan, sorted the same way every time.
#[derive(Debug, Clone)]
pub struct ScanReport {
    /// Whether the scan ran to the end.
    pub outcome: ScanOutcome,
    /// Every integration, sorted by ID.
    pub integrations: Vec<IntegrationReport>,
    /// Every finding, sorted by integration, then path.
    pub findings: Vec<Finding>,
}

/// What to scan with.
#[derive(Debug, Clone, Default)]
pub struct ScanRequest {
    /// Every root on this platform, as the composition root resolved them.
    pub roots: Roots,
    /// Only the integrations of this lens, or all of them.
    pub lens: Option<Lens>,
}

/// Why an engine can't be built.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EngineError {
    /// A rule's integration isn't registered, so nothing could run it.
    #[error("rule {rule} belongs to {integration}, which isn't registered")]
    NoIntegration {
        /// The rule.
        rule: RuleId,
        /// The integration it names.
        integration: IntegrationId,
    },
}

/// The ports an engine reads the machine through. The platform implements them; tests
/// fake them.
#[derive(Debug)]
pub struct Ports {
    /// Read-only, no-follow file system probes.
    pub fs: Box<dyn FsProbe>,
    /// Sizes findings and finds when they last changed.
    pub sizer: Box<dyn Sizer>,
    /// Lists the running executables.
    pub processes: Box<dyn ProcessProbe>,
    /// The current time.
    pub clock: Box<dyn Clock>,
}

/// Runs scans over the registered integrations, through the injected ports.
#[derive(Debug)]
pub struct Engine {
    ports: Ports,
    integrations: Vec<Registered>,
    plan: WalkPlan,
}

/// An integration with its descriptor and its rules.
#[derive(Debug)]
struct Registered {
    integration: Box<dyn Integration>,
    descriptor: IntegrationDescriptor,
    rules: Vec<Rule>,
}

/// What every integration run of one scan shares.
#[derive(Debug, Clone, Copy)]
struct Shared<'a> {
    roots: &'a Roots,
    events: &'a EventTx,
    cancel: &'a CancelToken,
    next_id: &'a AtomicU64,
    walks: &'a Walks,
    repositories: &'a Repositories,
    processes: &'a Processes,
}

/// One integration's findings and report.
#[derive(Debug)]
struct IntegrationRun {
    report: IntegrationReport,
    findings: Vec<Finding>,
}

impl Engine {
    /// An engine over these ports, integrations and rules. Each rule runs in its
    /// integration's `discover`.
    ///
    /// # Errors
    /// When a rule's integration isn't one of `integrations`.
    pub fn new(
        ports: Ports,
        integrations: Vec<Box<dyn Integration>>,
        rules: Vec<Rule>,
    ) -> Result<Self, EngineError> {
        let mut registered: Vec<Registered> = integrations
            .into_iter()
            .map(|integration| Registered {
                descriptor: integration.descriptor(),
                integration,
                rules: Vec::new(),
            })
            .collect();
        let plan = WalkPlan::new(registered.iter().map(|r| &r.descriptor), &rules);
        for rule in rules {
            let Some(owner) = registered
                .iter_mut()
                .find(|r| r.descriptor.id == rule.integration)
            else {
                return Err(EngineError::NoIntegration {
                    integration: rule.integration,
                    rule: rule.id,
                });
            };
            owner.rules.push(rule);
        }
        Ok(Self {
            ports,
            integrations: registered,
            plan,
        })
    }

    /// Runs every integration concurrently, then sizes what each one found. Blocks until
    /// done, streaming events as it goes.
    ///
    /// A full event stream makes the scan wait. Cancelling `cancel`, or dropping the
    /// stream's receiver, stops new work from being scheduled, and the report then holds
    /// what was found until then.
    #[must_use]
    pub fn scan(&self, req: &ScanRequest, events: &EventTx, cancel: &CancelToken) -> ScanReport {
        let selected: Vec<&Registered> = self
            .integrations
            .iter()
            .filter(|r| req.lens.is_none_or(|lens| r.descriptor.lens == lens))
            .collect();
        let started = ScanEvent::ScanStarted {
            integrations: selected.len(),
        };
        if events.send(started, cancel).is_err() {
            return ScanReport {
                outcome: ScanOutcome::Cancelled,
                integrations: Vec::new(),
                findings: Vec::new(),
            };
        }
        let next_id = AtomicU64::new(0);
        let walks = Walks::default();
        let repositories = Repositories::default();
        let processes = Processes::default();
        let shared = Shared {
            roots: &req.roots,
            events,
            cancel,
            next_id: &next_id,
            walks: &walks,
            repositories: &repositories,
            processes: &processes,
        };
        let runs = self.run_all(&selected, shared);
        let outcome = if cancel.is_cancelled() {
            ScanOutcome::Cancelled
        } else {
            ScanOutcome::Completed
        };
        events.deliver(ScanEvent::ScanCompleted { outcome }, cancel);
        report(outcome, runs)
    }

    fn run_all(&self, selected: &[&Registered], shared: Shared<'_>) -> Vec<IntegrationRun> {
        thread::scope(|scope| {
            #[expect(
                clippy::needless_collect,
                reason = "every integration is spawned before the first join, or they would run one at a time"
            )]
            let handles: Vec<_> = selected
                .iter()
                .map(|&registered| {
                    let id = registered.descriptor.id;
                    let handle = scope.spawn(move || self.run_one(registered, shared));
                    (id, handle)
                })
                .collect();
            handles
                .into_iter()
                .map(|(id, handle)| handle.join().unwrap_or_else(|_| crashed(id, shared)))
                .collect()
        })
    }

    fn run_one(&self, registered: &Registered, shared: Shared<'_>) -> IntegrationRun {
        let id = registered.descriptor.id;
        let mut omissions = Vec::new();
        let opened = self
            .open_roots(registered, shared, &mut omissions)
            .unwrap_or_default();
        let cx = ScanContext {
            roots: &opened,
            rules: &registered.rules,
            fs: self.ports.fs.as_ref(),
            cancel: shared.cancel,
            plan: &self.plan,
            walks: shared.walks,
            walked: RefCell::default(),
        };
        let mut sink = FindingSink {
            integration: id,
            cx: &cx,
            events: shared.events,
            next_id: shared.next_id,
            found: Vec::new(),
            advice: Vec::new(),
        };
        if !shared.cancel.is_cancelled() {
            // A panic is caught here so what the integration found before it is kept.
            let discovered = panic::catch_unwind(AssertUnwindSafe(|| {
                registered.integration.discover(&cx, &mut sink)
            }));
            match discovered {
                Ok(Ok(()) | Err(IntegrationError::Stopped)) => {}
                Ok(Err(IntegrationError::Probe(error))) => {
                    omissions.push(Omission::DiscoveryFailed { error });
                }
                Err(_) => omissions.push(Omission::Crashed),
            }
        }
        // A directory under two paths of a root is one gap, in path order.
        let mut unlisted = BTreeMap::new();
        for walk in cx.walked.take().into_values() {
            for error in &walk.unreadable {
                unlisted.insert(error.path().to_owned(), error.clone());
            }
        }
        omissions.extend(
            unlisted
                .into_values()
                .map(|error| Omission::Unlisted { error }),
        );
        let (mut found, advice) = (sink.found, sink.advice);
        found.sort_by(|a, b| a.finding.path.cmp(&b.finding.path));
        let owner = Owner {
            registered,
            advised: !advice.is_empty(),
        };
        let findings = self.measure_all(found, owner, shared, &mut omissions);
        finish(id, omissions, advice, findings, shared)
    }

    /// The roots of the integration and its rules that exist and are directories that can
    /// be listed, announcing each one. A root that doesn't exist is not an omission: the
    /// tool simply isn't there.
    fn open_roots(
        &self,
        registered: &Registered,
        shared: Shared<'_>,
        omissions: &mut Vec<Omission>,
    ) -> Result<Vec<ResolvedRoot>, Stopped> {
        let mut roots: Vec<SymbolicRoot> = registered.descriptor.roots.to_vec();
        roots.extend(registered.rules.iter().map(|rule| rule.root));
        roots.sort_unstable();
        roots.dedup();
        let resolutions = roots
            .into_iter()
            .flat_map(|root| shared.roots.all(root).iter().map(move |r| (root, r)));
        let mut opened = Vec::new();
        for (root, resolution) in resolutions {
            if shared.cancel.is_cancelled() {
                return Err(Stopped::Cancelled);
            }
            let resolved = match resolution {
                Err(reason) => {
                    let reason = reason.clone();
                    omissions.push(Omission::RootUnresolved { root, reason });
                    continue;
                }
                Ok(resolved) => resolved,
            };
            match self.open(&resolved.path) {
                Ok(meta) if changed(resolved, &meta) => omissions.push(Omission::RootChanged {
                    root,
                    path: resolved.path.clone(),
                }),
                Ok(meta) if meta.kind == EntryKind::Dir => {
                    let event = ScanEvent::RootOpened {
                        integration: registered.descriptor.id,
                        root,
                        path: resolved.path.clone(),
                    };
                    shared.events.send(event, shared.cancel)?;
                    opened.push(resolved.clone());
                }
                Ok(meta) => omissions.push(Omission::RootNotDirectory {
                    root,
                    path: resolved.path.clone(),
                    kind: meta.kind,
                }),
                Err(error) if resolved.link.is_some() && missing(&error) => {
                    omissions.push(Omission::RootChanged {
                        root,
                        path: resolved.path.clone(),
                    });
                }
                Err(error) if missing(&error) => {}
                Err(error) => omissions.push(Omission::RootUnreadable { root, error }),
            }
        }
        Ok(opened)
    }

    /// What is at `path`. A directory that can't be listed is unreadable, even when its
    /// metadata can be read.
    fn open(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        let meta = self.ports.fs.metadata(path)?;
        if meta.kind == EntryKind::Dir {
            self.ports.fs.open_dir(path)?;
        }
        Ok(meta)
    }

    /// Measures each finding in turn and establishes its facts, streaming each result.
    /// Stops at the first one once the scan is cancelled; those not reached stay
    /// unmeasured.
    fn measure_all(
        &self,
        found: Vec<Found>,
        owner: Owner<'_>,
        shared: Shared<'_>,
        omissions: &mut Vec<Omission>,
    ) -> Vec<Finding> {
        let mut findings = Vec::with_capacity(found.len());
        let mut stopped = false;
        for one in found {
            stopped = stopped || shared.cancel.is_cancelled();
            if stopped {
                findings.push(one.finding);
                continue;
            }
            let measured = self.ports.sizer.size(&one.finding.path, shared.cancel);
            let measurement = match measured {
                Ok(measurement) => Some(measurement),
                Err(error) => {
                    let finding = one.finding.id;
                    omissions.push(Omission::SizingFailed { finding, error });
                    None
                }
            };
            let facts = self.facts(&one, measurement, owner, shared);
            let mut finding = one.finding;
            finding.size = measurement.map(|m| m.size);
            finding.facts = Some(facts.clone());
            let event = ScanEvent::FindingUpdated {
                id: finding.id,
                size: finding.size,
                facts,
            };
            stopped = shared.events.send(event, shared.cancel).is_err();
            findings.push(finding);
        }
        findings
    }

    /// What the host knows about a finding once measured. One that couldn't be measured
    /// completely has no known modification time.
    fn facts(
        &self,
        found: &Found,
        measurement: Option<Measurement>,
        owner: Owner<'_>,
        shared: Shared<'_>,
    ) -> Facts {
        let path = &found.finding.path;
        let modified = measurement
            .filter(|m| m.size.completeness == Completeness::Complete)
            .and_then(|m| m.modified)
            .map_or(Modified::Unknown, |at| {
                let age = self.ports.clock.now().duration_since(at);
                Modified::Ago(age.unwrap_or_default())
            });
        let processes = self.ports.processes.as_ref();
        let rule = owner.rule(&found.finding.rule);
        let layout_verified = found
            .finding
            .evidence
            .iter()
            .any(|evidence| matches!(evidence, Evidence::Signature(_)));
        Facts {
            proposal: owner.proposal(rule),
            layout_verified,
            consequence: found.consequence,
            tracked: shared.repositories.tracked(self.ports.fs.as_ref(), path),
            activity: shared.processes.activity(processes, path),
            modified,
            recent_days: rule.and_then(|rule| rule.recent_days),
        }
    }
}

/// The integration whose findings are measured, and whether it gave advice.
#[derive(Debug, Clone, Copy)]
struct Owner<'a> {
    registered: &'a Registered,
    advised: bool,
}

impl<'a> Owner<'a> {
    fn rule(&self, id: &RuleId) -> Option<&'a Rule> {
        self.registered.rules.iter().find(|rule| &rule.id == id)
    }

    /// What `rule` proposes: a report rule of an integration that gave advice points to
    /// that advice.
    fn proposal(&self, rule: Option<&Rule>) -> Proposal {
        match rule.map(|rule| &rule.operation) {
            Some(Operation::RemoveFiles(methods)) => Proposal::Remove(methods.clone()),
            Some(Operation::Report) | None if self.advised => Proposal::Advice,
            Some(Operation::Report) | None => Proposal::Report,
        }
    }
}

/// A finding as its integration added it, with the consequence it verified.
#[derive(Debug)]
pub(crate) struct Found {
    pub(crate) finding: Finding,
    pub(crate) consequence: Option<Consequence>,
}

fn missing(error: &ProbeError) -> bool {
    error.io_kind() == Some(io::ErrorKind::NotFound)
}

/// Whether a root resolved through a link no longer has the identity it had then.
fn changed(resolved: &ResolvedRoot, meta: &EntryMeta) -> bool {
    resolved
        .link
        .as_ref()
        .is_some_and(|link| meta.identity != Some(link.identity))
}

fn finish(
    id: IntegrationId,
    omissions: Vec<Omission>,
    advice: Vec<Advice>,
    findings: Vec<Finding>,
    shared: Shared<'_>,
) -> IntegrationRun {
    let event = ScanEvent::IntegrationCompleted {
        integration: id,
        omissions: omissions.clone(),
    };
    shared.events.deliver(event, shared.cancel);
    IntegrationRun {
        report: IntegrationReport {
            id,
            omissions,
            advice,
        },
        findings,
    }
}

/// The run of an integration whose sizing panicked. Its findings were streamed but are
/// lost.
fn crashed(id: IntegrationId, shared: Shared<'_>) -> IntegrationRun {
    finish(id, vec![Omission::Crashed], Vec::new(), Vec::new(), shared)
}

fn report(outcome: ScanOutcome, runs: Vec<IntegrationRun>) -> ScanReport {
    let mut integrations = Vec::with_capacity(runs.len());
    let mut findings = Vec::new();
    for run in runs {
        integrations.push(run.report);
        findings.extend(run.findings);
    }
    integrations.sort_by_key(|report| report.id);
    findings.sort_by(|a, b| (a.integration, &a.path).cmp(&(b.integration, &b.path)));
    ScanReport {
        outcome,
        integrations,
        findings,
    }
}
