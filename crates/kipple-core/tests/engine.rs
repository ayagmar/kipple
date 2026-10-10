//! `Engine::scan` against fake ports: the event contract of `docs/05-architecture.md` §2
//! and the root rules of §1.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

use kipple_core::{
    Allocated, Apparent, CancelToken, Completeness, DirEntryMeta, Engine, EntryKind, EntryMeta,
    EventRx, FindingSink, FsProbe, Integration, IntegrationDescriptor, IntegrationError,
    IntegrationId, Omission, ProbeError, ResolvedRoot, RootSource, Roots, ScanContext, ScanEvent,
    ScanOutcome, ScanReport, ScanRequest, Sizer, SpaceEstimate, SymbolicRoot, UnresolvedRoot,
    event_channel,
};

/// Ten times the 100 ms cancellation budget: long enough not to flake on a loaded CI
/// runner, short enough that a producer stuck on a full stream fails the test.
const ACK_LIMIT: Duration = Duration::from_secs(1);

const SIZE: SpaceEstimate = SpaceEstimate {
    apparent: Apparent(10),
    allocated: Some(Allocated(4096)),
    unique_reclaim: Some(Allocated(4096)),
    completeness: Completeness::Complete,
};

/// Every path is a directory that can be listed, unless listed otherwise.
#[derive(Debug, Default)]
struct FakeFs {
    entries: BTreeMap<PathBuf, Result<EntryKind, io::ErrorKind>>,
    unlistable: BTreeSet<PathBuf>,
}

impl FsProbe for FakeFs {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        match self
            .entries
            .get(path)
            .copied()
            .unwrap_or(Ok(EntryKind::Dir))
        {
            Ok(kind) => Ok(EntryMeta {
                kind,
                identity: None,
            }),
            Err(kind) => Err(ProbeError::io(path, kind.into())),
        }
    }

    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        if self.unlistable.contains(dir) {
            return Err(ProbeError::io(dir, io::ErrorKind::PermissionDenied.into()));
        }
        Ok(Vec::new())
    }
}

/// Sizes everything as [`SIZE`]. With `block_first`, the first call meets the test at the
/// barrier and then waits for cancellation, like a walk of a huge tree.
#[derive(Debug, Default)]
struct FakeSizer {
    calls: Arc<AtomicUsize>,
    block_first: Option<Arc<Barrier>>,
}

impl Sizer for FakeSizer {
    fn size(&self, _: &Path, cancel: &CancelToken) -> Result<SpaceEstimate, ProbeError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let Some(started) = self.block_first.as_ref().filter(|_| call == 0) else {
            return Ok(SIZE);
        };
        started.wait();
        while !cancel.is_cancelled() {
            thread::yield_now();
        }
        Ok(SpaceEstimate {
            completeness: Completeness::Cancelled,
            ..SIZE
        })
    }
}

/// The root every emitter reads.
const FOUND: &str = "/found";

/// Adds `count` findings under `/found/<id>/`, in descending order, then stops. Counts
/// how many it tried to add, and can meet the test at a barrier before the first one.
#[derive(Debug)]
struct Emitter {
    id: &'static str,
    count: usize,
    attempts: Arc<AtomicUsize>,
    first_add: Option<Arc<Barrier>>,
}

impl Emitter {
    fn new(id: &'static str, count: usize) -> Self {
        Self {
            id,
            count,
            attempts: Arc::default(),
            first_add: None,
        }
    }
}

impl Integration for Emitter {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId(self.id),
            roots: vec![SymbolicRoot::XdgCache],
        }
    }

    fn discover(
        &self,
        _: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        for n in (0..self.count).rev() {
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0
                && let Some(first_add) = &self.first_add
            {
                first_add.wait();
            }
            sink.add(PathBuf::from(format!("/found/{}/{n:05}", self.id)))?;
        }
        Ok(())
    }
}

/// A request in which the emitters' root exists.
fn found() -> ScanRequest {
    let mut roots = Roots::default();
    insert(&mut roots, SymbolicRoot::XdgCache, FOUND);
    ScanRequest { roots }
}

fn engine(sizer: FakeSizer, integrations: Vec<Box<dyn Integration>>) -> Engine {
    Engine::new(Box::new(FakeFs::default()), Box::new(sizer), integrations)
}

/// Runs the scan on its own thread, so the test can act as a stalled or departing consumer.
fn spawn_scan(
    engine: Engine,
    request: ScanRequest,
    capacity: usize,
    cancel: &CancelToken,
) -> (EventRx, mpsc::Receiver<ScanReport>) {
    let (events, rx) = event_channel(capacity);
    let (done, report) = mpsc::channel();
    let cancel = cancel.clone();
    thread::spawn(move || {
        done.send(engine.scan(&request, &events, &cancel)).ok();
    });
    (rx, report)
}

#[test]
fn a_stalled_consumer_cannot_keep_a_cancel_from_being_acknowledged() {
    let first_add = Arc::new(Barrier::new(2));
    let mut emitter = Emitter::new("stalled", 10_000);
    emitter.first_add = Some(Arc::clone(&first_add));
    let attempts = Arc::clone(&emitter.attempts);
    let cancel = CancelToken::new();
    let engine = engine(FakeSizer::default(), vec![Box::new(emitter)]);

    // Nobody reads: the stream fills with `ScanStarted` and `RootOpened`, and the first
    // finding waits.
    let (_events, report) = spawn_scan(engine, found(), 2, &cancel);
    first_add.wait();
    cancel.cancel();

    let report = report
        .recv_timeout(ACK_LIMIT)
        .expect("cancel not acknowledged");
    assert_eq!(report.outcome, ScanOutcome::Cancelled);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(report.findings.len(), 0);
}

#[test]
fn a_slow_consumer_receives_every_finding_and_size_instead_of_losing_them() {
    let count = 300;
    let engine = engine(
        FakeSizer::default(),
        vec![Box::new(Emitter::new("slow", count))],
    );
    let (events, report) = spawn_scan(engine, found(), 2, &CancelToken::new());

    let mut added = HashSet::new();
    let mut sized = HashSet::new();
    let mut last = None;
    for sequenced in events {
        if let ScanEvent::FindingAdded { id, .. } = &sequenced.event {
            assert!(added.insert(*id));
        }
        if let ScanEvent::FindingUpdated { id, .. } = &sequenced.event {
            assert!(added.contains(id), "{id} sized before it was added");
            assert!(sized.insert(*id));
        }
        last = Some(sequenced.event);
    }

    assert_eq!((added.len(), sized.len()), (count, count));
    assert!(matches!(
        last,
        Some(ScanEvent::ScanCompleted {
            outcome: ScanOutcome::Completed
        })
    ));
    let report = report.recv_timeout(ACK_LIMIT).unwrap();
    assert_eq!(report.outcome, ScanOutcome::Completed);
    assert!(report.findings.iter().all(|f| f.size == Some(SIZE)));
}

#[test]
fn a_consumer_that_goes_away_cancels_the_scan_and_keeps_the_partial_report() {
    let emitter = Emitter::new("dropped", 10_000);
    let attempts = Arc::clone(&emitter.attempts);
    let cancel = CancelToken::new();
    let engine = engine(FakeSizer::default(), vec![Box::new(emitter)]);
    let (events, report) = spawn_scan(engine, found(), 4, &cancel);

    assert_eq!(events.take(20).count(), 20);

    let report = report.recv_timeout(ACK_LIMIT).expect("scan kept running");
    assert_eq!(report.outcome, ScanOutcome::Cancelled);
    assert!(cancel.is_cancelled());
    // It stopped at the first finding nobody could receive.
    assert_ne!(report.findings.len(), 0);
    assert_eq!(attempts.load(Ordering::SeqCst), report.findings.len() + 1);
}

#[test]
fn cancelling_during_sizing_stops_before_the_next_finding_is_sized() {
    let sizing = Arc::new(Barrier::new(2));
    let sizer = FakeSizer {
        block_first: Some(Arc::clone(&sizing)),
        ..FakeSizer::default()
    };
    let calls = Arc::clone(&sizer.calls);
    let cancel = CancelToken::new();
    let engine = engine(sizer, vec![Box::new(Emitter::new("sizing", 3))]);
    let (events, report) = spawn_scan(engine, found(), 64, &cancel);
    let drain = thread::spawn(move || events.count());

    sizing.wait();
    cancel.cancel();

    let report = report
        .recv_timeout(ACK_LIMIT)
        .expect("cancel not acknowledged");
    assert_eq!(report.outcome, ScanOutcome::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let measured: Vec<_> = report
        .findings
        .iter()
        .map(|f| f.size.map(|s| s.completeness))
        .collect();
    assert_eq!(measured, [Some(Completeness::Cancelled), None, None]);
    drain.join().unwrap();
}

/// Declares every root it is given and records what the engine let it read: for each
/// path in `probe`, how listing it and adding it as a finding went.
#[derive(Debug)]
struct RootReader {
    roots: Vec<SymbolicRoot>,
    seen: Arc<Mutex<Vec<PathBuf>>>,
    probes: Arc<Mutex<Vec<(&'static str, &'static str)>>>,
    probe: Vec<PathBuf>,
}

/// How a probe went: allowed, or refused and why.
const fn outcome(error: Option<&ProbeError>) -> &'static str {
    match error {
        None => "allowed",
        Some(ProbeError::OutsideRoots { .. }) => "outside",
        Some(ProbeError::ThroughLink { .. }) => "link",
        Some(ProbeError::Io { .. }) => "failed",
    }
}

impl Integration for RootReader {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("roots"),
            roots: self.roots.clone(),
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.extend(cx.roots().iter().map(|root| root.path.clone()));
        }
        if let Ok(mut probes) = self.probes.lock() {
            for path in &self.probe {
                let read = cx.read_dir(path).err();
                let add = match sink.add(path.clone()) {
                    Ok(()) | Err(IntegrationError::Stopped) => None,
                    Err(IntegrationError::Probe(error)) => Some(error),
                };
                probes.push((outcome(read.as_ref()), outcome(add.as_ref())));
            }
        }
        Ok(())
    }
}

fn insert(roots: &mut Roots, root: SymbolicRoot, path: &str) {
    let path = PathBuf::from(path);
    let source = RootSource::Env("TEST");
    roots.insert(root, Ok(ResolvedRoot { root, path, source }));
}

fn run(engine: &Engine, request: &ScanRequest) -> ScanReport {
    let (events, rx) = event_channel(1024);
    let report = engine.scan(request, &events, &CancelToken::new());
    drop(rx);
    report
}

#[test]
fn roots_that_cannot_be_read_are_omissions_and_are_never_handed_to_the_integration() {
    let fs = FakeFs {
        entries: BTreeMap::from([
            (
                PathBuf::from("/denied"),
                Err(io::ErrorKind::PermissionDenied),
            ),
            (PathBuf::from("/absent"), Err(io::ErrorKind::NotFound)),
            (PathBuf::from("/link"), Ok(EntryKind::Symlink)),
        ]),
        // Its metadata can be read, but it can't be listed, like a directory with mode 000.
        unlistable: BTreeSet::from([PathBuf::from("/unlistable")]),
    };
    let mut roots = Roots::default();
    insert(&mut roots, SymbolicRoot::Home, "/home");
    insert(&mut roots, SymbolicRoot::XdgCache, "/denied");
    insert(&mut roots, SymbolicRoot::XdgData, "/absent");
    insert(&mut roots, SymbolicRoot::XdgState, "/link");
    insert(&mut roots, SymbolicRoot::LocalAppData, "/unlistable");
    let unset = UnresolvedRoot::NotSet { var: "HOME" };
    roots.insert(SymbolicRoot::XdgConfig, Err(unset));
    let seen = Arc::default();
    let reader = RootReader {
        // macOS caches is not a root on this fake platform, so it is simply skipped.
        roots: vec![
            SymbolicRoot::Home,
            SymbolicRoot::XdgCache,
            SymbolicRoot::XdgData,
            SymbolicRoot::XdgState,
            SymbolicRoot::LocalAppData,
            SymbolicRoot::XdgConfig,
            SymbolicRoot::MacosCaches,
        ],
        seen: Arc::clone(&seen),
        probes: Arc::default(),
        probe: Vec::new(),
    };
    let engine = Engine::new(
        Box::new(fs),
        Box::new(FakeSizer::default()),
        vec![Box::new(reader)],
    );

    let report = run(&engine, &ScanRequest { roots });

    assert_eq!(*seen.lock().unwrap(), [PathBuf::from("/home")]);
    let omissions = &report.integrations[0].omissions;
    assert!(matches!(
        omissions.as_slice(),
        [
            Omission::RootUnreadable { root: SymbolicRoot::XdgCache, error: stat },
            Omission::RootNotDirectory { root: SymbolicRoot::XdgState, kind: EntryKind::Symlink, .. },
            Omission::RootUnreadable { root: SymbolicRoot::LocalAppData, error: list },
            Omission::RootUnresolved { root: SymbolicRoot::XdgConfig, .. },
        ] if stat.io_kind() == Some(io::ErrorKind::PermissionDenied)
            && list.io_kind() == Some(io::ErrorKind::PermissionDenied)
    ));
}

#[test]
fn an_integration_can_neither_probe_nor_report_anything_outside_its_roots() {
    let mut roots = Roots::default();
    insert(&mut roots, SymbolicRoot::XdgCache, "/home/u/.cache");
    let fs = FakeFs {
        entries: BTreeMap::from([(PathBuf::from("/home/u/.cache/link"), Ok(EntryKind::Symlink))]),
        ..FakeFs::default()
    };
    let probes = Arc::default();
    let reader = RootReader {
        roots: vec![SymbolicRoot::XdgCache],
        seen: Arc::default(),
        probes: Arc::clone(&probes),
        probe: [
            "/home/u/.cache/tool",
            "/home/u",
            "/home/u/.cache-other",
            "/home/u/.cache/../.ssh",
            "/home/u/.cache/link/inner",
        ]
        .map(PathBuf::from)
        .to_vec(),
    };
    let engine = Engine::new(
        Box::new(fs),
        Box::new(FakeSizer::default()),
        vec![Box::new(reader)],
    );

    let report = run(&engine, &ScanRequest { roots });

    assert_eq!(
        *probes.lock().unwrap(),
        [
            ("allowed", "allowed"),
            ("outside", "outside"),
            ("outside", "outside"),
            ("outside", "outside"),
            ("link", "link"),
        ]
    );
    let found: Vec<_> = report.findings.iter().map(|f| f.path.clone()).collect();
    assert_eq!(found, [PathBuf::from("/home/u/.cache/tool")]);
}

/// Adds one finding, then panics.
#[derive(Debug)]
struct Crashes;

impl Integration for Crashes {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("crashes"),
            roots: vec![SymbolicRoot::XdgCache],
        }
    }

    #[expect(
        clippy::panic,
        clippy::panic_in_result_fn,
        reason = "simulates a bug in an integration"
    )]
    fn discover(
        &self,
        _: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        sink.add(PathBuf::from("/found/crashes/00000"))?;
        panic!("integration bug");
    }
}

#[test]
fn the_report_is_sorted_and_keeps_what_a_crashed_integration_found_before_it_crashed() {
    let engine = engine(
        FakeSizer::default(),
        vec![
            Box::new(Emitter::new("zeta", 3)),
            Box::new(Crashes),
            Box::new(Emitter::new("alpha", 3)),
        ],
    );

    let report = run(&engine, &found());

    let found: Vec<_> = report
        .findings
        .iter()
        .map(|f| f.path.to_str().unwrap())
        .collect();
    assert_eq!(
        found,
        [
            "/found/alpha/00000",
            "/found/alpha/00001",
            "/found/alpha/00002",
            "/found/crashes/00000",
            "/found/zeta/00000",
            "/found/zeta/00001",
            "/found/zeta/00002",
        ]
    );
    let ids: Vec<_> = report.integrations.iter().map(|i| i.id.0).collect();
    assert_eq!(ids, ["alpha", "crashes", "zeta"]);
    assert!(matches!(
        report.integrations[1].omissions.as_slice(),
        [Omission::Crashed]
    ));
    assert_eq!(report.outcome, ScanOutcome::Completed);
}
