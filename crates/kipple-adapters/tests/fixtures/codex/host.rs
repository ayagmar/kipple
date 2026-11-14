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
    Inventory, Lineage, Measurement, Mount, MountNamespace, NodeId, Now, Observer, Omission,
    OwnerStatus, Owners, Platform, Policy, Ports, ProbeError, ProcessOutcome, ProcessOwner,
    ProcessProbe, ProcessRecord, ProcessSnapshot, Protections, ReleaseFacts, Rescan as _,
    ResolvedRoot, RootSource, Roots, ScanEvent, ScanReport, ScanRequest, Sizer, Skip,
    SpaceEstimate, Subject, SymbolicRoot, TrackedQuery, Unread, UnresolvedRoot, Verdict,
    event_channel,
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

/// The identity the host gives the file at `path`, off any mount.
fn identity(path: &Path) -> FileIdentity {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    FileIdentity {
        volume: 1,
        file: u128::from(hasher.finish()),
    }
}

/// A file system with the scripted parts of [`Scene`] over the real one.
#[derive(Debug, Clone, Default)]
struct Disk {
    /// Paths that can't be read, nor anything below them. A test may add one after a scan.
    denied: Arc<Mutex<Vec<PathBuf>>>,
    /// Directories that can't be listed, though what is below them can be reached.
    unlistable: Vec<PathBuf>,
    /// Paths that are neither a file, a directory nor a link, such as a FIFO.
    odd: Vec<PathBuf>,
    /// Directories that are mounts: they and everything below them are another volume.
    mounts: Vec<PathBuf>,
    /// Directories that are bind mounts: they and everything below them are another mount
    /// of the same volume.
    binds: Vec<PathBuf>,
    /// Links that read as holding the second path, and not what the OS made of it.
    stored: Vec<(PathBuf, PathBuf)>,
    /// Links an installer made again after the scan, with what each now holds: both what
    /// they read as and their stamp follow.
    relinked: Arc<Mutex<Vec<(PathBuf, PathBuf)>>>,
    /// The home directory, which only its user may enter.
    home: PathBuf,
    /// Every file that was opened, in order.
    opened: Arc<Mutex<Vec<PathBuf>>>,
}

impl Disk {
    fn check(&self, path: &Path) -> Result<(), ProbeError> {
        let denied = {
            let denied = self.denied.lock().unwrap_or_else(PoisonError::into_inner);
            denied.iter().any(|at| path.starts_with(at))
        };
        if denied {
            return Err(ProbeError::io(path, io::ErrorKind::PermissionDenied.into()));
        }
        Ok(())
    }

    fn check_listing(&self, dir: &Path) -> Result<(), ProbeError> {
        self.check(dir)?;
        if self.unlistable.iter().any(|at| at == dir) {
            return Err(ProbeError::io(dir, io::ErrorKind::PermissionDenied.into()));
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
        let mounted = self.mounts.iter().any(|mount| path.starts_with(mount));
        FileIdentity {
            volume: if mounted { 2 } else { 1 },
            ..identity(path)
        }
    }

    /// The mount `path` is on, by an ID as Linux gives one.
    fn mount(&self, path: &Path) -> Mount {
        if self.mounts.iter().any(|mount| path.starts_with(mount)) {
            Mount::Id(3)
        } else if self.binds.iter().any(|bind| path.starts_with(bind)) {
            Mount::Id(2)
        } else {
            Mount::Id(1)
        }
    }

    /// What Unix records of the link at `path`, whether or not this test can read it.
    fn stamp(path: &Path) -> Result<String, ProbeError> {
        let target = fs::read_link(path).map_err(|e| ProbeError::io(path, e))?;
        Ok(Self::stamp_of(&target))
    }

    /// The stamp of a link that holds `target`.
    fn stamp_of(target: &Path) -> String {
        let mut hasher = DefaultHasher::new();
        target.hash(&mut hasher);
        format!("50 {} 1793458575.030397325", hasher.finish())
    }

    /// What the link at `path` holds since an installer made it again, if one did.
    fn relinked(&self, path: &Path) -> Option<PathBuf> {
        let relinked = self.relinked.lock().unwrap_or_else(PoisonError::into_inner);
        let found = relinked.iter().rev().find(|(link, _)| link == path);
        let target = found.map(|(_, target)| target.clone());
        drop(relinked);
        target
    }
}

impl FsProbe for Disk {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        self.check(path)?;
        let meta = fs::symlink_metadata(path).map_err(|e| ProbeError::io(path, e))?;
        Ok(EntryMeta {
            kind: self.kind(path, meta.file_type()),
            identity: Some(self.identity(path)),
            mount: self.mount(path),
        })
    }

    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        self.check_listing(dir)?;
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
        self.check_listing(dir)?;
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
        if let Some(target) = self.relinked(path) {
            return Ok(target);
        }
        let stored = self.stored.iter().find(|(link, _)| link == path);
        if let Some((_, target)) = stored {
            return Ok(target.clone());
        }
        fs::read_link(path).map_err(|e| ProbeError::io(path, e))
    }

    fn link_stamp(&self, path: &Path) -> Result<String, ProbeError> {
        self.check(path)?;
        self.relinked(path)
            .map_or_else(|| Self::stamp(path), |target| Ok(Self::stamp_of(&target)))
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

/// What the process probe shows, which a test can change between a scan and apply's reading.
#[derive(Debug, Clone)]
pub(crate) struct Shown {
    /// Paths in the tree that a process of the user runs from.
    pub(crate) running: Vec<String>,
    /// Processes of the user that run a hard link to a file in the tree from elsewhere.
    pub(crate) linked: Vec<HardLink>,
    /// Commands of processes of the user that couldn't be inspected.
    pub(crate) uninspected: Vec<String>,
    /// Whether every process was listed.
    pub(crate) inventory: Inventory,
}

/// The processes of the host as they are shown when asked.
#[derive(Debug)]
struct Snapshot {
    tree: PathBuf,
    root_dir: FileIdentity,
    shown: Arc<Mutex<Shown>>,
}

impl ProcessProbe for Snapshot {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        let shown = self.shown.lock().unwrap_or_else(PoisonError::into_inner);
        let shell = self.tree.join("usr/bin/zsh");
        let mut processes = session(&shell, self.root_dir);
        for (pid, path) in (3000..).zip(&shown.running) {
            let executable = self.tree.join(path);
            processes.push(inspected(pid, &executable, self.root_dir));
        }
        for (pid, link) in (4000..).zip(&shown.linked) {
            let mut process = inspected(pid, &self.tree.join(&link.runs), self.root_dir);
            if let ProcessOutcome::Identified { identity: id, .. } = &mut process.outcome {
                *id = identity(&self.tree.join(&link.file));
            }
            processes.push(process);
        }
        for (pid, command) in (5000..).zip(&shown.uninspected) {
            let command = self.tree.join(command);
            let command = command.to_string_lossy();
            processes.push(uninspectable(pid, ProcessOwner::CurrentUser, &[&command]));
        }
        Ok(ProcessSnapshot {
            processes,
            inventory: shown.inventory.clone(),
            observer: Observer {
                mount_namespace: Ok(OUR_NAMESPACE),
                root_dir: Ok(self.root_dir),
                command_lines: CommandLines::Public,
                process_paths: true,
            },
        })
    }
}

fn denied() -> Unread {
    Unread::Failed(Arc::new(io::ErrorKind::PermissionDenied.into()))
}

/// `path` as the host takes it: one starting with `/` is made absolute, since Windows takes
/// a path without a drive as relative, and a process's command line only says where it runs
/// from when its program is absolute. Anything else is kept as it is.
fn abs(path: &str) -> PathBuf {
    if path.starts_with('/') && !Path::new("/").is_absolute() {
        PathBuf::from(format!("C:{path}"))
    } else {
        PathBuf::from(path)
    }
}

fn argv(values: &[&str]) -> Vec<OsString> {
    let value = |value: &&str| abs(value).into_os_string();
    values.iter().map(value).collect()
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
        cwd: Ok(abs("/")),
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

/// A process running, from `runs`, a hard link to `file`: both are paths in the tree.
#[derive(Debug, Clone)]
pub(crate) struct HardLink {
    pub(crate) runs: String,
    pub(crate) file: String,
}

/// What `selection-history` holds for a store.
#[derive(Debug)]
struct History {
    store: &'static str,
    selected: String,
    previous: String,
}

/// A link that reads as holding `target`, a path below the tree. Windows may fold `..` out of
/// a target as it makes the link, so a test of one can't build it there.
#[derive(Debug)]
pub(crate) struct StoredLink {
    pub(crate) link: String,
    pub(crate) target: String,
}

/// A host to scan: the tree, and what is scripted over it.
#[derive(Debug)]
pub(crate) struct Scene {
    pub(crate) tree: FixtureTree,
    histories: Vec<History>,
    /// Paths in the tree that can't be read.
    pub(crate) denied: Vec<String>,
    /// Directories in the tree that can't be listed, as with `chmod 311`: only that fails.
    pub(crate) unlistable: Vec<String>,
    /// Paths in the tree that are FIFOs or the like.
    pub(crate) odd: Vec<String>,
    /// Paths in the tree that are mounts.
    pub(crate) mounts: Vec<String>,
    /// Paths in the tree that are bind mounts of a directory on the same volume.
    pub(crate) binds: Vec<String>,
    /// Links that read as holding what the test says.
    pub(crate) stored: Vec<StoredLink>,
    /// Which OS's rules apply.
    pub(crate) platform: Platform,
    /// Paths in the tree that a process of the user runs from.
    pub(crate) running: Vec<String>,
    /// Processes of the user that run a hard link to a file in the tree from elsewhere.
    pub(crate) linked: Vec<HardLink>,
    /// Commands, paths in the tree, of processes of the user that couldn't be inspected.
    pub(crate) uninspected: Vec<String>,
    /// The user's home directory in the tree, which holds the Codex home unless a test
    /// moves it.
    pub(crate) user_home: &'static str,
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
            unlistable: Vec::new(),
            odd: Vec::new(),
            mounts: Vec::new(),
            binds: Vec::new(),
            stored: Vec::new(),
            platform: Platform::Linux,
            running: Vec::new(),
            linked: Vec::new(),
            uninspected: Vec::new(),
            user_home: "home",
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
        fs::create_dir_all(temp.path().join(self.user_home)).unwrap();
        let root = temp.path();
        let inside = |paths: &[String]| paths.iter().map(|path| root.join(path)).collect();
        let disk = Disk {
            denied: Arc::new(Mutex::new(inside(&self.denied))),
            unlistable: inside(&self.unlistable),
            odd: inside(&self.odd),
            mounts: inside(&self.mounts),
            binds: inside(&self.binds),
            stored: self
                .stored
                .iter()
                .map(|stored| (root.join(&stored.link), root.join(&stored.target)))
                .collect(),
            home: root.join(self.user_home),
            opened: Arc::default(),
            relinked: Arc::default(),
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
            shown: Arc::new(Mutex::new(Shown {
                running: self.running,
                linked: self.linked,
                uninspected: self.uninspected,
                inventory: self.inventory,
            })),
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
    shown: Arc<Mutex<Shown>>,
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
        let home = self.disk.home.clone();
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

    /// Scans it as it is now, for a test that changed a file since it was built.
    pub(crate) fn scan(self: &Rc<Self>) -> Scan {
        let (engine, request) = self.engine();
        let cancel = CancelToken::new();
        let EventChannel { tx, rx } = event_channel(4096, &cancel);
        let report = engine.scan(&request, &tx, &cancel);
        drop(tx);
        let events = rx.map(|sequenced| sequenced.event).collect();
        Scan {
            built: Rc::clone(self),
            engine,
            request,
            report,
            events,
        }
    }

    /// An engine with Codex's rules of this OS, and what it scans.
    fn engine(&self) -> (Engine, ScanRequest) {
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
            processes: Box::new(Snapshot {
                tree: self.temp.path().to_owned(),
                root_dir: self.disk.identity(&abs("/")),
                shown: Arc::clone(&self.shown),
            }),
            clock: Box::new(YearLater),
            tracked: Box::new(NoGit),
            tools: Box::new(kipple_core::Unavailable),
            worktrees: Box::new(kipple_core::Unavailable),
        };
        let engine = Engine::new(ports, integrations, rules).unwrap();
        let request = ScanRequest {
            roots: self.roots(),
            lens: None,
            exclude: Vec::new(),
            protections: self.protections(),
        };
        (engine, request)
    }

    /// The host's protections, as the binary composes them: every built-in integration's
    /// state roots and kipple's data directory, each as the disk identifies it.
    fn protections(&self) -> Protections {
        let held = |at: &Path| HeldPath {
            forms: vec![at.to_owned()],
            lineage: self.lineage(at),
            note: None,
        };
        let data = self.temp.path().join("data");
        Protections::of_host(&self.roots(), Some(&data), &builtin::descriptors(), &held)
    }

    fn lineage(&self, path: &Path) -> Lineage {
        let identity = |dir: &Path| Some(self.disk.identity(dir));
        Lineage(path.ancestors().map(identity).collect())
    }
}

/// What a scan found, by paths relative to the home with `/` separators.
#[derive(Debug)]
pub(crate) struct Scan {
    pub(crate) built: Rc<Built>,
    engine: Engine,
    request: ScanRequest,
    pub(crate) report: ScanReport,
    /// What the scan streamed, in order.
    pub(crate) events: Vec<ScanEvent>,
}

impl Scan {
    /// What apply reads again about the finding at `path`, as the host is now.
    pub(crate) fn now(&self, path: &str) -> Result<Now, Skip> {
        let rescan = self.engine.rescan(&self.request);
        rescan.now(&self.report, self.finding(path), &CancelToken::new())
    }

    /// Plays an install selecting `selected` in `store` after the scan: `current` made
    /// again, then `selection-history` written for it with `previous`.
    pub(crate) fn select(&self, store: &str, selected: &str, previous: &str) {
        let dir = self.built.home().join(component(store));
        let link = dir.join("current");
        let target = self.built.home().join(release(store, selected));
        let stamp = Disk::stamp_of(&target);
        self.built
            .disk
            .relinked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((link, target));
        let history = format!("{selected}\n{previous}\n{stamp}\n");
        fs::write(dir.join("selection-history"), history).unwrap();
    }

    /// Writes `tree` into the host after the scan, as an installer does.
    pub(crate) fn add(&self, tree: &FixtureTree) {
        tree.build_in(self.built.temp.path()).unwrap();
    }

    /// Removes the directory `path`, relative to the home, after the scan, as this run or
    /// another prune does.
    pub(crate) fn remove(&self, path: &str) {
        #[expect(
            clippy::disallowed_methods,
            reason = "the test plays a release removed between two items of a run"
        )]
        fs::remove_dir_all(self.built.home().join(path)).unwrap();
    }

    /// Changes what the process probe shows from here on.
    pub(crate) fn show(&self, change: impl FnOnce(&mut Shown)) {
        change(
            &mut self
                .built
                .shown
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    /// Makes `path`, relative to the home, unreadable from here on.
    pub(crate) fn deny(&self, path: &str) {
        let denied = &self.built.disk.denied;
        let mut denied = denied.lock().unwrap_or_else(PoisonError::into_inner);
        denied.push(self.built.home().join(path));
    }

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
        self.verdict_at(path, &Policy::default())
    }

    /// The verdict at `path`, under `policy`.
    pub(crate) fn verdict_at(&self, path: &str, policy: &Policy) -> Verdict {
        let finding = self.finding(path);
        let facts = finding.facts.as_ref().unwrap();
        policy.verdict(&finding.rule, &finding.path, facts)
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
        let full = self.built.home().join(path);
        let lineage = self.built.lineage(&full);
        self.built
            .protections()
            .check(&full, &lineage, integration, owners)
            .is_some()
    }

    /// [`Self::closes`], for Codex in this scan.
    pub(crate) fn closed(&self, path: &str) -> bool {
        self.closes(path, IntegrationId("codex"), &self.report.owners())
    }

    /// `owners` of this scan, with `integration` supported too.
    pub(crate) fn owners_with(&self, integration: IntegrationId) -> Owners {
        let mut owners = self.report.owners();
        owners.0.insert(integration, OwnerStatus::Supported);
        owners
    }
}
