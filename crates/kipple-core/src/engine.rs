//! The engine every frontend calls (`docs/05-architecture.md` §2). Scanning is
//! read-only: integrations propose findings, the engine sizes them and reports.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::thread;

use crate::cancel::CancelToken;
use crate::event::{EventTx, ScanEvent, Stopped};
use crate::integration::{
    FindingSink, Integration, IntegrationDescriptor, IntegrationError, IntegrationId, ScanContext,
};
use crate::probe::{EntryKind, FsProbe, ProbeError, Sizer};
use crate::root::{ResolvedRoot, Roots, SymbolicRoot, UnresolvedRoot};
use crate::size::SpaceEstimate;

/// A finding's ID within one scan. IDs are references, never authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FindingId(pub u64);

impl fmt::Display for FindingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Something an integration found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Its ID within this scan.
    pub id: FindingId,
    /// The integration that found it.
    pub integration: IntegrationId,
    /// Where it is.
    pub path: PathBuf,
    /// What removing it would free, once sized.
    pub size: Option<SpaceEstimate>,
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
    /// The integration panicked. Whatever it found before is kept.
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
}

/// Runs scans over the registered integrations, through the injected ports.
#[derive(Debug)]
pub struct Engine {
    fs: Box<dyn FsProbe>,
    sizer: Box<dyn Sizer>,
    integrations: Vec<Box<dyn Integration>>,
}

/// What every integration run of one scan shares.
#[derive(Debug, Clone, Copy)]
struct Shared<'a> {
    roots: &'a Roots,
    events: &'a EventTx,
    cancel: &'a CancelToken,
    next_id: &'a AtomicU64,
}

/// One integration's findings and report.
#[derive(Debug)]
struct IntegrationRun {
    report: IntegrationReport,
    findings: Vec<Finding>,
}

impl Engine {
    /// An engine over these ports and integrations.
    #[must_use]
    pub fn new(
        fs: Box<dyn FsProbe>,
        sizer: Box<dyn Sizer>,
        integrations: Vec<Box<dyn Integration>>,
    ) -> Self {
        Self {
            fs,
            sizer,
            integrations,
        }
    }

    /// Runs every integration concurrently, then sizes what each one found. Blocks until
    /// done, streaming events as it goes.
    ///
    /// A full event stream makes the scan wait. Cancelling `cancel`, or dropping the
    /// stream's receiver, stops new work from being scheduled, and the report then holds
    /// what was found until then.
    #[must_use]
    pub fn scan(&self, req: &ScanRequest, events: &EventTx, cancel: &CancelToken) -> ScanReport {
        let started = ScanEvent::ScanStarted {
            integrations: self.integrations.len(),
        };
        if events.send(started, cancel).is_err() {
            return ScanReport {
                outcome: ScanOutcome::Cancelled,
                integrations: Vec::new(),
                findings: Vec::new(),
            };
        }
        let next_id = AtomicU64::new(0);
        let shared = Shared {
            roots: &req.roots,
            events,
            cancel,
            next_id: &next_id,
        };
        let runs = self.run_all(shared);
        let outcome = if cancel.is_cancelled() {
            ScanOutcome::Cancelled
        } else {
            ScanOutcome::Completed
        };
        events.deliver(ScanEvent::ScanCompleted { outcome }, cancel);
        report(outcome, runs)
    }

    fn run_all(&self, shared: Shared<'_>) -> Vec<IntegrationRun> {
        thread::scope(|scope| {
            #[expect(
                clippy::needless_collect,
                reason = "every integration is spawned before the first join, or they would run one at a time"
            )]
            let handles: Vec<_> = self
                .integrations
                .iter()
                .map(|integration| {
                    let descriptor = integration.descriptor();
                    let id = descriptor.id;
                    let handle = scope
                        .spawn(move || self.run_one(integration.as_ref(), &descriptor, shared));
                    (id, handle)
                })
                .collect();
            handles
                .into_iter()
                .map(|(id, handle)| handle.join().unwrap_or_else(|_| crashed(id, shared)))
                .collect()
        })
    }

    fn run_one(
        &self,
        integration: &dyn Integration,
        descriptor: &IntegrationDescriptor,
        shared: Shared<'_>,
    ) -> IntegrationRun {
        let mut omissions = Vec::new();
        let opened = self
            .open_roots(descriptor, shared, &mut omissions)
            .unwrap_or_default();
        let mut sink = FindingSink {
            integration: descriptor.id,
            events: shared.events,
            cancel: shared.cancel,
            next_id: shared.next_id,
            found: Vec::new(),
        };
        if !shared.cancel.is_cancelled() {
            let cx = ScanContext {
                roots: &opened,
                fs: self.fs.as_ref(),
                cancel: shared.cancel,
            };
            if let Err(IntegrationError::Probe(error)) = integration.discover(&cx, &mut sink) {
                omissions.push(Omission::DiscoveryFailed { error });
            }
        }
        let mut findings = sink.found;
        findings.sort_by(|a, b| a.path.cmp(&b.path));
        self.size_all(&mut findings, shared, &mut omissions);
        finish(descriptor.id, omissions, findings, shared)
    }

    /// The declared roots that exist and are directories, announcing each one. A root
    /// that doesn't exist is not an omission: the tool simply isn't there.
    fn open_roots(
        &self,
        descriptor: &IntegrationDescriptor,
        shared: Shared<'_>,
        omissions: &mut Vec<Omission>,
    ) -> Result<Vec<ResolvedRoot>, Stopped> {
        let mut opened = Vec::new();
        for &root in &descriptor.roots {
            let resolved = match shared.roots.get(root) {
                None => continue,
                Some(Err(reason)) => {
                    let reason = reason.clone();
                    omissions.push(Omission::RootUnresolved { root, reason });
                    continue;
                }
                Some(Ok(resolved)) => resolved,
            };
            match self.fs.metadata(&resolved.path) {
                Ok(meta) if meta.kind == EntryKind::Dir => {
                    let event = ScanEvent::RootOpened {
                        integration: descriptor.id,
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
                Err(error) if error.io_kind() == Some(io::ErrorKind::NotFound) => {}
                Err(error) => omissions.push(Omission::RootUnreadable { root, error }),
            }
        }
        Ok(opened)
    }

    fn size_all(
        &self,
        findings: &mut [Finding],
        shared: Shared<'_>,
        omissions: &mut Vec<Omission>,
    ) {
        for finding in findings {
            if shared.cancel.is_cancelled() {
                return;
            }
            let size = match self.sizer.size(&finding.path, shared.cancel) {
                Ok(size) => size,
                Err(error) => {
                    let finding = finding.id;
                    omissions.push(Omission::SizingFailed { finding, error });
                    continue;
                }
            };
            finding.size = Some(size);
            let event = ScanEvent::FindingUpdated {
                id: finding.id,
                size,
            };
            if shared.events.send(event, shared.cancel).is_err() {
                return;
            }
        }
    }
}

fn finish(
    id: IntegrationId,
    omissions: Vec<Omission>,
    findings: Vec<Finding>,
    shared: Shared<'_>,
) -> IntegrationRun {
    let event = ScanEvent::IntegrationCompleted {
        integration: id,
        omissions: omissions.clone(),
    };
    shared.events.deliver(event, shared.cancel);
    IntegrationRun {
        report: IntegrationReport { id, omissions },
        findings,
    }
}

/// The run of an integration that panicked. Its findings were streamed but are lost.
fn crashed(id: IntegrationId, shared: Shared<'_>) -> IntegrationRun {
    finish(id, vec![Omission::Crashed], Vec::new(), shared)
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
