//! A host for the agent fixtures: the real file system read without following links, with
//! every root, the clock and the process snapshot injected, so a test can place a tool where
//! it likes and show the processes a real host shows. The shared `fixtures/scan.rs` has no
//! processes, sizes or times, which is what these rules turn on.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use kipple_adapters::builtin;
use kipple_core::{
    Activity, Advice, Anchor, Apparent, CancelToken, Capabilities, Clock, CommandLines,
    Completeness, DirAccess, DirEntryMeta, Engine, EntryKind, EntryMeta, EventChannel, FactValue,
    Facts, FileIdentity, FileSystem, Finding, FrozenTree, FsProbe, GitGap, HeldPath, Hold,
    IntegrationId, Inventory, Level, Lineage, Measurement, MountNamespace, NodeId, Now, Observer,
    Omission, OwnerStatus, Platform, Policy, Ports, ProbeError, ProcessOutcome, ProcessOwner,
    ProcessProbe, ProcessRecord, ProcessSnapshot, Proposal, Protection, Protections, Rescan as _,
    ResolvedRoot, RootSource, Roots, ScanReport, ScanRequest, Sizer, Skip, SpaceEstimate,
    SymbolicRoot, TrackedQuery, Unread, UnresolvedRoot, Verdict, event_channel,
};

use super::tree::FixtureTree;

pub(crate) const DAY: Duration = Duration::from_hours(24);

/// How long ago a fixture's files changed unless a test says otherwise: past every window.
const AGE: Duration = Duration::from_hours(24 * 41);

const OUR_NAMESPACE: MountNamespace = MountNamespace(4_026_531_841);

/// Each root of the host, relative to the tree. A root a test doesn't move stays here.
const ROOTS: &[(SymbolicRoot, &str)] = &[
    (SymbolicRoot::Home, "home"),
    (SymbolicRoot::XdgCache, "home/.cache"),
    (SymbolicRoot::ClaudeConfig, "home/.claude"),
    (SymbolicRoot::ClaudeHome, "home/.claude"),
    (SymbolicRoot::ClaudeConfigFile, "home/.claude.json"),
    (SymbolicRoot::ClaudeVersions, "home/.local/share/claude"),
    (SymbolicRoot::ClaudeInstallState, "home/.local/state/claude"),
    (SymbolicRoot::PiAgent, "home/.pi/agent"),
    (SymbolicRoot::PiHome, "home/.pi"),
    (SymbolicRoot::CursorConfig, "home/.cursor"),
    (SymbolicRoot::CursorAppData, "home/.config/Cursor"),
];

/// `path` as the host takes it: Windows reads one with no drive as relative.
pub(crate) fn abs(path: &str) -> PathBuf {
    if path.starts_with('/') && !Path::new("/").is_absolute() {
        PathBuf::from(format!("C:{path}"))
    } else {
        PathBuf::from(path)
    }
}

fn identity(path: &Path) -> FileIdentity {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    FileIdentity {
        volume: 1,
        file: u128::from(hasher.finish()),
    }
}

fn lineage(path: &Path) -> Lineage {
    Lineage(path.ancestors().map(identity).map(Some).collect())
}

fn held(path: &Path) -> HeldPath {
    HeldPath {
        forms: vec![path.to_owned()],
        lineage: lineage(path),
        note: None,
    }
}

fn kind(file_type: fs::FileType) -> EntryKind {
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

/// The files `read_file` was asked for, in order.
type Reads = Arc<Mutex<Vec<PathBuf>>>;

/// What the probe answers for `.git` below a file, which is what the host asks of every file
/// finding (`Repositories::above` starts at the item).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitBelowFile {
    /// `ENOTDIR`, as Linux and macOS answer. The host takes that for a `.git` it couldn't
    /// check, so it holds every file finding as `unknown-git`.
    NotADirectory,
    /// Nothing there, as Windows answers and as the host should read `ENOTDIR`. A stand-in
    /// until `kipple-core/src/facts.rs` does: without it no rule here could reach its verdict.
    /// Remove this once it does; `a_file_finding_is_held_by_the_git_probe` then fails to say so.
    Absent,
}

/// How the disk treats a name's case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Names {
    /// As the temp directory's file system does. Linux keeps case apart.
    AsTheDiskDoes,
    /// Looking up a name finds an entry that differs only by case, as macOS and Windows do
    /// by default, so a suite run on Linux meets what they would answer.
    Folded,
}

/// The paths the OS refuses, each with everything below it.
type Denied = Vec<PathBuf>;

/// The real file system, with each path's identity derived from it, as a host with no links
/// above the tree has.
#[derive(Debug)]
struct TempFs {
    reads: Reads,
    git_below_file: GitBelowFile,
    names: Names,
    denied: Denied,
}

impl TempFs {
    /// The answer for `path` when it is `.git` below a file, on any OS.
    fn git_below_file(&self, path: &Path) -> Option<io::ErrorKind> {
        let parent = path.parent().filter(|_| path.ends_with(".git"))?;
        fs::symlink_metadata(parent)
            .ok()
            .filter(fs::Metadata::is_file)?;
        Some(match self.git_below_file {
            GitBelowFile::NotADirectory => io::ErrorKind::NotADirectory,
            GitBelowFile::Absent => io::ErrorKind::NotFound,
        })
    }

    /// The entry a lookup of `path` lands on.
    fn found(&self, path: &Path) -> PathBuf {
        let folded = || {
            let name = path.file_name()?.to_str()?.to_lowercase();
            let entries = fs::read_dir(path.parent()?).ok()?;
            let same = |entry: &fs::DirEntry| {
                entry
                    .file_name()
                    .to_str()?
                    .to_lowercase()
                    .eq(&name)
                    .then_some(())
            };
            entries
                .flatten()
                .find(|entry| same(entry).is_some())
                .map(|entry| entry.path())
        };
        let fold = self.names == Names::Folded;
        fold.then(folded)
            .flatten()
            .unwrap_or_else(|| path.to_owned())
    }
}

impl FsProbe for TempFs {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        if self.denied.iter().any(|denied| path.starts_with(denied)) {
            let denied = io::ErrorKind::PermissionDenied.into();
            return Err(ProbeError::io(path, denied));
        }
        if let Some(kind) = self.git_below_file(path) {
            return Err(ProbeError::io(path, kind.into()));
        }
        let meta = fs::symlink_metadata(self.found(path)).map_err(|e| ProbeError::io(path, e))?;
        Ok(EntryMeta {
            kind: kind(meta.file_type()),
            identity: Some(identity(path)),
        })
    }

    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        let read = || -> io::Result<Vec<DirEntryMeta>> {
            fs::read_dir(self.found(dir))?
                .map(|entry| {
                    let entry = entry?;
                    Ok(DirEntryMeta {
                        name: entry.file_name(),
                        kind: kind(entry.file_type()?),
                    })
                })
                .collect()
        };
        read().map_err(|e| ProbeError::io(dir, e))
    }

    fn open_dir(&self, dir: &Path) -> Result<(), ProbeError> {
        fs::read_dir(dir)
            .map(drop)
            .map_err(|e| ProbeError::io(dir, e))
    }

    fn read_file(&self, path: &Path, limit: u64) -> Result<Vec<u8>, ProbeError> {
        let reads = &self.reads;
        reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(path.to_owned());
        if fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_file()) {
            return Err(ProbeError::io(path, io::ErrorKind::InvalidInput.into()));
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .and_then(|file| file.take(limit).read_to_end(&mut bytes))
            .map_err(|e| ProbeError::io(path, e))?;
        Ok(bytes)
    }

    fn access(&self, _: &Path) -> Result<DirAccess, ProbeError> {
        Ok(DirAccess {
            owned_by_current_user: true,
            open_to_others: false,
            acl: false,
            file_system: FileSystem::Local,
        })
    }

    fn read_link(&self, path: &Path) -> Result<PathBuf, ProbeError> {
        fs::read_link(path).map_err(|e| ProbeError::io(path, e))
    }
}

/// When a file changed is what the logs' rules read, so that is real; sizes are not.
#[derive(Debug)]
struct TimeSizer;

impl Sizer for TimeSizer {
    fn size(&self, path: &Path, _: &CancelToken) -> Result<Measurement, ProbeError> {
        let meta = fs::symlink_metadata(path).map_err(|e| ProbeError::io(path, e))?;
        let size = SpaceEstimate {
            apparent: Apparent(meta.len()),
            allocated: None,
            unique_reclaim: None,
            completeness: Completeness::Complete,
        };
        Ok(Measurement {
            size,
            modified: meta.modified().ok(),
            repository: None,
            nested_repository: None,
        })
    }
}

/// A time that stays put.
#[derive(Debug)]
struct Fixed(SystemTime);

impl Clock for Fixed {
    fn now(&self) -> SystemTime {
        self.0
    }
}

/// These fixtures hold no repository, so it is never asked.
#[derive(Debug)]
struct NoGit;

impl TrackedQuery for NoGit {
    fn count(&self, _: &Path, _: &Path, _: &CancelToken) -> Result<u64, GitGap> {
        Err(GitGap::Unsupported)
    }

    fn entries(
        &self,
        _: &Path,
        _: &Anchor,
        _: &FrozenTree,
        _: &CancelToken,
    ) -> Result<Vec<NodeId>, GitGap> {
        Err(GitGap::Unsupported)
    }
}

/// What the process probe shows, which a test can change between a scan and apply's reading.
#[derive(Debug, Clone)]
pub(crate) struct Processes {
    pub(crate) running: Vec<ProcessRecord>,
    pub(crate) inventory: Inventory,
}

type Shown = Arc<Mutex<Processes>>;

/// Linux as it shows processes to kipple: everyone's command line is public.
#[derive(Debug)]
struct Snapshot(Shown);

impl ProcessProbe for Snapshot {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        let shown = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(ProcessSnapshot {
            processes: shown.running.clone(),
            inventory: shown.inventory.clone(),
            observer: Observer {
                mount_namespace: Ok(OUR_NAMESPACE),
                root_dir: Ok(identity(&abs("/"))),
                command_lines: CommandLines::Public,
                process_paths: true,
            },
        })
    }
}

fn failed() -> Unread {
    Unread::Failed(Arc::new(io::ErrorKind::PermissionDenied.into()))
}

/// A process of the current user, every fact read: working in `/`, started as `name`, with
/// no path arguments, environment paths or mapped files.
pub(crate) fn running(pid: u32, name: &str, executable: &str) -> ProcessRecord {
    let outcome = ProcessOutcome::Identified {
        path: abs(executable),
        identity: FileIdentity {
            volume: 2,
            file: u128::from(pid),
        },
        deleted: false,
    };
    ProcessRecord {
        owner: ProcessOwner::CurrentUser,
        cwd: Ok(abs("/")),
        argv: Ok(vec![OsString::from(name)]),
        env_paths: Ok(Vec::new()),
        mapped_files: Ok(Vec::new()),
        mount_namespace: Ok(OUR_NAMESPACE),
        root_dir: Ok(identity(&abs("/"))),
        capabilities: Ok(Capabilities(0)),
        ..ProcessRecord::new(pid, outcome)
    }
}

/// A process of `owner` as Linux shows one that refuses `/proc/<pid>/exe`: only its command
/// line is read.
pub(crate) fn sealed(pid: u32, owner: ProcessOwner, argv: &[&str]) -> ProcessRecord {
    let denied = ProcessOutcome::Denied(Arc::new(io::ErrorKind::PermissionDenied.into()));
    ProcessRecord {
        owner,
        argv: Ok(argv.iter().map(|arg| abs(arg).into_os_string()).collect()),
        cwd: Err(failed()),
        env_paths: Err(failed()),
        mapped_files: Err(failed()),
        mount_namespace: Err(failed()),
        root_dir: Err(failed()),
        capabilities: Ok(Capabilities(0)),
        ..ProcessRecord::new(pid, denied)
    }
}

/// What any desktop session runs: root's services and the user's session manager, which
/// refuse to be inspected, and a shell. A rule that can't be idle beside these ships dead.
pub(crate) fn session() -> Vec<ProcessRecord> {
    let me = ProcessOwner::CurrentUser;
    vec![
        sealed(1, ProcessOwner::Root, &["/sbin/init"]),
        sealed(
            310,
            ProcessOwner::Root,
            &["/usr/lib/systemd/systemd-journald"],
        ),
        sealed(1201, me.clone(), &["/usr/lib/systemd/systemd", "--user"]),
        sealed(1202, me.clone(), &["(sd-pam)"]),
        sealed(
            1340,
            me,
            &["/usr/bin/ssh-agent", "-D", "-a", "/run/user/1000/agent"],
        ),
        running(2001, "zsh", "/usr/bin/zsh"),
    ]
}

/// A machine to scan.
pub(crate) struct Host {
    temp: tempfile::TempDir,
    pub(crate) tree: FixtureTree,
    /// The time it is on this host, which every age is measured from.
    pub(crate) now: SystemTime,
    pub(crate) processes: Processes,
    /// Which OS's rules run.
    pub(crate) platform: Platform,
    pub(crate) git_below_file: GitBelowFile,
    pub(crate) names: Names,
    /// Paths below the tree the OS refuses, `/`-separated.
    pub(crate) denied: Vec<String>,
    roots: BTreeMap<SymbolicRoot, Vec<String>>,
    unresolved: Vec<SymbolicRoot>,
}

impl Host {
    /// Linux, to hold `tree`, with every file past every window and a desktop session
    /// running.
    #[expect(
        clippy::unwrap_used,
        reason = "a host that can't be set up fails the test"
    )]
    pub(crate) fn new(mut tree: FixtureTree) -> Self {
        let now = SystemTime::now() + 60 * DAY;
        tree.modified("", now - AGE);
        let mut roots: BTreeMap<SymbolicRoot, Vec<String>> = BTreeMap::new();
        for (root, path) in ROOTS {
            roots.entry(*root).or_default().push((*path).to_owned());
        }
        Self {
            temp: tempfile::tempdir().unwrap(),
            tree,
            now,
            processes: Processes {
                running: session(),
                inventory: Inventory::Complete,
            },
            platform: Platform::Linux,
            git_below_file: GitBelowFile::Absent,
            names: Names::AsTheDiskDoes,
            denied: Vec::new(),
            roots,
            unresolved: Vec::new(),
        }
    }

    /// `root` resolves to these paths, relative to the tree, and to nothing else.
    pub(crate) fn root(&mut self, root: SymbolicRoot, paths: &[&str]) {
        let paths = paths.iter().map(|path| (*path).to_owned()).collect();
        self.roots.insert(root, paths);
    }

    /// `root` also has a path that didn't resolve.
    pub(crate) fn unresolved(&mut self, root: SymbolicRoot) {
        self.unresolved.push(root);
    }

    /// Where `relative`, `/`-separated, will be.
    pub(crate) fn path(&self, relative: &str) -> PathBuf {
        below(self.temp.path(), relative)
    }

    /// Builds the tree and runs the rules of `self.platform` over it.
    #[expect(
        clippy::unwrap_used,
        reason = "a host that can't be set up fails the test"
    )]
    pub(crate) fn scan(self) -> Scan {
        self.tree.build_in(self.temp.path()).unwrap();
        let mut roots = Roots::default();
        for (root, paths) in &self.roots {
            for path in paths {
                let path = below(self.temp.path(), path);
                roots.add(*root, Ok(ResolvedRoot::new(*root, path, RootSource::Fixed)));
            }
        }
        for root in &self.unresolved {
            roots.add(*root, Err(UnresolvedRoot::NotSet { var: "UNSET" }));
        }
        let request = ScanRequest {
            roots,
            lens: None,
            exclude: Vec::new(),
        };
        let reads = Reads::default();
        let shown = Arc::new(Mutex::new(self.processes));
        let fs = TempFs {
            reads: Arc::clone(&reads),
            git_below_file: self.git_below_file,
            names: self.names,
            denied: self
                .denied
                .iter()
                .map(|path| below(self.temp.path(), path))
                .collect(),
        };
        let engine = engine(self.platform, self.now, &shown, fs);
        let cancel = CancelToken::new();
        let EventChannel { tx, rx } = event_channel(4096, &cancel);
        let report = engine.scan(&request, &tx, &cancel);
        drop(rx);
        Scan {
            temp: self.temp,
            report,
            engine,
            request,
            reads,
            shown,
        }
    }
}

fn below(base: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(base.to_owned(), |at, part| at.join(part))
}

/// An engine with the integrations and rules of `platform`, as the binary composes one there.
#[expect(
    clippy::unwrap_used,
    reason = "a host that can't be set up fails the test"
)]
fn engine(platform: Platform, now: SystemTime, shown: &Shown, fs: TempFs) -> Engine {
    let ports = Ports {
        fs: Box::new(fs),
        sizer: Box::new(TimeSizer),
        processes: Box::new(Snapshot(Arc::clone(shown))),
        clock: Box::new(Fixed(now)),
        tracked: Box::new(NoGit),
        tools: Box::new(kipple_core::Unavailable),
        worktrees: Box::new(kipple_core::Unavailable),
    };
    let packs = builtin::packs(&semver::Version::new(0, 0, 0)).unwrap();
    let rules = packs
        .into_iter()
        .flat_map(|pack| pack.rules)
        .filter(|rule| rule.platforms.contains(&platform))
        .collect();
    let integrations = builtin::integrations()
        .into_iter()
        .filter(|integration| integration.descriptor().platforms.contains(&platform))
        .collect();
    Engine::new(ports, integrations, rules).unwrap()
}

/// What a scan found, and what a test asks of it. Findings are named by their path below
/// the tree, with `/` separators.
pub(crate) struct Scan {
    temp: tempfile::TempDir,
    pub(crate) report: ScanReport,
    engine: Engine,
    request: ScanRequest,
    reads: Reads,
    shown: Shown,
}

#[expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a finding a test names and can't be found fails it"
)]
impl Scan {
    /// Where `relative`, `/`-separated, is.
    pub(crate) fn path(&self, relative: &str) -> PathBuf {
        below(self.temp.path(), relative)
    }

    /// `path` below the tree, or the whole of it when it isn't there.
    fn relative(&self, path: &Path) -> String {
        let below = path.strip_prefix(self.temp.path()).unwrap_or(path);
        let parts: Vec<_> = below.iter().map(|part| part.to_string_lossy()).collect();
        parts.join("/")
    }

    /// Where `rule` found something, sorted.
    pub(crate) fn found(&self, rule: &str) -> Vec<String> {
        let mut found: Vec<String> = self
            .report
            .findings
            .iter()
            .filter(|finding| finding.rule.0 == rule)
            .map(|finding| self.relative(&finding.path))
            .collect();
        found.sort();
        found
    }

    /// Every finding of `integration`, sorted, as `<rule> <path>`.
    pub(crate) fn all(&self, integration: IntegrationId) -> Vec<String> {
        let mut found: Vec<String> = self
            .report
            .findings
            .iter()
            .filter(|finding| finding.integration == integration)
            .map(|finding| format!("{} {}", finding.rule, self.relative(&finding.path)))
            .collect();
        found.sort();
        found
    }

    pub(crate) fn finding(&self, relative: &str) -> &Finding {
        let found = self
            .report
            .findings
            .iter()
            .find(|finding| self.relative(&finding.path) == relative);
        found.unwrap_or_else(|| panic!("no finding at {relative}"))
    }

    fn facts(&self, relative: &str) -> &Facts {
        self.finding(relative).facts.as_ref().unwrap()
    }

    /// The fact `name` that the finding at `relative` was given.
    pub(crate) fn fact(&self, relative: &str, name: &str) -> Option<FactValue> {
        self.facts(relative).supplied.values.get(name).copied()
    }

    /// What the rule of the finding at `relative` proposes.
    pub(crate) fn proposal(&self, relative: &str) -> Proposal {
        self.facts(relative).proposal.clone()
    }

    /// Whether a program uses the finding at `relative`.
    pub(crate) fn activity(&self, relative: &str) -> Activity {
        self.facts(relative).activity.clone()
    }

    /// The standing of the finding at `relative` at `level`.
    pub(crate) fn verdict_at(&self, relative: &str, level: Level) -> Verdict {
        let policy = Policy {
            level,
            ..Policy::default()
        };
        self.finding(relative).verdict(&policy).unwrap()
    }

    /// The standing of the finding at `relative` at the default level.
    pub(crate) fn verdict(&self, relative: &str) -> Verdict {
        self.finding(relative).verdict(&Policy::default()).unwrap()
    }

    /// The holds of the finding at `relative`, none when it isn't protected.
    pub(crate) fn holds(&self, relative: &str) -> Vec<Hold> {
        match self.verdict(relative) {
            Verdict::Protected(holds) => holds,
            Verdict::Safe | Verdict::Review(_) => Vec::new(),
        }
    }

    /// What the host protects `relative` with, if `integration` asked to remove it.
    pub(crate) fn protection(
        &self,
        relative: &str,
        integration: IntegrationId,
    ) -> Option<Protection> {
        let path = self.path(relative);
        let data = self.path("home/.local/share/kipple");
        let host = Protections::of_host(
            &self.request.roots,
            Some(&data),
            &builtin::descriptors(),
            &held,
        );
        host.check(&path, &lineage(&path), integration, &self.report.owners())
    }

    /// What `integration`'s scan says of its owner status.
    pub(crate) fn owner(&self, integration: IntegrationId) -> OwnerStatus {
        self.report.owners().status(integration)
    }

    /// What `integration` couldn't cover.
    pub(crate) fn omissions(&self, integration: IntegrationId) -> Vec<&Omission> {
        let reports = self.report.integrations.iter();
        let ours = reports.filter(|report| report.id == integration);
        ours.flat_map(|report| &report.omissions).collect()
    }

    /// What `integration` advises.
    pub(crate) fn advice(&self, integration: IntegrationId) -> Vec<&Advice> {
        let reports = self.report.integrations.iter();
        let ours = reports.filter(|report| report.id == integration);
        ours.flat_map(|report| &report.advice).collect()
    }

    /// Changes what the process probe shows from here on.
    pub(crate) fn show(&self, change: impl FnOnce(&mut Processes)) {
        change(&mut self.shown.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// What apply reads again about the finding at `relative`.
    pub(crate) fn now(&self, relative: &str) -> Result<Now, Skip> {
        let rescan = self.engine.rescan(&self.request);
        rescan.now(&self.report, self.finding(relative), &CancelToken::new())
    }

    /// Every file an integration asked to read, by its path below the tree.
    pub(crate) fn read(&self) -> Vec<String> {
        let reads = self.reads.lock().unwrap_or_else(PoisonError::into_inner);
        reads.iter().map(|path| self.relative(path)).collect()
    }
}

/// A small generator, seeded, so a failure names its input.
pub(crate) struct Generated(u64);

impl Generated {
    pub(crate) const fn seeded(seed: u64) -> Self {
        Self(
            seed.wrapping_mul(2_862_933_555_777_941_757)
                .wrapping_add(3_037_000_493),
        )
    }

    pub(crate) const fn below(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((self.0 >> 33) as usize) % bound
    }

    /// `valid` with its bytes cut, flipped or spliced, as a writer stopped short or a
    /// different upstream would leave it.
    pub(crate) fn mutated(&mut self, valid: &[u8]) -> Vec<u8> {
        let mut bytes = valid.to_vec();
        for _ in 0..=self.below(4) {
            let at = self.below(bytes.len().max(1));
            match self.below(4) {
                0 => bytes.truncate(at),
                1 if at < bytes.len() => {
                    let byte = u8::try_from(self.below(256)).unwrap_or(0);
                    bytes.splice(at..=at, [byte]);
                }
                2 => bytes.insert(at.min(bytes.len()), b'"'),
                _ if at < bytes.len() => {
                    bytes.remove(at);
                }
                _ => bytes.push(b'}'),
            }
        }
        bytes
    }
}
