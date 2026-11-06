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
    Activity, ActivityGap, Advice, AgentLabel, Allocated, Anchor, Apparent, Bounds, CancelToken,
    Category, Checkout, Claim, Clause, Clock, Completeness, Consequence, Cost, DirEntryMeta,
    Engine, EngineError, EntryKind, EntryMeta, EventChannel, EventRx, Evidence, FactKind, FactSpec,
    FactState, FactValue, Facts, FileIdentity, Finding, FindingSink, FrozenTree, FsProbe, GitGap,
    GroupKey, Hold, Integration, IntegrationDescriptor, IntegrationError, IntegrationId, Inventory,
    Lens, Level, Lineage, Measurement, Method, Modified, Name, NativeKind, NodeId, Now, Observer,
    Omission, OpenedRoot, Operation, OwnerStatus, Platform, Policy, Ports, Predicate, ProbeError,
    ProcessOutcome, ProcessProbe, ProcessRecord, ProcessSnapshot, Proposal, Recovery, Registration,
    RegistrationState, ReleaseFacts, ReleaseGroup, ReleaseRole, ReleaseState, ReleaseVersion,
    RepositoryDir, Rescan as _, ResolvedRoot, Retention, RootLink, RootSource, Roots, Rule, RuleId,
    RuleText, ScanContext, ScanEvent, ScanOutcome, ScanReport, ScanRequest, Selector,
    SemanticFacts, Settlement, Sizer, Skip, SpaceEstimate, Subject, Supplied, SymbolicRoot, Test,
    ToolGap, Tracked, TrackedQuery, UnresolvedRoot, Verdict, WalkGap, WorktreeFacts, event_channel,
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
        nested_repository: None,
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
        observer: Observer::PLAIN,
    }))
}

/// The engine's ports over `fs` and `sizer`, on a host that runs nothing, at [`NOW`].
fn ports(fs: FakeFs, sizer: FakeSizer) -> Ports {
    Ports {
        fs: Box::new(fs),
        sizer: Box::new(sizer),
        processes: Box::new(no_processes()),
        clock: Box::new(FixedClock),
        tracked: Box::new(FakeGit(Err(GitGap::Unsupported))),
        tools: Box::new(kipple_core::Unavailable),
        worktrees: Box::new(kipple_core::Unavailable),
    }
}

/// git, answering every count with this.
#[derive(Debug)]
struct FakeGit(Result<u64, GitGap>);

impl TrackedQuery for FakeGit {
    fn count(&self, _: &Path, _: &Path, _: &CancelToken) -> Result<u64, GitGap> {
        self.0
    }

    fn entries(
        &self,
        _: &Path,
        _: &Anchor,
        _: &FrozenTree,
        _: &CancelToken,
    ) -> Result<Vec<NodeId>, GitGap> {
        self.0.map(|_| Vec::new())
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
        consequences: &[],
        facts: &[],
        state: &[],
        evidences: &[],
        revision: 1,
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
    let EventChannel { tx: events, rx } = event_channel(capacity, cancel);
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
            assert!(added.insert(finding.id), "a finding is added once");
        }
        if let ScanEvent::FindingUpdated { id, .. } = &sequenced.event {
            assert!(added.contains(id), "{id} sized before it was added");
            assert!(sized.insert(*id), "a finding is sized once");
        }
        last = Some(sequenced.event);
    }

    assert_eq!((added.len(), sized.len()), (count, count));
    assert!(
        matches!(
            last,
            Some(ScanEvent::ScanCompleted {
                outcome: ScanOutcome::Completed
            })
        ),
        "the last event is a completed scan"
    );
    let report = report.recv_timeout(ACK_LIMIT).unwrap();
    assert_eq!(report.outcome, ScanOutcome::Completed);
    assert!(
        report.findings.iter().all(|f| f.size == Some(SIZE)),
        "every finding carries its size"
    );
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
    assert!(
        cancel.is_cancelled(),
        "a consumer that went away cancels the scan"
    );
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
    let EventChannel { tx: events, rx } = event_channel(1024, &cancel);
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
    assert!(
        matches!(
            omissions.as_slice(),
            [
                Omission::RootUnresolved { root: SymbolicRoot::XdgConfig, .. },
                Omission::RootUnreadable { root: SymbolicRoot::XdgCache, error: stat },
                Omission::RootNotDirectory { root: SymbolicRoot::XdgState, kind: EntryKind::Symlink, .. },
                Omission::RootUnreadable { root: SymbolicRoot::LocalAppData, error: list },
            ] if stat.io_kind() == Some(io::ErrorKind::PermissionDenied)
                && list.io_kind() == Some(io::ErrorKind::PermissionDenied)
        ),
        "the unresolved and unreadable roots are omissions"
    );
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
    assert!(
        matches!(
            report.integrations[1].omissions.as_slice(),
            [Omission::Crashed]
        ),
        "the crashed integration has a crash omission"
    );
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
    assert!(
        matches!(
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
        ),
        "a root leading somewhere else is an omission"
    );
    // What a plan anchors at: only the root that opened, with the identity it had then.
    assert_eq!(
        report.integrations[0].opened,
        [OpenedRoot {
            root: SymbolicRoot::XdgCache,
            path: PathBuf::from("/same"),
            identity: Some(resolved),
        }]
    );
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

/// An engine with the cargo and node project rules over `fs`.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn project_engine(fs: FakeFs) -> Engine {
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
    Engine::new(ports(fs, FakeSizer::default()), integrations, rules).unwrap()
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
    let engine = project_engine(fs);

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
        assert!(
            matches!(
                integration.omissions.as_slice(),
                [Omission::Unlisted { error }] if error.io_kind() == Some(io::ErrorKind::PermissionDenied)
            ),
            "a denied listing is the only omission"
        );
    }
}

#[test]
fn a_rule_whose_integration_is_not_registered_is_refused_instead_of_never_running() {
    let built = Engine::new(
        ports(FakeFs::default(), FakeSizer::default()),
        vec![Box::new(Emitter::new("cargo", 0))],
        vec![any_rule("cargo"), any_rule("node")],
    );

    assert!(
        matches!(
            built,
            Err(EngineError::NoIntegration {
                integration: IntegrationId("node"),
                ..
            })
        ),
        "a rule for an unregistered integration is refused"
    );
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

/// A git fixture: a repository above one finding, one finding that is a work tree, one
/// whose `.git` can't be read and one with none.
fn git_fs() -> FakeFs {
    FakeFs {
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
    }
}

const GIT_FINDINGS: Proposer = Proposer {
    paths: &[
        "/found/repo/target",
        "/found/self",
        "/found/locked/target",
        "/found/plain/target",
    ],
};

/// Catches a finding inside a repository judged without git's answer, or a work tree, an
/// unreadable `.git` or a failed query taken as untracked (D-066; spec M3b pinned 13, 16).
#[test]
fn git_answers_for_a_finding_inside_a_repository_and_nothing_else_is_asked() {
    let answered = Ports {
        tracked: Box::new(FakeGit(Ok(3))),
        ..ports(git_fs(), FakeSizer::default())
    };
    let found = facts(answered, GIT_FINDINGS);
    let tracked = |path: &str| &found[Path::new(path)].tracked;
    assert!(
        matches!(
            tracked("/found/repo/target"),
            Tracked::Asked { work_tree, tracked: 3 } if work_tree == Path::new("/found/repo")
        ),
        "a target inside a repository carries git's answer, got {:?}",
        tracked("/found/repo/target")
    );
    assert!(
        matches!(tracked("/found/self"), Tracked::HoldsRepository { work_tree } if work_tree == Path::new("/found/self")),
        "a finding that is a work tree holds a repository, got {:?}",
        tracked("/found/self")
    );
    assert!(
        matches!(
            tracked("/found/locked/target"),
            Tracked::Unchecked(error) if error.io_kind() == Some(io::ErrorKind::PermissionDenied)
        ),
        "an unreadable git directory leaves tracking unchecked"
    );
    assert!(
        matches!(tracked("/found/plain/target"), Tracked::NoRepository),
        "a target with no git directory has no repository"
    );

    let failed = facts(ports(git_fs(), FakeSizer::default()), GIT_FINDINGS);
    assert!(
        matches!(
            &failed[Path::new("/found/repo/target")].tracked,
            Tracked::InsideRepository {
                gap: GitGap::Unsupported,
                ..
            }
        ),
        "a query git couldn't answer leaves the repository's files unknown"
    );
}

/// A process the probe identified as running `path`.
fn identified(pid: u32, path: &str) -> ProcessRecord {
    let outcome = ProcessOutcome::Identified {
        path: PathBuf::from(path),
        identity: FileIdentity {
            volume: 1,
            file: u128::from(pid),
        },
        deleted: false,
    };
    // Everything else it could show is read too, since the proposer declares no
    // narrower evidence: it was started by its own path.
    ProcessRecord {
        cwd: Ok(PathBuf::from("/")),
        argv: Ok(vec![path.into()]),
        env_paths: Ok(Vec::new()),
        mapped_files: Ok(Vec::new()),
        ..ProcessRecord::new(pid, outcome)
    }
}

/// The activity of a finding at `/found/app/target` among `processes`.
fn activity(processes: FakeProcesses) -> Option<Activity> {
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
}

#[test]
fn a_program_running_from_a_finding_marks_it_in_use_and_any_unidentified_one_unknown() {
    let denied = ProcessRecord::new(
        1,
        ProcessOutcome::Denied(Arc::new(io::ErrorKind::PermissionDenied.into())),
    );
    let snapshot = |processes, inventory| {
        FakeProcesses(Ok(ProcessSnapshot {
            processes,
            inventory,
            observer: Observer::PLAIN,
        }))
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

    assert!(
        matches!(
            activity(snapshot(running, Inventory::Complete)),
            Some(Activity::InUse { pid: 4121, .. })
        ),
        "a program running from the finding marks it in use"
    );
    assert!(
        matches!(
            activity(snapshot(
                vec![identified(7, "/usr/bin/cargo")],
                Inventory::Complete
            )),
            Some(Activity::Idle)
        ),
        "a program running from elsewhere leaves the finding idle"
    );
    assert!(
        matches!(
            activity(snapshot(vec![denied], Inventory::Complete)),
            Some(Activity::Unknown(ActivityGap::Uninspected(1)))
        ),
        "a denied process leaves the finding unknown"
    );
    assert!(
        matches!(
            activity(snapshot(Vec::new(), partial)),
            Some(Activity::Unknown(ActivityGap::Partial(_)))
        ),
        "a partial inventory leaves the finding unknown"
    );
    assert!(
        matches!(
            activity(unlisted),
            Some(Activity::Unknown(ActivityGap::NoListing(_)))
        ),
        "a process listing that failed leaves the finding unknown"
    );
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
                nested_repository: None,
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
        nested_repository: Some(PathBuf::from("/found/.venv/src/pkg")),
        ..measured(SIZE)
    };
    let unreadable = measured(SpaceEstimate {
        completeness: Completeness::Incomplete {
            unreadable: 1,
            skipped_mounts: 0,
        },
        ..SIZE
    });

    assert!(
        matches!(
            tracked(editable),
            Some(Tracked::HoldsRepository { work_tree }) if work_tree == Path::new("/found/.venv/src/pkg")
        ),
        "a checkout inside the finding holds it"
    );
    // A walk that met a checkout before an unreadable entry shows the specific reason.
    let unreadable_after_checkout = Measurement {
        repository: Some(PathBuf::from("/found/.venv/src/pkg")),
        nested_repository: Some(PathBuf::from("/found/.venv/src/pkg")),
        ..unreadable
    };
    assert!(
        matches!(
            tracked(unreadable_after_checkout),
            Some(Tracked::HoldsRepository { .. })
        ),
        "a checkout met before an unreadable entry still holds the finding"
    );
    assert!(
        matches!(
            tracked(unreadable),
            Some(Tracked::Unwalked(WalkGap::Unread))
        ),
        "an unread walk leaves tracking unwalked"
    );
    assert!(
        matches!(tracked(measured(SIZE)), Some(Tracked::NoRepository)),
        "a complete walk without a checkout finds no repository"
    );
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
    assert!(
        matches!(facts.tracked, Tracked::Unwalked(WalkGap::OtherFileSystem)),
        "a skipped mount leaves tracking unwalked for that reason"
    );
    assert_eq!(facts.modified, Modified::Unknown(WalkGap::OtherFileSystem));
    let rule = RuleId("test.found".to_owned());
    let at = Path::new("/found/target");
    let Verdict::Protected(holds) = Policy::at(Level::Balanced).verdict(&rule, at, &facts) else {
        panic!("a finding with a skipped mount is never Safe or Review");
    };
    assert!(
        holds.contains(&Hold::UnknownGit),
        "a skipped mount holds the finding for git"
    );
    assert!(
        holds.contains(&Hold::UnknownModified),
        "a skipped mount holds the finding for last change"
    );
}

#[test]
fn a_complete_walk_with_an_entry_without_a_time_leaves_only_the_last_change_unknown() {
    // Every entry was read, so git is settled; only the time is missing, and the
    // reason says so instead of claiming something couldn't be read.
    let sizer = FakeSizer {
        measurement: Some(Measurement {
            modified: None,
            ..measured(SIZE)
        }),
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

    assert!(
        matches!(facts.tracked, Tracked::NoRepository),
        "a complete walk finds no repository even without an entry time"
    );
    assert_eq!(facts.modified, Modified::Unknown(WalkGap::Untimed));
}

/// A fact of the [`Supplier`] integration.
const DIALECT: FactSpec = FactSpec {
    name: "test.dialect",
    kind: FactKind::Enum(&["old", "new"]),
    selector: None,
    meaning: "which dialect it speaks",
};

/// Adds one finding at `/found/item` under a rule with this operation and these
/// predicates, supplying `values`, and advice when `advice` says to.
#[derive(Debug)]
struct Supplier {
    operation: Operation,
    requirements: Vec<Predicate>,
    eligibility: Vec<Predicate>,
    values: BTreeMap<&'static str, FactValue>,
    advice: Advised,
    subject: Subject,
}

/// Whether a [`Supplier`] gives advice, and whether its finding names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Advised {
    Nothing,
    OnTheSide,
    ForTheFinding,
}

impl Supplier {
    fn eligible(fact: &'static str, test: Test) -> Self {
        Self {
            operation: Operation::RemoveFiles(vec![Method::Delete]),
            requirements: Vec::new(),
            eligibility: vec![Predicate { fact, test }],
            values: BTreeMap::new(),
            advice: Advised::Nothing,
            subject: Subject::Plain,
        }
    }

    /// The same, about a worktree at `/found/item`.
    fn of_a_worktree(self) -> Self {
        Self {
            subject: worktree("/found/item"),
            ..self
        }
    }

    fn with(mut self, fact: &'static str, value: FactValue) -> Self {
        self.values.insert(fact, value);
        self
    }

    fn rule(&self) -> Rule {
        Rule {
            requirements: self.requirements.clone(),
            eligibility: self.eligibility.clone(),
            operation: self.operation.clone(),
            ..any_rule("supplier")
        }
    }

    /// The facts its one finding gets.
    #[expect(
        clippy::unwrap_used,
        reason = "a fixture that fails to build fails the test"
    )]
    fn scan(self) -> Facts {
        let rule = self.rule();
        let fs = FakeFs {
            no_git: true,
            ..FakeFs::default()
        };
        let ports = ports(fs, FakeSizer::default());
        let engine = Engine::new(ports, vec![Box::new(self)], vec![rule]).unwrap();
        run(&engine, &found())
            .findings
            .into_iter()
            .next()
            .and_then(|finding| finding.facts)
            .unwrap()
    }
}

impl Integration for Supplier {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            roots: &[SymbolicRoot::XdgCache],
            facts: &[DIALECT],
            ..descriptor("supplier")
        }
    }

    fn discover(
        &self,
        _: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let advice = (self.advice != Advised::Nothing).then(|| {
            sink.advise(Advice {
                title: "Ask the tool".to_owned(),
                text: String::new(),
                snippets: Vec::new(),
                references: Vec::new(),
            })
        });
        let claim = Claim {
            evidence: vec![Evidence::Signature("TAG".to_owned())],
            consequence: Some(Consequence {
                recovery: Recovery::RebuildLocal,
                loss: &[],
                cost: Cost::Unknown,
            }),
        };
        let supplied = Supplied {
            values: self.values.clone(),
            advice: advice.filter(|_| self.advice == Advised::ForTheFinding),
            subject: self.subject.clone(),
            ..sink.supplied()
        };
        sink.add_supplied(&self.rule(), Path::new("/found/item"), claim, supplied)
    }
}

/// The one way the finding's single predicate fell short, if it did.
fn unmet_state(facts: &Facts) -> Option<FactState> {
    assert!(
        facts.unmet.len() <= 1,
        "each of these rules has one predicate, got {:?}",
        facts.unmet
    );
    facts.unmet.first().map(|unmet| unmet.state)
}

fn dialect_is_new() -> Supplier {
    Supplier::eligible(DIALECT.name, Test::OneOf(vec!["new"]))
}

/// What holds the finding, whatever the level.
fn holds(facts: &Facts) -> Vec<Hold> {
    let rule = RuleId("builtin.supplier.test".to_owned());
    match Policy::at(Level::Thorough).verdict(&rule, Path::new("/found/item"), facts) {
        Verdict::Protected(holds) => holds,
        Verdict::Safe | Verdict::Review(_) => Vec::new(),
    }
}

/// Catches a predicate over a fact nobody supplied passing, because a missing value reads
/// as "nothing wrong".
#[test]
fn predicate_on_unsupplied_fact_holds() {
    let facts = dialect_is_new().scan();

    assert_eq!(unmet_state(&facts), Some(FactState::Unknown));
    assert!(
        holds(&facts).contains(&Hold::FactUnknown),
        "expected the unknown fact to hold it, got {:?}",
        holds(&facts)
    );
}

/// Catches a value of another type, or one its spec doesn't list, being compared anyway.
#[test]
fn wrong_fact_type_is_unknown() {
    let cases = [
        ("another type", FactValue::Bool(true)),
        ("a value the spec doesn't list", FactValue::Enum("newest")),
        ("said to be unknown", FactValue::Unknown),
    ];
    for (case, value) in cases {
        let facts = dialect_is_new().with(DIALECT.name, value).scan();
        assert_eq!(unmet_state(&facts), Some(FactState::Unknown), "{case}");
    }
    let test_of_another_type = Supplier::eligible(DIALECT.name, Test::Is(true))
        .with(DIALECT.name, FactValue::Enum("new"))
        .scan();
    assert_eq!(unmet_state(&test_of_another_type), Some(FactState::Unknown));
}

/// Catches the host's "no repository" standing in for git's answer about a worktree, which
/// is a repository: a dirty one would pass "no changes".
#[test]
fn supplied_fact_wins_over_generic() {
    let no_changes = || {
        let none = Bounds {
            min: None,
            max: Some(0),
        };
        Supplier::eligible("git.tracked_changes", Test::Count(none)).of_a_worktree()
    };

    let dirty = no_changes()
        .with("git.tracked_changes", FactValue::Count(3))
        .scan();
    assert_eq!(unmet_state(&dirty), Some(FactState::Failed));
    let clean = no_changes()
        .with("git.tracked_changes", FactValue::Count(0))
        .scan();
    assert_eq!(unmet_state(&clean), None);
}

const fn at_least<T>(min: T) -> Bounds<T> {
    Bounds {
        min: Some(min),
        max: None,
    }
}

/// Catches an integration talking its finding past a rule's stricter predicate: what the
/// host measured stands, on any subject, and what git status doesn't cover stays the host's.
#[test]
fn a_supplied_value_never_beats_the_host() {
    let long = Duration::from_secs(1 << 40);
    let cases = [
        (
            "size",
            Test::Bytes(at_least(1 << 40)),
            FactValue::Bytes(1 << 41),
        ),
        (
            "modified_age",
            Test::Duration(at_least(long)),
            FactValue::Duration(long),
        ),
        ("marker.present", Test::Is(true), FactValue::Bool(true)),
        (
            "activity",
            Test::OneOf(vec!["busy"]),
            FactValue::Enum("busy"),
        ),
        (
            "git.tracked_files_in_item",
            Test::Count(at_least(1)),
            FactValue::Count(1),
        ),
    ];
    for (fact, test, value) in cases {
        let asked = || Supplier::eligible(fact, test.clone());
        let plain = unmet_state(&asked().scan());
        assert!(plain.is_some(), "expected the host to fail {fact}");
        assert_eq!(
            unmet_state(&asked().with(fact, value).scan()),
            plain,
            "{fact}"
        );

        let checkout = unmet_state(&asked().of_a_worktree().scan());
        assert!(checkout.is_some(), "expected the host to fail {fact}");
        let supplied = asked().of_a_worktree().with(fact, value).scan();
        assert_eq!(unmet_state(&supplied), checkout, "{fact}");
    }
}

/// The git facts, each with the test a clean item passes.
fn git_facts_at_most_zero() -> [(&'static str, Test); 3] {
    let none = || {
        Test::Count(Bounds {
            min: None,
            max: Some(0),
        })
    };
    [
        ("git.tracked_files_in_item", none()),
        ("git.tracked_changes", none()),
        ("git.untracked_files", none()),
    ]
}

/// Catches a worktree outside any repository passing "no changes" when git's answer is
/// missing: the host's "no repository" says nothing about a checkout that is one.
#[test]
fn an_unsupplied_git_fact_of_a_worktree_is_unknown() {
    for (fact, test) in git_facts_at_most_zero() {
        let plain = Supplier::eligible(fact, test.clone()).scan();
        assert_eq!(unmet_state(&plain), None, "{fact}");

        let checkout = Supplier::eligible(fact, test).of_a_worktree().scan();
        assert!(
            matches!(checkout.tracked, Tracked::NoRepository),
            "expected no repository around it, got {:?}",
            checkout.tracked
        );
        assert_eq!(unmet_state(&checkout), Some(FactState::Unknown), "{fact}");
    }
}

/// Catches a fact nothing publishes passing, as a rule of a newer integration would.
#[test]
fn a_fact_nothing_publishes_is_unknown_even_when_supplied() {
    let facts = Supplier::eligible("test.unpublished", Test::Is(true))
        .with("test.unpublished", FactValue::Bool(true))
        .scan();

    assert_eq!(unmet_state(&facts), Some(FactState::Unknown));
}

/// Catches a known answer of the wrong value reading as unknown, or as passed, and a
/// requirement being treated unlike an eligibility clause.
#[test]
fn a_failed_predicate_holds_in_either_clause() {
    let eligibility = dialect_is_new().with(DIALECT.name, FactValue::Enum("old"));
    let requirement = Supplier {
        requirements: eligibility.eligibility.clone(),
        eligibility: Vec::new(),
        ..dialect_is_new().with(DIALECT.name, FactValue::Enum("old"))
    };

    let in_eligibility = eligibility.scan();
    let in_requirements = requirement.scan();

    assert_eq!(in_eligibility.unmet[0].clause, Clause::Eligibility);
    assert_eq!(in_requirements.unmet[0].clause, Clause::Requirement);
    for facts in [in_eligibility, in_requirements] {
        assert!(
            holds(&facts).contains(&Hold::FactUnmet),
            "expected the failed predicate to hold it, got {:?}",
            holds(&facts)
        );
    }
}

/// Catches a passing predicate holding, which would turn every predicate rule off.
#[test]
fn a_met_predicate_changes_nothing() {
    let facts = dialect_is_new()
        .with(DIALECT.name, FactValue::Enum("new"))
        .scan();

    assert_eq!(facts.unmet, Vec::new());
    let found = holds(&facts);
    assert!(
        !found.contains(&Hold::FactUnmet) && !found.contains(&Hold::FactUnknown),
        "expected no predicate to hold it, got {found:?}"
    );
}

/// Catches advice spreading to every report finding of an integration that gave some, which
/// is what a per-integration flag did.
#[test]
fn only_a_finding_that_names_advice_points_to_it() {
    let proposal = |advice| {
        let supplier = Supplier {
            operation: Operation::Report,
            eligibility: Vec::new(),
            advice,
            ..dialect_is_new()
        };
        supplier.scan().proposal
    };

    assert_eq!(proposal(Advised::ForTheFinding), Proposal::Advice);
    assert_eq!(proposal(Advised::OnTheSide), Proposal::Report);
}

/// Catches a report rule's unmet predicate adding holds to a finding nothing removes, which
/// would change the reasons `scan --json` gives for it.
#[test]
fn a_rule_that_only_reports_decides_nothing_by_its_predicates() {
    let facts = Supplier {
        operation: Operation::Report,
        ..dialect_is_new()
    }
    .scan();

    assert_eq!(facts.proposal, Proposal::Report);
    let found = holds(&facts);
    assert_eq!(found, vec![Hold::ReportOnly], "a report rule only reports");
}

/// How [`Owning`]'s discovery ends, after it says its layout is supported.
#[derive(Debug, Clone, Copy)]
enum Ending {
    Finishes,
    Fails,
    Crashes,
}

#[derive(Debug)]
struct Owning(Ending);

impl Integration for Owning {
    fn descriptor(&self) -> IntegrationDescriptor {
        descriptor("owning")
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
        sink.set_owner_status(OwnerStatus::Supported);
        match self.0 {
            Ending::Finishes => Ok(()),
            Ending::Fails => Err(ProbeError::io(
                Path::new("/found"),
                io::ErrorKind::PermissionDenied.into(),
            )
            .into()),
            Ending::Crashes => panic!("integration bug"),
        }
    }
}

/// Catches a discovery that said "supported" and then failed opening its state roots, and
/// an integration the scan didn't run reading as supported.
#[test]
fn an_owner_is_supported_only_if_its_discovery_finishes() {
    let status = |ending| {
        let engine = engine(FakeSizer::default(), vec![Box::new(Owning(ending))]);
        run(&engine, &found())
            .owners()
            .status(IntegrationId("owning"))
    };

    assert_eq!(status(Ending::Finishes), OwnerStatus::Supported);
    assert_eq!(status(Ending::Fails), OwnerStatus::Unsupported);
    assert_eq!(status(Ending::Crashes), OwnerStatus::Unsupported);
    let nothing_ran = engine(FakeSizer::default(), Vec::new());
    assert_eq!(
        run(&nothing_ran, &found())
            .owners()
            .status(IntegrationId("owning")),
        OwnerStatus::Unsupported
    );
}

/// A worktree as git's integration would supply it, with nothing else known.
fn worktree(path: &str) -> Subject {
    Subject::Worktree(Box::new(WorktreeFacts {
        registration: Registration {
            path: PathBuf::from(path),
            head: None,
            checkout: Checkout::Detached,
            locked: None,
            prunable: false,
            bare: false,
        },
        state: RegistrationState::RegisteredPresent,
        repository: PathBuf::from("/found/main"),
        admin: None,
        witness: Err(GitGap::Unsupported),
        tool: Err(ToolGap::Missing),
        agent: AgentLabel::Unknown,
    }))
}

/// Adds one finding under a rule of `operation`, saying it is `subject`.
#[derive(Debug)]
struct Subjected {
    id: &'static str,
    path: &'static str,
    operation: Operation,
    subject: Subject,
}

impl Subjected {
    fn rule(&self) -> Rule {
        Rule {
            operation: self.operation.clone(),
            ..any_rule(self.id)
        }
    }

    #[expect(
        clippy::unwrap_used,
        reason = "a fixture that fails to build fails the test"
    )]
    fn engine(self, ports: Ports) -> Engine {
        let rule = self.rule();
        Engine::new(ports, vec![Box::new(self)], vec![rule]).unwrap()
    }
}

impl Integration for Subjected {
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
        let claim = Claim {
            evidence: vec![Evidence::Worktree {
                agent: AgentLabel::Unknown,
            }],
            consequence: None,
        };
        let supplied = Supplied {
            subject: self.subject.clone(),
            ..sink.supplied()
        };
        sink.add_supplied(&self.rule(), Path::new(self.path), claim, supplied)
    }
}

/// What `Engine::facts` says git tracks of a worktree at `/found/repo/wt`, whose walk found
/// a `.git` at `inside`, under `git` answering for repositories above.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn worktree_tracked(
    id: &'static str,
    deeper: Option<&str>,
    answer: Result<u64, GitGap>,
) -> Tracked {
    let sizer = FakeSizer {
        // As the walker reports a worktree: first its own `.git`, then what is below it.
        measurement: Some(Measurement {
            repository: Some(PathBuf::from("/found/repo/wt")),
            nested_repository: deeper.map(PathBuf::from),
            ..measured(SIZE)
        }),
        ..FakeSizer::default()
    };
    let fs = FakeFs {
        no_git: true,
        entries: BTreeMap::from([
            (PathBuf::from("/found/repo/.git"), Ok(EntryKind::Dir)),
            (PathBuf::from("/found/repo/wt/.git"), Ok(EntryKind::File)),
        ]),
        ..FakeFs::default()
    };
    let ports = Ports {
        tracked: Box::new(FakeGit(answer)),
        ..ports(fs, sizer)
    };
    let subjected = Subjected {
        id,
        path: "/found/repo/wt",
        operation: Operation::Native(NativeKind::GitWorktreeRemove),
        subject: worktree("/found/repo/wt"),
    };
    let report = run(&subjected.engine(ports), &found());
    let finding = report.findings.into_iter().next();
    finding.and_then(|finding| finding.facts).unwrap().tracked
}

/// Catches the exception losing the repositories above, which still answer for the files
/// in the worktree, or covering a repository deeper in it.
#[test]
fn a_worktree_stops_counting_only_its_own_git() {
    assert!(
        matches!(
            worktree_tracked("git", None, Ok(0)),
            Tracked::Asked { tracked: 0, work_tree } if work_tree == Path::new("/found/repo")
        ),
        "the repository above is asked, and the worktree's own `.git` doesn't count"
    );
    assert!(
        matches!(
            worktree_tracked("git", None, Ok(2)),
            Tracked::Asked { tracked: 2, .. }
        ),
        "what the repository above tracks is reported"
    );
    assert!(
        matches!(
            worktree_tracked("git", Some("/found/repo/wt/vendor/lib"), Ok(0)),
            Tracked::HoldsRepository { work_tree } if work_tree == Path::new("/found/repo/wt/vendor/lib")
        ),
        "a repository deeper in the worktree still holds it"
    );
    assert!(
        matches!(
            worktree_tracked("cargo", None, Ok(0)),
            Tracked::HoldsRepository { .. }
        ),
        "any integration but git keeps the protection"
    );
}

/// [`Subjected`]'s finding, read again at apply as `read` says.
#[derive(Debug)]
struct Revisiting {
    subjected: Subjected,
    read: Reading,
}

/// What an integration reads again at apply.
type Reading = fn() -> Result<Option<Supplied>, IntegrationError>;

impl Integration for Revisiting {
    fn descriptor(&self) -> IntegrationDescriptor {
        self.subjected.descriptor()
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        self.subjected.discover(cx, sink)
    }

    fn revisit(
        &self,
        _: &ScanContext<'_>,
        _: &Finding,
    ) -> Result<Option<Supplied>, IntegrationError> {
        (self.read)()
    }
}

const fn subjected(subject: Subject) -> Subjected {
    Subjected {
        id: "git",
        path: "/found/item",
        operation: Operation::Report,
        subject,
    }
}

/// What `engine` says its only finding rests on now, twice over one apply's rescan.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn now_of(engine: &Engine, times: usize) -> Vec<Result<Now, Skip>> {
    let request = found();
    let report = run(engine, &request);
    let finding = report.findings.first().unwrap();
    let rescan = engine.rescan(&request);
    (0..times)
        .map(|_| rescan.now(&report, finding, &CancelToken::new()))
        .collect()
}

fn subjects() -> [Subject; 3] {
    [
        Subject::Release(ReleaseFacts {
            group: ReleaseGroup {
                component: "cli".to_owned(),
                target: "any".to_owned(),
            },
            version: ReleaseVersion("1.0.0".to_owned()),
            role: ReleaseRole::Installed,
            files: Vec::new(),
            locks: Vec::new(),
            state: ReleaseState::Sound,
        }),
        worktree("/found/item"),
        Subject::Semantic(SemanticFacts {
            kind: NativeKind::NpmCacheClean,
            tool: Err(ToolGap::Missing),
        }),
    ]
}

/// Catches a forgotten override passing mid-run on the facts the apply scan read before
/// the item's lock (pinned 13). A plain finding has nothing to read again.
#[test]
fn revisit_default_is_refused_for_release_worktree_and_semantic() {
    let fs = || FakeFs {
        no_git: true,
        ..FakeFs::default()
    };
    for subject in subjects() {
        let engine = subjected(subject).engine(ports(fs(), FakeSizer::default()));
        let refused = now_of(&engine, 1).remove(0).map(|_| ());
        assert_eq!(refused, Err(Skip::RevisitMissing));
    }

    let engine = subjected(Subject::Plain).engine(ports(fs(), FakeSizer::default()));
    assert!(
        now_of(&engine, 1).remove(0).is_ok(),
        "expected a plain finding's facts to stand"
    );
}

/// Catches a reading that failed or crashed taken for one that found nothing changed: the
/// set of items only shrinks (pinned 13).
#[test]
fn a_revisit_that_fails_holds_the_item() {
    let fails: [Reading; 2] = [
        || Err(IntegrationError::Stopped),
        || panic!("a bug in an integration"),
    ];
    for read in fails {
        let revisiting = Revisiting {
            subjected: subjected(Subject::Plain),
            read,
        };
        let rule = revisiting.subjected.rule();
        let fs = FakeFs {
            no_git: true,
            ..FakeFs::default()
        };
        let ports = ports(fs, FakeSizer::default());
        let engine = Engine::new(ports, vec![Box::new(revisiting)], vec![rule]).unwrap();

        let held = now_of(&engine, 1).remove(0).map(|_| ());

        assert_eq!(held, Err(Skip::Protected(vec![Hold::FactUnknown])));
    }
}

/// How many process snapshots a host was asked for.
#[derive(Debug, Default)]
struct Counted(Arc<AtomicUsize>);

impl ProcessProbe for Counted {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        no_processes().snapshot()
    }
}

/// Catches an item that isn't plain judged by a snapshot from before its lock, and a
/// plain one paying for a snapshot per item (pinned 13). Half of
/// `native_snapshot_is_taken_after_the_locks`, whose other half is apply's order.
#[test]
fn a_finding_that_is_not_plain_takes_a_new_snapshot_and_a_plain_one_a_recent_one() {
    let snapshots = |subject, read| {
        let taken = Arc::new(AtomicUsize::new(0));
        let revisiting = Revisiting {
            subjected: subjected(subject),
            read,
        };
        let rule = revisiting.subjected.rule();
        let fs = FakeFs {
            no_git: true,
            ..FakeFs::default()
        };
        let ports = Ports {
            processes: Box::new(Counted(Arc::clone(&taken))),
            ..ports(fs, FakeSizer::default())
        };
        let engine = Engine::new(ports, vec![Box::new(revisiting)], vec![rule]).unwrap();
        let now = now_of(&engine, 2);
        assert!(
            now.iter().all(Result::is_ok),
            "expected both to read: {now:?}"
        );
        taken.load(Ordering::SeqCst)
    };
    let the_scans_own = 1;

    let again = || {
        Ok(Some(Supplied {
            subject: worktree("/found/item"),
            ..Supplied::default()
        }))
    };
    assert_eq!(
        snapshots(worktree("/found/item"), again),
        the_scans_own + 2,
        "one per item"
    );
    assert_eq!(
        snapshots(Subject::Plain, || Ok(None)),
        the_scans_own + 1,
        "one shared while it is recent"
    );
}

/// The events of one scan, in order.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn events_of(engine: &Engine, request: &ScanRequest) -> Vec<ScanEvent> {
    let cancel = CancelToken::new();
    let EventChannel { tx, rx } = event_channel(1024, &cancel);
    let collect = thread::spawn(move || rx.into_iter().map(|one| one.event).collect());
    let _report = engine.scan(request, &tx, &cancel);
    drop(tx);
    collect.join().unwrap()
}

/// Catches a finding that waits for the settle seam never being announced, so a frontend
/// shows it unmeasured for good, and a release reaching one before its group settled.
#[test]
fn a_finding_that_is_not_plain_is_announced_once_and_stays_held() {
    let subjected = Subjected {
        id: "codex",
        path: "/found/releases/1.0.0",
        operation: Operation::RemoveVerifiedRelease {
            methods: vec![Method::Delete],
            retention: Retention {
                group_by: vec![GroupKey::Component],
            },
        },
        subject: Subject::Release(ReleaseFacts {
            group: ReleaseGroup {
                component: "cli".to_owned(),
                target: "any".to_owned(),
            },
            version: ReleaseVersion("1.0.0".to_owned()),
            role: ReleaseRole::Installed,
            files: Vec::new(),
            locks: Vec::new(),
            state: ReleaseState::Sound,
        }),
    };
    let engine = subjected.engine(ports(FakeFs::default(), FakeSizer::default()));

    let events = events_of(&engine, &found());

    let updates: Vec<&Facts> = events
        .iter()
        .filter_map(|event| {
            if let ScanEvent::FindingUpdated { facts, .. } = event {
                Some(facts)
            } else {
                None
            }
        })
        .collect();
    let [facts] = updates.as_slice() else {
        panic!("one update, got {updates:?}");
    };
    assert_eq!(facts.settlement, Settlement::Unsettled);
    assert!(
        matches!(events.last(), Some(ScanEvent::ScanCompleted { .. })),
        "the scan ends last, got {:?}",
        events.last()
    );
}

/// Adds a finding through `add_unread` under a rule of `operation`, which is outside its
/// roots.
#[derive(Debug)]
struct Unreading {
    operation: Operation,
    result: Arc<Mutex<Option<bool>>>,
}

impl Unreading {
    fn rule(&self) -> Rule {
        Rule {
            operation: self.operation.clone(),
            ..any_rule("unreading")
        }
    }
}

impl Integration for Unreading {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            roots: &[SymbolicRoot::XdgCache],
            ..descriptor("unreading")
        }
    }

    #[expect(
        clippy::unwrap_used,
        clippy::unwrap_in_result,
        reason = "a fake whose lock is poisoned fails the test"
    )]
    fn discover(
        &self,
        _: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let path = Path::new("/elsewhere/gone");
        let supplied = sink.supplied();
        let added = sink.add_unread(&self.rule(), path, Vec::new(), supplied);
        *self.result.lock().unwrap() = Some(added.is_ok());
        Ok(())
    }
}

/// Catches an unread finding being sized, or its facts reading as anything but unknown.
#[test]
fn an_unread_finding_is_reported_and_never_measured() {
    let unreading = Unreading {
        operation: Operation::Report,
        result: Arc::default(),
    };
    let rule = unreading.rule();
    let sizer = FakeSizer::default();
    let calls = Arc::clone(&sizer.calls);
    let ports = ports(FakeFs::default(), sizer);
    let engine = Engine::new(ports, vec![Box::new(unreading)], vec![rule]).unwrap();

    let report = run(&engine, &found());

    let [finding] = report.findings.as_slice() else {
        panic!("one finding, got {:?}", report.findings);
    };
    let facts = finding.facts.clone().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(finding.size, None);
    assert_eq!(facts.lineage, Lineage::default());
    assert_eq!(facts.proposal, Proposal::Report);
    assert!(
        matches!(facts.activity, Activity::Unknown(_)),
        "expected unknown activity, got {:?}",
        facts.activity
    );
    assert!(
        matches!(facts.tracked, Tracked::Unwalked(WalkGap::Unread)),
        "expected unread git, got {:?}",
        facts.tracked
    );
}

/// Catches a path outside every root getting into a rule that removes, past the
/// confinement `add` applies.
#[test]
fn a_rule_that_removes_cant_add_an_unread_finding() {
    let result = Arc::default();
    let unreading = Unreading {
        operation: Operation::RemoveFiles(vec![Method::Delete]),
        result: Arc::clone(&result),
    };
    let rule = unreading.rule();
    let ports = ports(FakeFs::default(), FakeSizer::default());
    let engine = Engine::new(ports, vec![Box::new(unreading)], vec![rule]).unwrap();

    let report = run(&engine, &found());

    assert_eq!(*result.lock().unwrap(), Some(false));
    assert_eq!(report.findings.len(), 0);
}

/// Lists what the walk of the project roots found, for a test to read.
#[derive(Debug)]
struct RepositoryLister {
    seen: Arc<Mutex<Vec<RepositoryDir>>>,
}

impl Integration for RepositoryLister {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            owner_files: &["tool.lock"],
            ..descriptor("lister")
        }
    }

    #[expect(
        clippy::unwrap_used,
        clippy::unwrap_in_result,
        reason = "a fake whose lock is poisoned fails the test"
    )]
    fn discover(
        &self,
        cx: &ScanContext<'_>,
        _: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        *self.seen.lock().unwrap() = cx.repositories(SymbolicRoot::ProjectRoots)?;
        Ok(())
    }
}

/// Catches the walk missing a linked worktree, whose `.git` is a file, or a repository in a
/// directory a tool owns, where the walk stops looking for projects.
#[test]
fn the_walk_lists_each_repository_by_what_its_git_is() {
    let fs = FakeFs::tree(&[
        "/found/",
        "/found/app/",
        "/found/app/.git/",
        "/found/linked/",
        "/found/linked/.git",
        "/found/owned/",
        "/found/owned/tool.lock",
        "/found/owned/.git/",
        "/found/plain/",
    ]);
    let seen = Arc::default();
    let lister = RepositoryLister {
        seen: Arc::clone(&seen),
    };
    let rule = project_rule("lister", "Cargo.toml", "target");
    let ports = ports(fs, FakeSizer::default());
    let engine = Engine::new(ports, vec![Box::new(lister)], vec![rule]).unwrap();
    let mut roots = Roots::default();
    insert(&mut roots, SymbolicRoot::ProjectRoots, FOUND);
    let request = ScanRequest {
        roots,
        lens: None,
        exclude: Vec::new(),
    };

    run(&engine, &request);

    let repository = |dir: &str, git| RepositoryDir {
        dir: PathBuf::from(dir),
        git,
    };
    assert_eq!(
        *seen.lock().unwrap(),
        [
            repository("/found/app", EntryKind::Dir),
            repository("/found/linked", EntryKind::File),
            repository("/found/owned", EntryKind::Dir),
        ]
    );
}
