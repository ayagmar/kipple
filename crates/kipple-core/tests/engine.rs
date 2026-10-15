//! `Engine::scan` against fake ports: the event contract of `docs/05-architecture.md` §2
//! and the root rules of §1.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use kipple_core::{
    Activity, ActivityGap, Allocated, Apparent, CancelToken, Category, Clock, Completeness,
    Consequence, Cost, DirEntryMeta, Engine, EngineError, EntryKind, EntryMeta, EventRx, Evidence,
    Facts, FileIdentity, FindingSink, FsProbe, Hold, Integration, IntegrationDescriptor,
    IntegrationError, IntegrationId, Inventory, Lens, Level, Measurement, Method, Modified, Name,
    Omission, Operation, Platform, Policy, Ports, ProbeError, ProcessOutcome, ProcessProbe,
    ProcessRecord, ProcessSnapshot, Recovery, ResolvedRoot, RootLink, RootSource, Roots, Rule,
    RuleId, RuleText, ScanContext, ScanEvent, ScanOutcome, ScanReport, ScanRequest, Selector,
    Sizer, SpaceEstimate, SymbolicRoot, Tracked, UnresolvedRoot, Verdict, WalkGap, event_channel,
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

/// Every path is an empty directory that can be listed, with no identity, unless listed
/// otherwise, or it is a `.git` and `no_git` is set. Counts how often each directory is
/// listed.
#[derive(Debug, Default)]
struct FakeFs {
    no_git: bool,
    entries: BTreeMap<PathBuf, Result<EntryKind, io::ErrorKind>>,
    unlistable: BTreeSet<PathBuf>,
    identities: BTreeMap<PathBuf, FileIdentity>,
    listings: BTreeMap<PathBuf, Vec<DirEntryMeta>>,
    listed: Arc<Mutex<Vec<PathBuf>>>,
}

impl FakeFs {
    /// A tree of directories and files, given as paths: one ending in `/` is a directory.
    #[expect(
        clippy::unwrap_used,
        reason = "a fixture that fails to build fails the test"
    )]
    fn tree(paths: &[&str]) -> Self {
        let mut listings: BTreeMap<PathBuf, Vec<DirEntryMeta>> = BTreeMap::new();
        let mut add = |path: &Path, kind| {
            let entry = DirEntryMeta {
                name: OsString::from(path.file_name().unwrap()),
                kind,
            };
            let listing = listings
                .entry(path.parent().unwrap().to_owned())
                .or_default();
            if !listing.contains(&entry) {
                listing.push(entry);
            }
        };
        for path in paths {
            let kind = if path.ends_with('/') {
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            let path = Path::new(path.trim_end_matches('/'));
            for dir in path
                .ancestors()
                .skip(1)
                .filter(|dir| dir.parent().is_some())
            {
                add(dir, EntryKind::Dir);
            }
            add(path, kind);
        }
        Self {
            listings,
            ..Self::default()
        }
    }
}

impl FsProbe for FakeFs {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        let absent = self.no_git && path.file_name() == Some(".git".as_ref());
        let default = if absent {
            Err(io::ErrorKind::NotFound)
        } else {
            Ok(EntryKind::Dir)
        };
        match self.entries.get(path).copied().unwrap_or(default) {
            Ok(kind) => Ok(EntryMeta {
                kind,
                identity: self.identities.get(path).copied(),
            }),
            Err(kind) => Err(ProbeError::io(path, kind.into())),
        }
    }

    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        self.open_dir(dir)?;
        if let Ok(mut listed) = self.listed.lock() {
            listed.push(dir.to_owned());
        }
        Ok(self.listings.get(dir).cloned().unwrap_or_default())
    }

    fn open_dir(&self, dir: &Path) -> Result<(), ProbeError> {
        if self.unlistable.contains(dir) {
            return Err(ProbeError::io(dir, io::ErrorKind::PermissionDenied.into()));
        }
        Ok(())
    }

    fn read_file(&self, path: &Path, _: u64) -> Result<Vec<u8>, ProbeError> {
        Err(ProbeError::io(path, io::ErrorKind::NotFound.into()))
    }
}

/// Sizes everything as [`SIZE`], last modified at [`NOW`], unless `measurement` says
/// otherwise. With `block_first`, the first call meets the test at the barrier and then
/// waits for cancellation, like a walk of a huge tree.
#[derive(Debug, Default)]
struct FakeSizer {
    calls: Arc<AtomicUsize>,
    block_first: Option<Arc<Barrier>>,
    measurement: Option<Measurement>,
}

impl Sizer for FakeSizer {
    fn size(&self, _: &Path, cancel: &CancelToken) -> Result<Measurement, ProbeError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let Some(started) = self.block_first.as_ref().filter(|_| call == 0) else {
            return Ok(self.measurement.clone().unwrap_or(measured(SIZE)));
        };
        started.wait();
        while !cancel.is_cancelled() {
            thread::yield_now();
        }
        Ok(measured(SpaceEstimate {
            completeness: Completeness::Cancelled,
            ..SIZE
        }))
    }
}

/// `size`, last modified at [`NOW`].
const fn measured(size: SpaceEstimate) -> Measurement {
    Measurement {
        size,
        modified: Some(NOW),
        repository: None,
    }
}

/// When every test scan runs.
const NOW: SystemTime = SystemTime::UNIX_EPOCH;

/// A clock stopped at [`NOW`].
#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        NOW
    }
}

/// The processes a host runs.
#[derive(Debug)]
struct FakeProcesses(Result<ProcessSnapshot, ProbeError>);

impl ProcessProbe for FakeProcesses {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        self.0.clone()
    }
}

/// A host that runs nothing.
const fn no_processes() -> FakeProcesses {
    FakeProcesses(Ok(ProcessSnapshot {
        processes: Vec::new(),
        inventory: Inventory::Complete,
    }))
}

/// The engine's ports over `fs` and `sizer`, on a host that runs nothing, at [`NOW`].
fn ports(fs: FakeFs, sizer: FakeSizer) -> Ports {
    Ports {
        fs: Box::new(fs),
        sizer: Box::new(sizer),
        processes: Box::new(no_processes()),
        clock: Box::new(FixedClock),
    }
}

/// A descriptor for `id` that reads no roots.
const fn descriptor(id: &'static str) -> IntegrationDescriptor {
    IntegrationDescriptor {
        id: IntegrationId(id),
        lens: Lens::Dev,
        platforms: &[Platform::Linux, Platform::Macos, Platform::Windows],
        roots: &[],
        selectors: &[],
        artifacts: &[],
        owner_files: &[],
        verified: &[],
    }
}

/// A report-only rule of `integration` under `root`.
fn rule(integration: &'static str, root: SymbolicRoot, selector: Selector) -> Rule {
    let text = |s: &str| s.to_owned();
    Rule {
        id: RuleId(format!("builtin.{integration}.test")),
        integration: IntegrationId(integration),
        text: RuleText {
            title: text("Test"),
            description: String::new(),
            why: text("why"),
            loss: text("loss"),
            references: Vec::new(),
        },
        category: Category::Cache,
        platforms: Platform::ALL.to_vec(),
        root,
        selector,
        requirements: Vec::new(),
        eligibility: Vec::new(),
        operation: Operation::Report,
        recent_days: None,
    }
}

/// A rule selected by an integration selector, which these tests never evaluate.
fn any_rule(integration: &'static str) -> Rule {
    rule(
        integration,
        SymbolicRoot::XdgCache,
        Selector::Integration("test"),
    )
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
            roots: &[SymbolicRoot::XdgCache],
            ..descriptor(self.id)
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
            let path = PathBuf::from(format!("/found/{}/{n:05}", self.id));
            sink.add(&any_rule(self.id), &path, Vec::new(), None)?;
        }
        Ok(())
    }
}

/// A request in which the emitters' root exists.
fn found() -> ScanRequest {
    let mut roots = Roots::default();
    insert(&mut roots, SymbolicRoot::XdgCache, FOUND);
    ScanRequest {
        roots,
        lens: None,
        exclude: Vec::new(),
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn engine(sizer: FakeSizer, integrations: Vec<Box<dyn Integration>>) -> Engine {
    Engine::new(ports(FakeFs::default(), sizer), integrations, Vec::new()).unwrap()
}

/// Runs the scan on its own thread, so the test can act as a stalled or departing consumer.
fn spawn_scan(
    engine: Engine,
    request: ScanRequest,
    capacity: usize,
    cancel: &CancelToken,
) -> (EventRx, mpsc::Receiver<ScanReport>) {
    let (events, rx) = event_channel(capacity, cancel);
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
        if let ScanEvent::FindingAdded { finding } = &sequenced.event {
            assert!(added.insert(finding.id));
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

#[test]
fn a_consumer_that_goes_away_during_sizing_stops_the_walk_in_progress() {
    let sizing = Arc::new(Barrier::new(2));
    let sizer = FakeSizer {
        block_first: Some(Arc::clone(&sizing)),
        ..FakeSizer::default()
    };
    let cancel = CancelToken::new();
    let engine = engine(sizer, vec![Box::new(Emitter::new("sizing", 3))]);
    let (events, report) = spawn_scan(engine, found(), 64, &cancel);

    // The walk sends nothing until it finishes, so only the dropped receiver can stop it.
    sizing.wait();
    drop(events);

    let report = report.recv_timeout(ACK_LIMIT).expect("walk kept running");
    assert_eq!(report.outcome, ScanOutcome::Cancelled);
}

/// Declares every root it is given and records what the engine let it read: for each
/// path in `probe`, how listing it and adding it as a finding went.
#[derive(Debug)]
struct RootReader {
    roots: &'static [SymbolicRoot],
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
            roots: self.roots,
            ..descriptor("roots")
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
                let add = match sink.add(&any_rule("roots"), path, Vec::new(), None) {
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
    roots.insert(root, Ok(ResolvedRoot::new(root, path, source)));
}

fn run(engine: &Engine, request: &ScanRequest) -> ScanReport {
    let cancel = CancelToken::new();
    let (events, rx) = event_channel(1024, &cancel);
    let report = engine.scan(request, &events, &cancel);
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
        ..FakeFs::default()
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
        roots: &[
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
        ports(fs, FakeSizer::default()),
        vec![Box::new(reader)],
        Vec::new(),
    )
    .unwrap();

    let report = run(
        &engine,
        &ScanRequest {
            roots,
            lens: None,
            exclude: Vec::new(),
        },
    );

    assert_eq!(*seen.lock().unwrap(), [PathBuf::from("/home")]);
    let omissions = &report.integrations[0].omissions;
    assert!(matches!(
        omissions.as_slice(),
        [
            Omission::RootUnresolved { root: SymbolicRoot::XdgConfig, .. },
            Omission::RootUnreadable { root: SymbolicRoot::XdgCache, error: stat },
            Omission::RootNotDirectory { root: SymbolicRoot::XdgState, kind: EntryKind::Symlink, .. },
            Omission::RootUnreadable { root: SymbolicRoot::LocalAppData, error: list },
        ] if stat.io_kind() == Some(io::ErrorKind::PermissionDenied)
            && list.io_kind() == Some(io::ErrorKind::PermissionDenied)
    ));
}

#[test]
fn an_integration_can_neither_probe_nor_report_anything_outside_its_roots() {
    let mut roots = Roots::default();
    insert(&mut roots, SymbolicRoot::XdgCache, "/home/u/.cache");
    let fs = FakeFs {
        entries: BTreeMap::from([
            // Above the root, so it never matters: on macOS `/var` is a link.
            (PathBuf::from("/home/u"), Ok(EntryKind::Symlink)),
            (PathBuf::from("/home/u/.cache/link"), Ok(EntryKind::Symlink)),
            // Beyond the link: probing it would already reach outside the root.
            (
                PathBuf::from("/home/u/.cache/link/deep"),
                Err(io::ErrorKind::PermissionDenied),
            ),
        ]),
        ..FakeFs::default()
    };
    let probes = Arc::default();
    let reader = RootReader {
        roots: &[SymbolicRoot::XdgCache],
        seen: Arc::default(),
        probes: Arc::clone(&probes),
        probe: [
            "/home/u/.cache",
            "/home/u/.cache/tool",
            "/home/u",
            "/home/u/.cache-other",
            "/home/u/.cache/../.ssh",
            "/home/u/.cache/link/inner",
            "/home/u/.cache/link/deep/file",
        ]
        .map(PathBuf::from)
        .to_vec(),
    };
    let engine = Engine::new(
        ports(fs, FakeSizer::default()),
        vec![Box::new(reader)],
        Vec::new(),
    )
    .unwrap();

    let report = run(
        &engine,
        &ScanRequest {
            roots,
            lens: None,
            exclude: Vec::new(),
        },
    );

    assert_eq!(
        *probes.lock().unwrap(),
        [
            ("allowed", "allowed"),
            ("allowed", "allowed"),
            ("outside", "outside"),
            ("outside", "outside"),
            ("outside", "outside"),
            ("link", "link"),
            ("link", "link"),
        ]
    );
    let found: Vec<_> = report.findings.iter().map(|f| f.path.clone()).collect();
    assert_eq!(
        found,
        [
            PathBuf::from("/home/u/.cache"),
            PathBuf::from("/home/u/.cache/tool")
        ]
    );
}

/// Adds one finding, then panics.
#[derive(Debug)]
struct Crashes;

impl Integration for Crashes {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            roots: &[SymbolicRoot::XdgCache],
            ..descriptor("crashes")
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
        let path = Path::new("/found/crashes/00000");
        sink.add(&any_rule("crashes"), path, Vec::new(), None)?;
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

    // Compared as paths: on Windows the engine reports them with `\` separators.
    let found: Vec<_> = report.findings.iter().map(|f| f.path.as_path()).collect();
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
        .map(Path::new)
    );
    let ids: Vec<_> = report.integrations.iter().map(|i| i.id.0).collect();
    assert_eq!(ids, ["alpha", "crashes", "zeta"]);
    assert!(matches!(
        report.integrations[1].omissions.as_slice(),
        [Omission::Crashed]
    ));
    assert_eq!(report.outcome, ScanOutcome::Completed);
}

/// A root resolved through a link to the directory with `identity` at `path` (D-041).
fn insert_linked(roots: &mut Roots, root: SymbolicRoot, path: &str, identity: FileIdentity) {
    let mut resolved = ResolvedRoot::new(root, PathBuf::from(path), RootSource::Env("TEST"));
    resolved.link = Some(RootLink {
        configured: PathBuf::from("/configured").join(root.name()),
        identity,
    });
    roots.insert(root, Ok(resolved));
}

#[test]
fn a_root_resolved_through_a_link_is_read_only_while_it_leads_to_the_same_directory() {
    let resolved = FileIdentity { volume: 1, file: 1 };
    let replaced = FileIdentity { volume: 1, file: 2 };
    let fs = FakeFs {
        entries: BTreeMap::from([(PathBuf::from("/gone"), Err(io::ErrorKind::NotFound))]),
        identities: BTreeMap::from([
            (PathBuf::from("/same"), resolved),
            (PathBuf::from("/swapped"), replaced),
        ]),
        ..FakeFs::default()
    };
    let mut roots = Roots::default();
    insert_linked(&mut roots, SymbolicRoot::XdgCache, "/same", resolved);
    insert_linked(&mut roots, SymbolicRoot::XdgData, "/swapped", resolved);
    insert_linked(&mut roots, SymbolicRoot::XdgState, "/gone", resolved);
    let seen = Arc::default();
    let reader = RootReader {
        roots: &[
            SymbolicRoot::XdgCache,
            SymbolicRoot::XdgData,
            SymbolicRoot::XdgState,
        ],
        seen: Arc::clone(&seen),
        probes: Arc::default(),
        probe: Vec::new(),
    };
    let engine = Engine::new(
        ports(fs, FakeSizer::default()),
        vec![Box::new(reader)],
        Vec::new(),
    )
    .unwrap();

    let report = run(
        &engine,
        &ScanRequest {
            roots,
            lens: None,
            exclude: Vec::new(),
        },
    );

    assert_eq!(*seen.lock().unwrap(), [PathBuf::from("/same")]);
    assert!(matches!(
        report.integrations[0].omissions.as_slice(),
        [
            Omission::RootChanged {
                root: SymbolicRoot::XdgData,
                ..
            },
            Omission::RootChanged {
                root: SymbolicRoot::XdgState,
                ..
            },
        ]
    ));
}

/// Reports every candidate of its `project-artifact` rules, with the markers beside it.
#[derive(Debug)]
struct ProjectFinder {
    id: &'static str,
    artifacts: &'static [&'static str],
}

impl Integration for ProjectFinder {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            artifacts: self.artifacts,
            ..descriptor(self.id)
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        for rule in cx.rules() {
            for stop in cx.project_artifacts(rule)? {
                let evidence = stop.markers.into_iter().map(Evidence::Marker).collect();
                sink.add(rule, &stop.path, evidence, None)?;
            }
        }
        Ok(())
    }
}

fn project_rule(integration: &'static str, marker: &str, artifact: &str) -> Rule {
    let selector = Selector::ProjectArtifact {
        markers: vec![Name(marker.to_owned())],
        artifact: Name(artifact.to_owned()),
        max_depth: 3,
    };
    rule(integration, SymbolicRoot::ProjectRoots, selector)
}

#[test]
fn project_artifacts_are_found_in_one_walk_that_never_looks_inside_one_or_goes_too_deep() {
    let mut fs = FakeFs::tree(&[
        "/p/a/Cargo.toml",
        "/p/a/target/",
        // Inside a candidate: never looked for.
        "/p/a/target/nested/Cargo.toml",
        "/p/a/target/nested/target/",
        "/p/b/package.json",
        "/p/b/node_modules/",
        "/p/b/node_modules/x/package.json",
        "/p/b/node_modules/x/node_modules/",
        // No marker beside it, so not a candidate: walked into like any directory.
        "/p/c/target/",
        "/p/c/target/Cargo.toml",
        "/p/c/target/target/",
        // Always node's, even without a marker: never walked into.
        "/p/e/node_modules/",
        "/p/e/node_modules/y/package.json",
        "/p/e/node_modules/y/node_modules/",
        // Deeper than `max_depth`.
        "/p/x/y/Cargo.toml",
        "/p/x/y/target/",
        "/p/x/y/z/Cargo.toml",
        "/p/x/y/z/target/",
        "/p/locked/",
    ]);
    fs.unlistable.insert(PathBuf::from("/p/locked"));
    let listed = Arc::clone(&fs.listed);
    let mut roots = Roots::default();
    insert(&mut roots, SymbolicRoot::ProjectRoots, "/p");
    let integrations: Vec<Box<dyn Integration>> = vec![
        Box::new(ProjectFinder {
            id: "cargo",
            artifacts: &[],
        }),
        Box::new(ProjectFinder {
            id: "node",
            artifacts: &["node_modules"],
        }),
    ];
    let rules = vec![
        project_rule("cargo", "Cargo.toml", "target"),
        project_rule("node", "package.json", "node_modules"),
    ];
    let engine = Engine::new(ports(fs, FakeSizer::default()), integrations, rules).unwrap();

    let report = run(
        &engine,
        &ScanRequest {
            roots,
            lens: None,
            exclude: Vec::new(),
        },
    );

    let found: Vec<_> = report
        .findings
        .iter()
        .map(|f| (f.integration.0, f.path.as_path(), f.evidence.as_slice()))
        .collect();
    let marker = |name: &str| vec![Evidence::Marker(name.to_owned())];
    let (cargo, node) = (marker("Cargo.toml"), marker("package.json"));
    assert_eq!(
        found,
        [
            ("cargo", Path::new("/p/a/target"), cargo.as_slice()),
            ("cargo", Path::new("/p/c/target/target"), cargo.as_slice()),
            ("cargo", Path::new("/p/x/y/target"), cargo.as_slice()),
            ("node", Path::new("/p/b/node_modules"), node.as_slice()),
        ]
    );
    let listed = listed.lock().unwrap();
    let unique: BTreeSet<_> = listed.iter().collect();
    assert_eq!(unique.len(), listed.len(), "a directory was walked twice");
    for skipped in ["/p/a/target", "/p/e/node_modules", "/p/x/y/z"] {
        assert!(
            !unique.contains(&PathBuf::from(skipped)),
            "{skipped} walked"
        );
    }
    for integration in &report.integrations {
        assert!(matches!(
            integration.omissions.as_slice(),
            [Omission::Unlisted { error }] if error.io_kind() == Some(io::ErrorKind::PermissionDenied)
        ));
    }
}

#[test]
fn a_rule_whose_integration_is_not_registered_is_refused_instead_of_never_running() {
    let built = Engine::new(
        ports(FakeFs::default(), FakeSizer::default()),
        vec![Box::new(Emitter::new("cargo", 0))],
        vec![any_rule("cargo"), any_rule("node")],
    );

    assert!(matches!(
        built,
        Err(EngineError::NoIntegration {
            integration: IntegrationId("node"),
            ..
        })
    ));
}

/// Proposes removing each of `paths`, recognised and with a known consequence, under a
/// `remove-files` rule of its own.
#[derive(Debug)]
struct Proposer {
    paths: &'static [&'static str],
}

impl Proposer {
    fn rule() -> Rule {
        Rule {
            operation: Operation::RemoveFiles(vec![Method::Delete]),
            ..rule(
                "proposer",
                SymbolicRoot::XdgCache,
                Selector::Integration("test"),
            )
        }
    }
}

impl Integration for Proposer {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            roots: &[SymbolicRoot::XdgCache],
            ..descriptor("proposer")
        }
    }

    fn discover(
        &self,
        _: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let consequence = Consequence {
            recovery: Recovery::RebuildLocal,
            loss: &[],
            cost: Cost::Unknown,
        };
        for path in self.paths {
            let evidence = vec![Evidence::Signature("TAG".to_owned())];
            sink.add(&Self::rule(), Path::new(path), evidence, Some(consequence))?;
        }
        Ok(())
    }
}

/// Scans what `proposer` proposes under `/found` with `ports`, and returns each finding's
/// facts by path.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn facts(ports: Ports, proposer: Proposer) -> BTreeMap<PathBuf, Facts> {
    let engine = Engine::new(ports, vec![Box::new(proposer)], vec![Proposer::rule()]).unwrap();
    let report = run(&engine, &found());
    report
        .findings
        .into_iter()
        .map(|finding| (finding.path, finding.facts.unwrap()))
        .collect()
}

#[test]
fn a_git_directory_in_or_above_a_finding_leaves_what_it_tracks_unknown() {
    let fs = FakeFs {
        no_git: true,
        entries: BTreeMap::from([
            (PathBuf::from("/found/repo/.git"), Ok(EntryKind::File)),
            (PathBuf::from("/found/self/.git"), Ok(EntryKind::Dir)),
            (
                PathBuf::from("/found/locked/.git"),
                Err(io::ErrorKind::PermissionDenied),
            ),
        ]),
        ..FakeFs::default()
    };
    let proposer = Proposer {
        paths: &[
            "/found/repo/target",
            "/found/self",
            "/found/locked/target",
            "/found/plain/target",
        ],
    };

    let facts = facts(ports(fs, FakeSizer::default()), proposer);

    let tracked = |path: &str| &facts[Path::new(path)].tracked;
    assert!(matches!(
        tracked("/found/repo/target"),
        Tracked::InsideRepository { work_tree } if work_tree == Path::new("/found/repo")
    ));
    assert!(matches!(
        tracked("/found/self"),
        Tracked::InsideRepository { work_tree } if work_tree == Path::new("/found/self")
    ));
    assert!(matches!(
        tracked("/found/locked/target"),
        Tracked::Unchecked(error) if error.io_kind() == Some(io::ErrorKind::PermissionDenied)
    ));
    assert!(matches!(
        tracked("/found/plain/target"),
        Tracked::NoRepository
    ));
}

#[test]
fn a_program_running_from_a_finding_marks_it_in_use_and_any_unidentified_one_unknown() {
    let identified = |pid, path: &str| {
        let outcome = ProcessOutcome::Identified {
            path: PathBuf::from(path),
            identity: FileIdentity {
                volume: 1,
                file: u64::from(pid),
            },
            deleted: false,
        };
        ProcessRecord::new(pid, outcome)
    };
    let denied = ProcessRecord::new(
        1,
        ProcessOutcome::Denied(Arc::new(io::ErrorKind::PermissionDenied.into())),
    );
    let snapshot = |processes, inventory| {
        FakeProcesses(Ok(ProcessSnapshot {
            processes,
            inventory,
        }))
    };
    let activity = |processes: FakeProcesses| {
        let ports = Ports {
            processes: Box::new(processes),
            ..ports(FakeFs::default(), FakeSizer::default())
        };
        let proposer = Proposer {
            paths: &["/found/app/target"],
        };
        facts(ports, proposer)
            .remove(Path::new("/found/app/target"))
            .map(|facts| facts.activity)
    };
    let running = vec![
        identified(7, "/usr/bin/cargo"),
        identified(4121, "/found/app/target/debug/app"),
        denied.clone(),
    ];
    let partial = Inventory::Partial {
        reason: "hidepid=2".to_owned(),
    };
    let unlisted = FakeProcesses(Err(ProbeError::io(
        Path::new("/proc"),
        io::ErrorKind::PermissionDenied.into(),
    )));

    assert!(matches!(
        activity(snapshot(running, Inventory::Complete)),
        Some(Activity::InUse { pid: 4121, .. })
    ));
    assert!(matches!(
        activity(snapshot(
            vec![identified(7, "/usr/bin/cargo")],
            Inventory::Complete
        )),
        Some(Activity::Idle)
    ));
    assert!(matches!(
        activity(snapshot(vec![denied], Inventory::Complete)),
        Some(Activity::Unknown(ActivityGap::Uninspected(1)))
    ));
    assert!(matches!(
        activity(snapshot(Vec::new(), partial)),
        Some(Activity::Unknown(ActivityGap::Partial(_)))
    ));
    assert!(matches!(
        activity(unlisted),
        Some(Activity::Unknown(ActivityGap::NoListing(_)))
    ));
}

#[test]
fn only_a_complete_measurement_dates_a_finding_and_a_future_time_counts_as_now() {
    let modified = |completeness, at: SystemTime| {
        let sizer = FakeSizer {
            measurement: Some(Measurement {
                size: SpaceEstimate {
                    completeness,
                    ..SIZE
                },
                modified: Some(at),
                repository: None,
            }),
            ..FakeSizer::default()
        };
        let clock = Box::new(FixedClock);
        let ports = Ports {
            clock,
            ..ports(FakeFs::default(), sizer)
        };
        let proposer = Proposer {
            paths: &["/found/target"],
        };
        facts(ports, proposer)
            .remove(Path::new("/found/target"))
            .map(|facts| facts.modified)
    };
    let day = Duration::from_hours(24);
    let incomplete = Completeness::Incomplete {
        unreadable: 1,
        skipped_mounts: 0,
    };

    assert_eq!(
        modified(Completeness::Complete, NOW - 3 * day),
        Some(Modified::Ago(3 * day))
    );
    assert_eq!(
        modified(Completeness::Complete, NOW + day),
        Some(Modified::Ago(Duration::ZERO))
    );
    assert_eq!(
        modified(incomplete, NOW - 3 * day),
        Some(Modified::Unknown(WalkGap::Unread))
    );
}

#[test]
fn a_checkout_inside_a_finding_holds_it_and_an_incomplete_walk_cant_rule_one_out() {
    let tracked = |measurement: Measurement| {
        let sizer = FakeSizer {
            measurement: Some(measurement),
            ..FakeSizer::default()
        };
        let fs = FakeFs {
            no_git: true,
            ..FakeFs::default()
        };
        let proposer = Proposer {
            paths: &["/found/.venv"],
        };
        facts(ports(fs, sizer), proposer)
            .remove(Path::new("/found/.venv"))
            .map(|facts| facts.tracked)
    };
    let editable = Measurement {
        // `pip install -e git+…` checks a package out inside the venv.
        repository: Some(PathBuf::from("/found/.venv/src/pkg")),
        ..measured(SIZE)
    };
    let unreadable = measured(SpaceEstimate {
        completeness: Completeness::Incomplete {
            unreadable: 1,
            skipped_mounts: 0,
        },
        ..SIZE
    });

    assert!(matches!(
        tracked(editable),
        Some(Tracked::HoldsRepository { work_tree }) if work_tree == Path::new("/found/.venv/src/pkg")
    ));
    // A walk that met a checkout before an unreadable entry shows the specific reason.
    let unreadable_after_checkout = Measurement {
        repository: Some(PathBuf::from("/found/.venv/src/pkg")),
        ..unreadable
    };
    assert!(matches!(
        tracked(unreadable_after_checkout),
        Some(Tracked::HoldsRepository { .. })
    ));
    assert!(matches!(
        tracked(unreadable),
        Some(Tracked::Unwalked(WalkGap::Unread))
    ));
    assert!(matches!(
        tracked(measured(SIZE)),
        Some(Tracked::NoRepository)
    ));
}

#[test]
fn a_mount_the_walk_skipped_inside_a_finding_holds_it_for_git_and_last_change() {
    // A bind mount or btrfs subvolume below the finding could hold a checkout or recent
    // files the walk never saw.
    let sizer = FakeSizer {
        measurement: Some(measured(SpaceEstimate {
            completeness: Completeness::Incomplete {
                unreadable: 0,
                skipped_mounts: 1,
            },
            ..SIZE
        })),
        ..FakeSizer::default()
    };
    let fs = FakeFs {
        no_git: true,
        ..FakeFs::default()
    };
    let proposer = Proposer {
        paths: &["/found/target"],
    };

    let facts = facts(ports(fs, sizer), proposer)
        .remove(Path::new("/found/target"))
        .unwrap();

    // The reason names the mount, not an unreadable entry.
    assert!(matches!(
        facts.tracked,
        Tracked::Unwalked(WalkGap::OtherFileSystem)
    ));
    assert_eq!(facts.modified, Modified::Unknown(WalkGap::OtherFileSystem));
    let rule = RuleId("test.found".to_owned());
    let at = Path::new("/found/target");
    let Verdict::Protected(holds) = Policy::at(Level::Balanced).verdict(&rule, at, &facts) else {
        panic!("a finding with a skipped mount is never Safe or Review");
    };
    assert!(holds.contains(&Hold::UnknownGit));
    assert!(holds.contains(&Hold::UnknownModified));
}
