//! The host Codex's tests run on: a `FixtureTree` on the real file system read without
//! following links, with what a test can't build on every OS scripted over it: what can't be
//! read, what isn't a file or a directory, what is on another mount, and a link's stamp,
//! which only Unix has and which Codex records as the same three fields there.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a host that can't be set up fails the test"
)]

use std::ffi::OsString;
use std::fs;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use super::tree::{FixtureTree, Holds};
use super::{codex_v0_165_0, codex_v0_166_0};
use kipple_adapters::builtin;
use kipple_core::{
    Advice, Anchor, Apparent, CancelToken, Capabilities, Clock, CommandLines, Completeness,
    DirAccess, DirEntryMeta, Engine, EntryKind, EntryMeta, EventChannel, FactValue, Facts,
    FileIdentity, FileSystem, Finding, FrozenTree, FsProbe, GitGap, HeldPath, Hold, IntegrationId,
    Inventory, Lineage, Measurement, MountNamespace, NodeId, Observer, Omission, OwnerStatus,
    Owners, Platform, Policy, Ports, ProbeError, ProcessOutcome, ProcessOwner, ProcessProbe,
    ProcessRecord, ProcessSnapshot, Protections, ReleaseFacts, ResolvedRoot, RootSource, Roots,
    ScanReport, ScanRequest, Sizer, SpaceEstimate, Subject, SymbolicRoot, TrackedQuery, Unread,
    UnresolvedRoot, Verdict, event_channel,
};

/// Where the Codex home is in a tree, as it is by default.
pub(crate) const HOME: &str = "home/.codex";

/// The target of every Linux release here.
pub(crate) const MUSL: &str = "x86_64-unknown-linux-musl";

/// The components of a store.
pub(crate) const STANDALONE: &str = "standalone";
pub(crate) const DAEMON: &str = "app-server-daemon";

const OUR_NAMESPACE: MountNamespace = MountNamespace(4_026_531_841);

/// `path`, relative to the home, as a path in the tree.
pub(crate) fn at(path: &str) -> String {
    format!("{HOME}/{path}")
}

/// A release's directory name.
pub(crate) fn name(version: &str) -> String {
    format!("{version}-{MUSL}")
}

/// `<component>` of the store, relative to the home.
pub(crate) fn component(component: &str) -> String {
    format!("packages/{component}")
}

/// The release `name` of `store`, relative to the home.
pub(crate) fn release(store: &str, name: &str) -> String {
    format!("packages/{store}/releases/{name}")
}

/// Adds the release `version` of `store`, in the layout its version has.
pub(crate) fn add_release(tree: &mut FixtureTree, store: &str, version: &str) {
    let dir = at(&release(store, &name(version)));
    match version {
        "0.162.1" | "0.163.0" | "0.164.0" | "0.165.0" => {
            codex_v0_165_0::release(tree, &dir, version, MUSL);
        }
        _ => codex_v0_166_0::release(tree, &dir, version, MUSL),
    }
}

/// A file system with the scripted parts of [`Scene`] over the real one.
#[derive(Debug, Clone, Default)]
struct Disk {
    /// Paths that can't be read, nor anything below them.
    denied: Vec<PathBuf>,
    /// Paths that are neither a file, a directory nor a link, such as a FIFO.
    odd: Vec<PathBuf>,
    /// Directories that are mounts: they and everything below them are another volume.
    mounts: Vec<PathBuf>,
    /// The home directory, which only its user may enter.
    home: PathBuf,
    /// Every file that was opened, in order.
    opened: Arc<Mutex<Vec<PathBuf>>>,
}

impl Disk {
    fn check(&self, path: &Path) -> Result<(), ProbeError> {
        let denied = self.denied.iter().any(|at| path.starts_with(at));
        if denied {
            return Err(ProbeError::io(path, io::ErrorKind::PermissionDenied.into()));
        }
        Ok(())
    }

    fn kind(&self, path: &Path, file_type: fs::FileType) -> EntryKind {
        if self.odd.iter().any(|at| at == path) {
            EntryKind::Other
        } else if file_type.is_symlink() {
            EntryKind::Symlink
        } else if file_type.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        }
    }

    fn identity(&self, path: &Path) -> FileIdentity {
        let mut hasher = DefaultHasher::new();
        path.hash(&mut hasher);
        let mounted = self.mounts.iter().any(|mount| path.starts_with(mount));
        FileIdentity {
            volume: if mounted { 2 } else { 1 },
            file: u128::from(hasher.finish()),
        }
    }

    /// What Unix records of the link at `path`, whether or not this test can read it.
    fn stamp(path: &Path) -> Result<String, ProbeError> {
        let target = fs::read_link(path).map_err(|e| ProbeError::io(path, e))?;
        let mut hasher = DefaultHasher::new();
        target.hash(&mut hasher);
        Ok(format!("50 {} 1793458575.030397325", hasher.finish()))
    }
}

/// `error`, with a path below a file reading as absent. The engine looks for `.git` inside
/// each finding, and for a file the OS answers `ENOTDIR`, which it takes for a check that
/// failed: every file would then be held for unknown git. These tests are about Codex's
/// rules, so that answer is left out here.
fn below_a_file(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::NotADirectory {
        io::ErrorKind::NotFound.into()
    } else {
        error
    }
}

impl FsProbe for Disk {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        self.check(path)?;
        let meta = fs::symlink_metadata(path).map_err(|e| ProbeError::io(path, below_a_file(e)))?;
        Ok(EntryMeta {
            kind: self.kind(path, meta.file_type()),
            identity: Some(self.identity(path)),
        })
    }

    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        self.check(dir)?;
        let read = || -> io::Result<Vec<DirEntryMeta>> {
            fs::read_dir(dir)?
                .map(|entry| {
                    let entry = entry?;
                    Ok(DirEntryMeta {
                        kind: self.kind(&entry.path(), entry.file_type()?),
                        name: entry.file_name(),
                    })
                })
                .collect()
        };
        read().map_err(|e| ProbeError::io(dir, e))
    }

    fn open_dir(&self, dir: &Path) -> Result<(), ProbeError> {
        self.check(dir)?;
        fs::read_dir(dir)
            .map(drop)
            .map_err(|e| ProbeError::io(dir, e))
    }

    fn read_file(&self, path: &Path, limit: u64) -> Result<Vec<u8>, ProbeError> {
        self.check(path)?;
        self.opened
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(path.to_owned());
        let meta = fs::symlink_metadata(path).map_err(|e| ProbeError::io(path, e))?;
        if !meta.is_file() {
            return Err(ProbeError::io(path, io::ErrorKind::InvalidInput.into()));
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .and_then(|file| file.take(limit).read_to_end(&mut bytes))
            .map_err(|e| ProbeError::io(path, e))?;
        Ok(bytes)
    }

    fn access(&self, dir: &Path) -> Result<DirAccess, ProbeError> {
        if !dir.starts_with(&self.home) {
            return Err(ProbeError::io(dir, io::ErrorKind::PermissionDenied.into()));
        }
        Ok(DirAccess {
            owned_by_current_user: true,
            open_to_others: dir != self.home,
            acl: false,
            file_system: FileSystem::Local,
        })
    }

    fn read_link(&self, path: &Path) -> Result<PathBuf, ProbeError> {
        self.check(path)?;
        fs::read_link(path).map_err(|e| ProbeError::io(path, e))
    }

    fn link_stamp(&self, path: &Path) -> Result<String, ProbeError> {
        self.check(path)?;
        Self::stamp(path)
    }
}

/// Everything is old and measured whole, so what holds a finding is what a test set up.
#[derive(Debug)]
struct OldFiles;

impl Sizer for OldFiles {
    fn size(&self, _: &Path, _: &CancelToken) -> Result<Measurement, ProbeError> {
        let size = SpaceEstimate {
            apparent: Apparent(10),
            allocated: None,
            unique_reclaim: None,
            completeness: Completeness::Complete,
        };
        Ok(Measurement {
            size,
            modified: Some(SystemTime::UNIX_EPOCH),
            repository: None,
            nested_repository: None,
        })
    }
}

/// A year after everything here last changed.
#[derive(Debug)]
struct YearLater;

impl Clock for YearLater {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_hours(24 * 365)
    }
}

/// No git: the fixtures hold no repository, so it is never asked.
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

#[derive(Debug)]
struct Snapshot(ProcessSnapshot);

impl ProcessProbe for Snapshot {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        Ok(self.0.clone())
    }
}

fn denied() -> Unread {
    Unread::Failed(Arc::new(io::ErrorKind::PermissionDenied.into()))
}

fn argv(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

/// A process of `owner` that couldn't be inspected, as Linux shows another user's.
fn uninspectable(pid: u32, owner: ProcessOwner, command: &[&str]) -> ProcessRecord {
    let outcome = ProcessOutcome::Denied(Arc::new(io::ErrorKind::PermissionDenied.into()));
    ProcessRecord {
        owner,
        argv: Ok(argv(command)),
        cwd: Err(denied()),
        env_paths: Err(denied()),
        mapped_files: Err(denied()),
        mount_namespace: Err(denied()),
        root_dir: Err(denied()),
        capabilities: Ok(Capabilities(0)),
        ..ProcessRecord::new(pid, outcome)
    }
}

/// A process of the user running `executable`, with every fact read.
fn inspected(pid: u32, executable: &Path, root_dir: FileIdentity) -> ProcessRecord {
    let outcome = ProcessOutcome::Identified {
        path: executable.to_owned(),
        identity: FileIdentity {
            volume: 1,
            file: u128::from(pid),
        },
        deleted: false,
    };
    let name = executable.file_name().unwrap_or_default().to_owned();
    ProcessRecord {
        owner: ProcessOwner::CurrentUser,
        cwd: Ok(PathBuf::from("/")),
        argv: Ok(vec![name]),
        env_paths: Ok(Vec::new()),
        mapped_files: Ok(Vec::new()),
        mount_namespace: Ok(OUR_NAMESPACE),
        root_dir: Ok(root_dir),
        capabilities: Ok(Capabilities(0)),
        ..ProcessRecord::new(pid, outcome)
    }
}

/// What an ordinary Linux session shows: init and its daemons as root, the user's session
/// manager and agents behind a denied `/proc`, and a shell.
fn session(shell: &Path, root_dir: FileIdentity) -> Vec<ProcessRecord> {
    let me = || ProcessOwner::CurrentUser;
    vec![
        uninspectable(1, ProcessOwner::Root, &["/sbin/init"]),
        uninspectable(
            310,
            ProcessOwner::Root,
            &["/usr/lib/systemd/systemd-journald"],
        ),
        uninspectable(1201, me(), &["/usr/lib/systemd/systemd", "--user"]),
        uninspectable(1202, me(), &["(sd-pam)"]),
        uninspectable(1340, me(), &["/usr/bin/ssh-agent", "-D"]),
        inspected(2001, shell, root_dir),
    ]
}

/// What `selection-history` holds for a store.
#[derive(Debug)]
struct History {
    store: &'static str,
    selected: String,
    previous: String,
}

/// A host to scan: the tree, and what is scripted over it.
#[derive(Debug)]
pub(crate) struct Scene {
    pub(crate) tree: FixtureTree,
    histories: Vec<History>,
    /// Paths in the tree that can't be read.
    pub(crate) denied: Vec<String>,
    /// Paths in the tree that are FIFOs or the like.
    pub(crate) odd: Vec<String>,
    /// Paths in the tree that are mounts.
    pub(crate) mounts: Vec<String>,
    /// Which OS's rules apply.
    pub(crate) platform: Platform,
    /// Paths in the tree that a process of the user runs from.
    pub(crate) running: Vec<String>,
    /// Whether every process was listed.
    pub(crate) inventory: Inventory,
    /// Why the home has no path, if it hasn't.
    pub(crate) unresolved: Option<UnresolvedRoot>,
}

impl Scene {
    /// An empty home on Linux, in an ordinary session.
    pub(crate) fn new() -> Self {
        let mut tree = FixtureTree::new();
        tree.dir(HOME).modified(HOME, SystemTime::UNIX_EPOCH);
        Self {
            tree,
            histories: Vec::new(),
            denied: Vec::new(),
            odd: Vec::new(),
            mounts: Vec::new(),
            platform: Platform::Linux,
            running: Vec::new(),
            inventory: Inventory::Complete,
            unresolved: None,
        }
    }

    /// Adds `versions` to `store`.
    pub(crate) fn releases(&mut self, store: &'static str, versions: &[&str]) -> &mut Self {
        for version in versions {
            add_release(&mut self.tree, store, version);
        }
        self
    }

    /// `current` of `store` selects the release `name`, as upstream writes it.
    pub(crate) fn select(&mut self, store: &'static str, name: &str) -> &mut Self {
        let target = at(&release(store, name));
        let dir = at(&component(store));
        self.tree
            .link(&format!("{dir}/current"), &target, Holds::AbsoluteInTree)
            .file(&format!("{dir}/install.lock"), b"");
        self
    }

    /// A `selection-history` written for the link as built.
    pub(crate) fn history(
        &mut self,
        store: &'static str,
        selected: &str,
        previous: &str,
    ) -> &mut Self {
        self.histories.push(History {
            store,
            selected: selected.to_owned(),
            previous: previous.to_owned(),
        });
        self
    }

    /// A whole store: `versions`, of which `active` is selected, after `previous`.
    pub(crate) fn store(
        &mut self,
        store: &'static str,
        versions: &[&str],
        active: &str,
        previous: &str,
    ) -> &mut Self {
        self.releases(store, versions)
            .select(store, &name(active))
            .history(store, &name(active), &name(previous))
    }

    /// Builds the tree in a temp dir.
    pub(crate) fn build(self) -> Rc<Built> {
        let temp = tempfile::tempdir().unwrap();
        self.tree.build_in(temp.path()).unwrap();
        let root = temp.path();
        let inside = |paths: &[String]| paths.iter().map(|path| root.join(path)).collect();
        let disk = Disk {
            denied: inside(&self.denied),
            odd: inside(&self.odd),
            mounts: inside(&self.mounts),
            home: root.join("home"),
            opened: Arc::default(),
        };
        for history in &self.histories {
            let dir = root.join(at(&component(history.store)));
            let stamp = Disk::stamp(&dir.join("current")).unwrap();
            let bytes = format!("{}\n{}\n{stamp}\n", history.selected, history.previous);
            fs::write(dir.join("selection-history"), bytes).unwrap();
        }
        Rc::new(Built {
            temp,
            disk,
            platform: self.platform,
            running: self.running,
            inventory: self.inventory,
            unresolved: self.unresolved,
        })
    }

    /// Builds the tree and scans it.
    pub(crate) fn scan(self) -> Scan {
        self.build().scan()
    }
}

/// A tree on disk, ready to scan.
#[derive(Debug)]
pub(crate) struct Built {
    temp: tempfile::TempDir,
    disk: Disk,
    platform: Platform,
    running: Vec<String>,
    inventory: Inventory,
    unresolved: Option<UnresolvedRoot>,
}

impl Built {
    /// Where the home is.
    pub(crate) fn home(&self) -> PathBuf {
        self.temp.path().join(HOME)
    }

    /// What `selection-history` of `store` records of `current` now.
    pub(crate) fn stamp(&self, store: &str) -> String {
        let link = self.home().join(component(store)).join("current");
        Disk::stamp(&link).unwrap()
    }

    fn roots(&self) -> Roots {
        let mut roots = Roots::default();
        let home = self.temp.path().join("home");
        let fixed = |root, path| ResolvedRoot::new(root, path, RootSource::Fixed);
        roots.insert(SymbolicRoot::Home, Ok(fixed(SymbolicRoot::Home, home)));
        let store = self.home().join("packages");
        let placed = [
            (SymbolicRoot::CodexHome, self.home()),
            (SymbolicRoot::CodexDefaultHome, self.home()),
            (SymbolicRoot::CodexReleaseStore, store),
        ];
        for (root, path) in placed {
            let resolved = self
                .unresolved
                .clone()
                .map_or_else(|| Ok(fixed(root, path)), Err);
            roots.insert(root, resolved);
        }
        roots
    }

    fn snapshot(&self) -> ProcessSnapshot {
        let root_dir = self.disk.identity(Path::new("/"));
        let shell = self.temp.path().join("usr/bin/zsh");
        let mut processes = session(&shell, root_dir);
        for (pid, path) in (3000..).zip(&self.running) {
            let executable = self.temp.path().join(path);
            processes.push(inspected(pid, &executable, root_dir));
        }
        ProcessSnapshot {
            processes,
            inventory: self.inventory.clone(),
            observer: Observer {
                mount_namespace: Ok(OUR_NAMESPACE),
                root_dir: Ok(root_dir),
                command_lines: CommandLines::Public,
                process_paths: true,
            },
        }
    }

    /// Scans it as it is now, for a test that changed a file since it was built.
    pub(crate) fn scan(self: &Rc<Self>) -> Scan {
        Scan {
            built: Rc::clone(self),
            report: self.report(),
        }
    }

    /// What a scan with Codex's rules of this OS finds now.
    fn report(&self) -> ScanReport {
        let packs = builtin::packs(&semver::Version::new(0, 0, 0)).unwrap();
        let rules = packs
            .into_iter()
            .flat_map(|pack| pack.rules)
            .filter(|rule| rule.integration == IntegrationId("codex"))
            .filter(|rule| rule.platforms.contains(&self.platform))
            .collect();
        let integrations = builtin::integrations()
            .into_iter()
            .filter(|integration| integration.descriptor().id == IntegrationId("codex"))
            .collect();
        let ports = Ports {
            fs: Box::new(self.disk.clone()),
            sizer: Box::new(OldFiles),
            processes: Box::new(Snapshot(self.snapshot())),
            clock: Box::new(YearLater),
            tracked: Box::new(NoGit),
            tools: Box::new(kipple_core::Unavailable),
            worktrees: Box::new(kipple_core::Unavailable),
        };
        let engine = Engine::new(ports, integrations, rules).unwrap();
        let cancel = CancelToken::new();
        let EventChannel { tx, rx } = event_channel(4096, &cancel);
        let request = ScanRequest {
            roots: self.roots(),
            lens: None,
            exclude: Vec::new(),
        };
        let report = engine.scan(&request, &tx, &cancel);
        drop(rx);
        report
    }
}

/// What a scan found, by paths relative to the home with `/` separators.
#[derive(Debug)]
pub(crate) struct Scan {
    pub(crate) built: Rc<Built>,
    pub(crate) report: ScanReport,
}

impl Scan {
    fn relative(&self, path: &Path) -> String {
        let home = self.built.home();
        let parts = path.strip_prefix(&home).unwrap_or(path).components();
        let parts: Vec<_> = parts
            .map(|part| part.as_os_str().to_string_lossy())
            .collect();
        parts.join("/")
    }

    /// Each finding as the end of its rule's ID and its path, sorted.
    pub(crate) fn rows(&self) -> Vec<String> {
        let row = |finding: &Finding| {
            let rule = finding.rule.0.trim_start_matches("builtin.codex.");
            format!("{rule} {}", self.relative(&finding.path))
        };
        let mut rows: Vec<String> = self.report.findings.iter().map(row).collect();
        rows.sort();
        rows
    }

    /// The finding at `path`, when there is one.
    pub(crate) fn get(&self, path: &str) -> Option<&Finding> {
        let found = |finding: &&Finding| self.relative(&finding.path) == path;
        self.report.findings.iter().find(found)
    }

    /// The finding at `path`.
    pub(crate) fn finding(&self, path: &str) -> &Finding {
        let rows = self.rows();
        self.get(path)
            .unwrap_or_else(|| panic!("no finding at {path}, only {rows:#?}"))
    }

    /// The end of the ID of the rule that found `path`.
    pub(crate) fn rule(&self, path: &str) -> &str {
        let rule = &self.finding(path).rule.0;
        rule.trim_start_matches("builtin.codex.")
    }

    /// What the host recorded of `path`.
    pub(crate) fn facts(&self, path: &str) -> &Facts {
        self.finding(path).facts.as_ref().unwrap()
    }

    /// What was supplied for the release at `path`.
    pub(crate) fn release(&self, path: &str) -> &ReleaseFacts {
        match &self.facts(path).supplied.subject {
            Subject::Release(release) => release,
            Subject::Plain | Subject::Worktree(_) | Subject::Semantic(_) => {
                panic!("{path} is no release")
            }
        }
    }

    /// The value `fact` was given at `path`.
    pub(crate) fn value(&self, path: &str, fact: &str) -> Option<FactValue> {
        self.facts(path).supplied.values.get(fact).copied()
    }

    /// The verdict at `path`, under the default policy.
    pub(crate) fn verdict(&self, path: &str) -> Verdict {
        let finding = self.finding(path);
        let facts = finding.facts.as_ref().unwrap();
        Policy::default().verdict(&finding.rule, &finding.path, facts)
    }

    /// Why `path` is held, if it is.
    pub(crate) fn holds(&self, path: &str) -> Vec<Hold> {
        match self.verdict(path) {
            Verdict::Protected(holds) => holds,
            Verdict::Safe | Verdict::Review(_) => Vec::new(),
        }
    }

    fn integration(&self) -> &kipple_core::IntegrationReport {
        let codex = |report: &&kipple_core::IntegrationReport| report.id == IntegrationId("codex");
        self.report.integrations.iter().find(codex).unwrap()
    }

    /// Whether Codex recognised what it read.
    pub(crate) fn owner(&self) -> OwnerStatus {
        self.integration().owner
    }

    /// What Codex suggests.
    pub(crate) fn advice(&self) -> &[Advice] {
        &self.integration().advice
    }

    /// What Codex couldn't cover.
    pub(crate) fn omissions(&self) -> &[Omission] {
        &self.integration().omissions
    }

    /// Every file a scan opened, relative to the home.
    pub(crate) fn opened(&self) -> Vec<String> {
        let opened = self.built.disk.opened.lock().unwrap();
        opened.iter().map(|path| self.relative(path)).collect()
    }

    /// Whether the host keeps `integration` away from `path` in the home, with `owners`.
    pub(crate) fn closes(&self, path: &str, integration: IntegrationId, owners: &Owners) -> bool {
        let roots = self.built.roots();
        let held = |at: &Path| HeldPath {
            forms: vec![at.to_owned()],
            lineage: self.lineage(at),
            note: None,
        };
        let data = self.built.temp.path().join("data");
        let descriptors = builtin::descriptors();
        let protections = Protections::of_host(&roots, Some(&data), &descriptors, &held);
        let full = self.built.home().join(path);
        let lineage = self.lineage(&full);
        protections
            .check(&full, &lineage, integration, owners)
            .is_some()
    }

    /// [`Self::closes`], for Codex in this scan.
    pub(crate) fn closed(&self, path: &str) -> bool {
        self.closes(path, IntegrationId("codex"), &self.report.owners())
    }

    fn lineage(&self, path: &Path) -> Lineage {
        let identity = |dir: &Path| Some(self.built.disk.identity(dir));
        Lineage(path.ancestors().map(identity).collect())
    }

    /// `owners` of this scan, with `integration` supported too.
    pub(crate) fn owners_with(&self, integration: IntegrationId) -> Owners {
        let mut owners = self.report.owners();
        owners.0.insert(integration, OwnerStatus::Supported);
        owners
    }
}
