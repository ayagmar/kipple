//! Whether a running program uses a finding (`docs/04-safety-model.md` §4, D-035,
//! D-050). Every path a process shows is compared with the finding by path, ignoring
//! case, and by file identity. Which processes count depends on the finding: a project
//! artifact in the home directory leaves out the root and `SYSTEM` processes kipple can't
//! inspect, and other users' processes that can't enter it.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf, is_separator};
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::engine::Identities;
use crate::integration::ActivityEvidence;
use crate::policy::{Activity, ActivityGap, Lineage, Seen};
use crate::probe::{DirAccess, FileIdentity, FileSystem, FsProbe, ProbeError};
use crate::process::{
    EnvPath, Inventory, Observer, Probed, ProcessClass, ProcessOutcome, ProcessOwner, ProcessProbe,
    ProcessRecord, ProcessSnapshot, Unread,
};

/// `CAP_DAC_OVERRIDE` and `CAP_DAC_READ_SEARCH`: a process with either reads past the
/// mode bits.
const READS_PAST_MODES: u64 = 1 << 1 | 1 << 2;

/// A kind of path a process shows, in the order a match is reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Executable,
    WorkingDirectory,
    Argument,
    Environment,
    MappedFile,
}

const EVERY_KIND: [Kind; 5] = [
    Kind::Executable,
    Kind::WorkingDirectory,
    Kind::Argument,
    Kind::Environment,
    Kind::MappedFile,
];

impl Kind {
    const fn seen(self, path: PathBuf) -> Seen {
        match self {
            Self::Executable => Seen::Executable(path),
            Self::WorkingDirectory => Seen::WorkingDirectory(path),
            Self::Argument => Seen::Argument(path),
            Self::Environment => Seen::Environment(path),
            Self::MappedFile => Seen::MappedFile(path),
        }
    }
}

/// The kinds of path a finding with `evidence` needs read: a process that couldn't show
/// one of them can't be ruled out.
const fn kinds(evidence: ActivityEvidence) -> &'static [Kind] {
    match evidence {
        ActivityEvidence::BuiltExecutables => &[Kind::Executable],
        ActivityEvidence::Everything | ActivityEvidence::Interpreted { .. } => &EVERY_KIND,
    }
}

/// What one running process shows, apart from its paths.
#[derive(Debug)]
struct View {
    pid: u32,
    owner: ProcessOwner,
    /// The kinds of path it shows that couldn't be read.
    unread: BTreeSet<Kind>,
    /// It names files by other paths than kipple does (another mount namespace or root),
    /// or that couldn't be ruled out.
    foreign: bool,
    cwd: Option<PathBuf>,
    /// The file names of its executable and of the name it was started by, lowercase.
    names: Vec<String>,
    /// Nothing on its command line can name a path kipple couldn't place: it was read and
    /// every relative path in it resolved, or the OS doesn't show other users' command
    /// lines at all.
    command_line_placed: bool,
    /// It may read past the mode bits, or that couldn't be ruled out.
    reads_past_modes: bool,
}

impl View {
    /// Whether every path of `kinds` it could show was read, through the same view of the
    /// file system as kipple's.
    fn inspected(&self, kinds: &[Kind]) -> bool {
        !self.foreign && kinds.iter().all(|kind| !self.unread.contains(kind))
    }

    fn runs(&self, runtimes: &[&str]) -> bool {
        self.names.iter().any(|name| {
            runtimes.iter().any(|runtime| {
                runtime.strip_suffix('*').map_or_else(
                    || *name == runtime.to_lowercase(),
                    |prefix| name.starts_with(&prefix.to_lowercase()),
                )
            })
        })
    }
}

/// A path a process shows.
#[derive(Debug)]
struct Shown {
    view: usize,
    kind: Kind,
    path: PathBuf,
}

/// The processes of one scan, with every path each one shows indexed under itself and
/// every directory above it, by path and by identity.
#[derive(Debug)]
struct Index {
    views: Vec<View>,
    shown: Vec<Shown>,
    by_path: HashMap<Vec<String>, Vec<usize>>,
    by_identity: HashMap<FileIdentity, Vec<usize>>,
    partial: Option<String>,
}

impl Index {
    fn new(snapshot: &ProcessSnapshot, lineage: &dyn Fn(&Path) -> Lineage) -> Self {
        let mut index = Self {
            views: Vec::new(),
            shown: Vec::new(),
            by_path: HashMap::new(),
            by_identity: HashMap::new(),
            partial: match &snapshot.inventory {
                Inventory::Complete => None,
                Inventory::Partial { reason } => Some(reason.clone()),
            },
        };
        for process in snapshot.processes.iter().filter(|p| running(p)) {
            let (view, paths) = view(process, &snapshot.observer);
            let at = index.views.len();
            index.views.push(view);
            for (kind, path) in paths {
                index.add(at, kind, path, lineage);
            }
        }
        index
    }

    fn add(&mut self, view: usize, kind: Kind, path: PathBuf, lineage: &dyn Fn(&Path) -> Lineage) {
        let at = self.shown.len();
        let parts = folded(&path);
        for end in 1..=parts.len() {
            let key = parts.get(..end).unwrap_or_default().to_vec();
            self.by_path.entry(key).or_default().push(at);
        }
        for identity in lineage(&path).0.into_iter().flatten() {
            self.by_identity.entry(identity).or_default().push(at);
        }
        self.shown.push(Shown { view, kind, path });
    }

    /// The first path a process shows at or inside `path`, which has `own` identity. Any
    /// kind of path counts here, whatever the layout needs: a match only ever holds more.
    fn shown_inside(&self, path: &Path, own: Option<FileIdentity>) -> Option<&Shown> {
        let by_path = self.by_path.get(&folded(path)).into_iter().flatten();
        let by_identity = own
            .and_then(|own| self.by_identity.get(&own))
            .into_iter()
            .flatten();
        by_path
            .chain(by_identity)
            .filter_map(|&at| self.shown.get(at))
            .min_by_key(|shown| (shown.view, shown.kind))
    }

    /// The first process that shows `subject` in use, and how.
    fn in_use(&self, scope: Scope<'_>, subject: Subject<'_>) -> Option<(u32, Seen)> {
        let pid = |view: usize| self.views.get(view).map(|v| v.pid);
        if let Some(shown) = self.shown_inside(subject.path, subject.lineage.own()) {
            return Some((pid(shown.view)?, shown.kind.seen(shown.path.clone())));
        }
        let ActivityEvidence::Interpreted { runtimes } = subject.evidence else {
            return None;
        };
        let project = folded(subject.project);
        let identity = scope.identities.identity(scope.fs, subject.project);
        self.views.iter().find_map(|view| {
            let cwd = view.cwd.as_ref().filter(|_| view.runs(runtimes))?;
            let under = folded(cwd).starts_with(&project)
                || identity.is_some_and(|identity| {
                    let lineage = scope.identities.lineage(scope.fs, cwd);
                    lineage.0.contains(&Some(identity))
                });
            under.then(|| (view.pid, Seen::Runtime(cwd.clone())))
        })
    }
}

/// The paths one process shows, and the kinds of path it couldn't read.
#[derive(Debug, Default)]
struct Paths {
    shown: Vec<(Kind, PathBuf)>,
    unread: BTreeSet<Kind>,
}

impl Paths {
    fn add(&mut self, kind: Kind, path: PathBuf) {
        self.shown.push((kind, path));
    }

    /// The fact, or none and `kind` unread.
    fn read<'p, T>(&mut self, kind: Kind, fact: &'p Probed<T>) -> Option<&'p T> {
        if fact.is_err() {
            self.unread.insert(kind);
        }
        fact.as_ref().ok()
    }

    /// Adds the paths `args` name: absolute ones as they are, relative ones in `cwd`, and
    /// a bare program name in each directory of its own `PATH` in `env`. Whether every
    /// one could be placed: a relative one can't without `cwd`.
    fn arguments(&mut self, args: &[OsString], cwd: Option<&PathBuf>, env: &[EnvPath]) -> bool {
        let mut placed = true;
        for (i, arg) in args.iter().enumerate() {
            let named = Path::new(arg);
            if !arg.to_string_lossy().contains(is_separator) {
                let path = env.iter().filter(|e| i == 0 && e.var == "PATH");
                self.shown
                    .extend(path.map(|e| (Kind::Argument, e.path.join(named))));
            } else if named.is_absolute() {
                self.add(Kind::Argument, named.to_owned());
            } else if let Some(cwd) = cwd {
                self.add(Kind::Argument, cwd.join(named));
            } else {
                placed = false;
                self.unread.insert(Kind::Argument);
            }
        }
        placed
    }
}

/// What `process` shows, and every path it shows, each with its kind.
fn view(process: &ProcessRecord, observer: &Observer) -> (View, Vec<(Kind, PathBuf)>) {
    let mut paths = Paths::default();
    let executable = match &process.outcome {
        ProcessOutcome::Identified { path, .. } | ProcessOutcome::PathOnly { path, .. } => {
            Some(path)
        }
        ProcessOutcome::Denied(_)
        | ProcessOutcome::Other(_)
        | ProcessOutcome::Exited
        | ProcessOutcome::Gone
        | ProcessOutcome::NoExecutable => None,
    };
    if process.outcome.class() != ProcessClass::Identified {
        paths.unread.insert(Kind::Executable);
    }
    if let Some(path) = executable {
        paths.add(Kind::Executable, path.clone());
    }
    let cwd = paths.read(Kind::WorkingDirectory, &process.cwd);
    if let Some(cwd) = cwd {
        paths.add(Kind::WorkingDirectory, cwd.clone());
    }
    let env = paths.read(Kind::Environment, &process.env_paths);
    let env = env.map_or(&[][..], Vec::as_slice);
    for entry in env {
        paths.add(Kind::Environment, entry.path.clone());
    }
    for file in paths
        .read(Kind::MappedFile, &process.mapped_files)
        .into_iter()
        .flatten()
    {
        paths.add(Kind::MappedFile, file.clone());
    }
    let argv = paths.read(Kind::Argument, &process.argv);
    let placed = argv.map_or(!observer.command_lines_public, |args| {
        paths.arguments(args, cwd, env)
    });
    let first = argv.and_then(|args| args.first()).map(Path::new);
    let view = View {
        pid: process.pid,
        owner: process.owner.clone(),
        unread: paths.unread,
        foreign: !same(&observer.mount_namespace, &process.mount_namespace)
            || !same(&observer.root_dir, &process.root_dir),
        cwd: cwd.cloned(),
        names: names(executable.map(PathBuf::as_path), first),
        command_line_placed: placed,
        reads_past_modes: match &process.capabilities {
            Ok(caps) => caps.0 & READS_PAST_MODES != 0,
            Err(Unread::Unsupported) => false,
            Err(Unread::Failed(_)) => true,
        },
    };
    (view, paths.shown)
}

/// The file names of a process's executable and of the name it was started by, lowercase.
fn names(executable: Option<&Path>, first: Option<&Path>) -> Vec<String> {
    [executable, first]
        .into_iter()
        .flatten()
        .filter_map(Path::file_name)
        .map(|name| name.to_string_lossy().to_lowercase())
        .collect()
}

/// Whether a process sees the same thing as kipple: always, where the OS has no such
/// thing to compare; otherwise only when both were read and agree.
fn same<T: PartialEq>(observer: &Result<T, Unread>, process: &Result<T, Unread>) -> bool {
    match (observer, process) {
        (Err(Unread::Unsupported), _) => true,
        (Ok(ours), Ok(theirs)) => ours == theirs,
        _ => false,
    }
}

/// `path`'s components, lowercase: a different case only ever matches more.
fn folded(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// What the processes that couldn't be inspected mean for a project artifact in the home
/// directory (D-050), for `doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessCensus {
    /// Root or `SYSTEM` processes that are left out.
    pub unchecked_system: usize,
    /// Processes that count for every such artifact, so every one stays unknown: the
    /// current user's, those of an unknown owner, and root or `SYSTEM` ones whose command
    /// line names a path kipple couldn't place.
    pub counted: usize,
}

/// The [`ProcessCensus`] of `snapshot`, for an interpreted layout, which needs the most.
#[must_use]
pub fn census(snapshot: &ProcessSnapshot) -> ProcessCensus {
    let mut census = ProcessCensus {
        unchecked_system: 0,
        counted: 0,
    };
    for process in snapshot.processes.iter().filter(|p| running(p)) {
        let (view, _) = view(process, &snapshot.observer);
        if view.inspected(&EVERY_KIND) {
            continue;
        }
        match view.owner {
            ProcessOwner::Root | ProcessOwner::System if view.command_line_placed => {
                census.unchecked_system += 1;
            }
            ProcessOwner::OtherUser(_) => {}
            ProcessOwner::Root
            | ProcessOwner::System
            | ProcessOwner::CurrentUser
            | ProcessOwner::Unknown => census.counted += 1,
        }
    }
    census
}

/// Whether `process` may be running a file: it was neither gone nor a kernel thread.
const fn running(process: &ProcessRecord) -> bool {
    matches!(
        process.outcome.class(),
        ProcessClass::Identified | ProcessClass::Unknown
    )
}

/// What one finding's activity is judged on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Subject<'a> {
    /// Its path.
    pub(crate) path: &'a Path,
    /// The identities of it and every directory above it.
    pub(crate) lineage: &'a Lineage,
    /// What its integration says can show it in use.
    pub(crate) evidence: ActivityEvidence,
    /// The directory of its project: a runtime working there or below uses it.
    pub(crate) project: &'a Path,
}

/// What the scan knows besides the processes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scope<'a> {
    pub(crate) fs: &'a dyn FsProbe,
    pub(crate) identities: &'a Identities,
    /// The identity of the home directory, unless it is unknown or `/`.
    pub(crate) home: Option<FileIdentity>,
}

/// The processes of one scan, listed once when the first finding needs them.
#[derive(Debug, Default)]
pub(crate) struct Processes {
    index: OnceLock<Result<Index, ProbeError>>,
    access: Mutex<HashMap<PathBuf, Option<DirAccess>>>,
    left_out: Mutex<BTreeSet<u32>>,
}

impl Processes {
    /// Whether a program uses `subject`. A process that counts and couldn't be inspected
    /// might be the one, so the answer is then unknown, never idle.
    pub(crate) fn activity(
        &self,
        probe: &dyn ProcessProbe,
        scope: Scope<'_>,
        subject: Subject<'_>,
    ) -> Activity {
        let built = self.index.get_or_init(|| {
            let lineage = |path: &Path| scope.identities.lineage(scope.fs, path);
            probe
                .snapshot()
                .map(|snapshot| Index::new(&snapshot, &lineage))
        });
        let index = match built {
            Ok(index) => index,
            Err(error) => return Activity::Unknown(ActivityGap::NoListing(error.clone())),
        };
        if let Some((pid, seen)) = index.in_use(scope, subject) {
            return Activity::InUse { pid, seen };
        }
        if let Some(reason) = &index.partial {
            return Activity::Unknown(ActivityGap::Partial(reason.clone()));
        }
        self.uninspected(index, scope, subject)
    }

    /// How many root or `SYSTEM` processes were left out of some finding that was idle
    /// apart from them.
    pub(crate) fn left_out(&self) -> usize {
        self.left_out
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Idle or unknown, from the processes that count for `subject` and couldn't be
    /// inspected.
    fn uninspected(&self, index: &Index, scope: Scope<'_>, subject: Subject<'_>) -> Activity {
        let kinds = kinds(subject.evidence);
        let scoped = subject.evidence != ActivityEvidence::Everything;
        let in_home = scope
            .home
            .is_some_and(|home| subject.lineage.0.contains(&Some(home)));
        let mut shut_out = None;
        let mut counted = 0;
        let mut left_out = Vec::new();
        for view in index.views.iter().filter(|view| !view.inspected(kinds)) {
            if !scoped || !in_home {
                counted += 1;
                continue;
            }
            let counts = match &view.owner {
                ProcessOwner::CurrentUser | ProcessOwner::Unknown => true,
                ProcessOwner::Root | ProcessOwner::System => {
                    left_out.extend(view.command_line_placed.then_some(view.pid));
                    !view.command_line_placed
                }
                ProcessOwner::OtherUser(_) => {
                    view.reads_past_modes
                        || !*shut_out.get_or_insert_with(|| self.others_shut_out(scope, subject))
                }
            };
            counted += usize::from(counts);
        }
        if counted > 0 {
            return Activity::Unknown(ActivityGap::Uninspected(counted));
        }
        if left_out.is_empty() {
            return Activity::Idle;
        }
        let unchecked = left_out.len();
        self.left_out
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(left_out);
        Activity::IdleExceptSystem { unchecked }
    }

    /// Whether no other user can enter `subject`: it is on a local file system, and some
    /// directory from the home directory down to it is the current user's, with no group
    /// or other execute bit and no access control list that grants anything.
    fn others_shut_out(&self, scope: Scope<'_>, subject: Subject<'_>) -> bool {
        let local = self
            .access(scope.fs, subject.path)
            .is_some_and(|access| access.file_system == FileSystem::Local);
        if !local {
            return false;
        }
        for (dir, identity) in subject.path.ancestors().zip(&subject.lineage.0) {
            let private = self.access(scope.fs, dir).is_some_and(|access| {
                access.owned_by_current_user && !access.open_to_others && !access.acl
            });
            if private {
                return true;
            }
            if identity.is_some() && *identity == scope.home {
                break;
            }
        }
        false
    }

    fn access(&self, fs: &dyn FsProbe, dir: &Path) -> Option<DirAccess> {
        let known = |cache: &Self| {
            let cache = cache.access.lock().unwrap_or_else(PoisonError::into_inner);
            cache.get(dir).copied()
        };
        if let Some(access) = known(self) {
            return access;
        }
        let access = fs.access(dir).ok();
        let mut cache = self.access.lock().unwrap_or_else(PoisonError::into_inner);
        cache.insert(dir.to_owned(), access);
        access
    }
}
