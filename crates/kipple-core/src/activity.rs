//! Whether a running program uses a finding (`docs/04-safety-model.md` §4, D-035,
//! D-050, D-052). Every path a process shows is compared with the finding by path,
//! ignoring case, and by file identity; a process in another mount namespace is read
//! through its root directory. Which processes count depends on the finding: a project
//! artifact in the home directory leaves out the root and `SYSTEM` processes kipple can't
//! inspect, other users' processes that can't enter it, and on Linux the current user's
//! that can be judged by their command line.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Component, Path, PathBuf, is_separator};
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::engine::Identities;
use crate::integration::ActivityEvidence;
use crate::policy::{Activity, ActivityGap, Lineage, Seen};
use crate::probe::{DirAccess, EntryKind, FileIdentity, FileSystem, FsProbe, ProbeError};
use crate::process::{
    CommandLines, EnvPath, Inventory, Observer, Probed, ProcessClass, ProcessOutcome, ProcessOwner,
    ProcessProbe, ProcessRecord, ProcessSnapshot, Unread,
};

/// `CAP_DAC_OVERRIDE` and `CAP_DAC_READ_SEARCH`: a process with either reads past the
/// mode bits.
const READS_PAST_MODES: u64 = 1 << 1 | 1 << 2;

/// The runtimes that run a project's files without naming them (04 §4). A process of the
/// current user running one can't be judged by its command line (D-052).
const RUNTIMES: &[&str] = &["python*", "node", "bun", "deno"];

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
    /// The kinds of path it shows that couldn't be read, or couldn't be resolved through
    /// its root.
    unread: BTreeSet<Kind>,
    /// How its paths were placed among kipple's.
    placing: Placing,
    cwd: Option<PathBuf>,
    /// The identities of its working directory and each directory above it, when it is
    /// in another mount namespace and they were resolved there.
    cwd_resolved: Option<Vec<FileIdentity>>,
    /// The file names of its executable and of the name it was started by, lowercase.
    names: Vec<String>,
    /// Nothing on its command line can name a path kipple couldn't place: it was read and
    /// every relative path in it resolved, or the OS doesn't show other users' command
    /// lines at all.
    command_line_placed: bool,
    /// Whether it reads past the mode bits.
    modes: Modes,
}

/// Whether a process reads past the mode bits (`CAP_DAC_OVERRIDE` or
/// `CAP_DAC_READ_SEARCH`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modes {
    /// It doesn't, or the OS has no such capabilities.
    Bound,
    /// It does.
    ReadsPast,
    /// Its capabilities couldn't be read.
    Unread,
}

/// How the paths a process shows were placed among kipple's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placing {
    /// In kipple's mount namespace, as kipple names them or below the process's root.
    Placed,
    /// In another mount namespace, through the process's root directory.
    Elsewhere {
        /// Whether its executable resolved there to the file it runs, which places the
        /// rest.
        placed: bool,
    },
    /// Not at all: the process refused its mount namespace or root, as one that isn't
    /// dumpable does.
    Refused,
    /// Not at all: kipple's own mount namespace couldn't be read.
    Unplaced,
}

impl View {
    /// Whether every path of `kinds` it could show was read, through a view of the file
    /// system kipple could place.
    fn inspected(&self, kinds: &[Kind]) -> bool {
        matches!(
            self.placing,
            Placing::Placed | Placing::Elsewhere { placed: true }
        ) && kinds.iter().all(|kind| !self.unread.contains(kind))
    }

    /// The identities of its working directory `cwd` and each directory above it, as the
    /// process sees them.
    fn cwd_lineage(&self, scope: Scope<'_>, cwd: &Path) -> Vec<FileIdentity> {
        self.cwd_resolved.clone().unwrap_or_else(|| {
            let lineage = scope.identities.lineage(scope.fs, cwd);
            lineage.0.into_iter().flatten().collect()
        })
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

    /// Whether it has root's reach, and so is judged like root's processes (D-050): root
    /// and `SYSTEM` themselves, and, on an OS that shows every command line, root's rights
    /// taken by the current user, or another user's process that reads past the mode
    /// bits.
    fn roots_reach(&self, command_lines: CommandLines) -> bool {
        let public = command_lines == CommandLines::Public;
        match self.owner {
            ProcessOwner::Root | ProcessOwner::System => true,
            ProcessOwner::Elevated => public,
            ProcessOwner::OtherUser(_) => public && self.modes == Modes::ReadsPast,
            ProcessOwner::CurrentUser | ProcessOwner::Unknown => false,
        }
    }

    /// Whether, on an OS that shows every command line, it can be judged by its command
    /// line alone when the rest of it can't be read (D-052): it was read and placed, the
    /// process is in kipple's mount namespace or refused to say, and its program is known
    /// and isn't a runtime, which runs a project's files without naming them.
    fn judged_by_command_line(&self) -> bool {
        self.command_line_placed
            && matches!(self.placing, Placing::Placed | Placing::Refused)
            && !self.names.is_empty()
            && !self.runs(RUNTIMES)
    }
}

/// A path a process shows.
#[derive(Debug)]
struct Shown {
    view: usize,
    kind: Kind,
    path: PathBuf,
}

impl Shown {
    /// Which match is reported first: the first process's, by kind, and a path the
    /// process named that holds no whitespace before one that does.
    fn rank(&self) -> (usize, Kind, bool) {
        (self.view, self.kind, self.holds_more())
    }

    /// Whether it is a path the process named itself that holds whitespace: a title
    /// rewritten into one string, read whole, which may carry secrets after the path
    /// (04 §8a).
    fn holds_more(&self) -> bool {
        matches!(self.kind, Kind::Argument | Kind::Environment)
            && self.path.to_string_lossy().contains(char::is_whitespace)
    }

    /// The path to show for a match at or inside `matched`: its own, or `matched` when
    /// its own may carry more than a path.
    fn reported(&self, matched: &Path) -> PathBuf {
        if self.holds_more() {
            matched.to_owned()
        } else {
            self.path.clone()
        }
    }
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
    /// Who the OS shows a process's command line to.
    command_lines: CommandLines,
    /// Whether the OS shows any path of a process besides its executable.
    process_paths: bool,
}

impl Index {
    fn new(
        snapshot: &ProcessSnapshot,
        fs: &dyn FsProbe,
        lineage: &dyn Fn(&Path) -> Lineage,
    ) -> Self {
        let mut index = Self {
            views: Vec::new(),
            shown: Vec::new(),
            by_path: HashMap::new(),
            by_identity: HashMap::new(),
            partial: match &snapshot.inventory {
                Inventory::Complete => None,
                Inventory::Partial { reason } => Some(reason.clone()),
            },
            command_lines: snapshot.observer.command_lines,
            process_paths: snapshot.observer.process_paths,
        };
        for process in snapshot.processes.iter().filter(|p| running(p)) {
            let (view, paths) = view(process, &snapshot.observer, fs);
            let at = index.views.len();
            index.views.push(view);
            for (kind, path) in paths.shown {
                index.add(at, kind, path, lineage);
            }
            for (kind, path, identities) in paths.resolved {
                index.add_resolved(at, kind, path, identities);
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

    /// A path of a process in another mount namespace, by the identities it and each
    /// directory above it have there. Its name means nothing to kipple.
    fn add_resolved(&mut self, view: usize, kind: Kind, path: PathBuf, ids: Vec<FileIdentity>) {
        let at = self.shown.len();
        for identity in ids {
            self.by_identity.entry(identity).or_default().push(at);
        }
        self.shown.push(Shown { view, kind, path });
    }

    /// Every path a process shows at or inside `path`, which has `own` identity. Any kind
    /// of path counts here, whatever the layout needs: a match only ever holds more.
    fn shown_at(&self, path: &Path, own: Option<FileIdentity>) -> impl Iterator<Item = &Shown> {
        let by_path = self.by_path.get(&folded(path)).into_iter().flatten();
        let by_identity = own
            .and_then(|own| self.by_identity.get(&own))
            .into_iter()
            .flatten();
        by_path
            .chain(by_identity)
            .filter_map(|&at| self.shown.get(at))
    }

    /// The first process that shows `subject` in use, and how.
    fn in_use(&self, scope: Scope<'_>, subject: Subject<'_>) -> Option<(u32, Seen)> {
        let pid = |view: usize| self.views.get(view).map(|v| v.pid);
        let inside = self.shown_at(subject.path, subject.lineage.own());
        if let Some(shown) = inside.min_by_key(|shown| shown.rank()) {
            let path = shown.reported(subject.path);
            return Some((pid(shown.view)?, shown.kind.seen(path)));
        }
        let ActivityEvidence::Interpreted { runtimes } = subject.evidence else {
            return None;
        };
        let (view, path) = self.runtime_in(scope, subject.project, runtimes)?;
        Some((pid(view)?, Seen::Runtime(path)))
    }

    /// The first process of one of `runtimes` that works in `project` or names a path
    /// there in its arguments or environment, and that path. A runtime runs a project's
    /// files, which load the project's dependencies without naming them.
    fn runtime_in(
        &self,
        scope: Scope<'_>,
        project: &Path,
        runtimes: &[&str],
    ) -> Option<(usize, PathBuf)> {
        let runs = |view: usize| self.views.get(view).is_some_and(|v| v.runs(runtimes));
        let folded_project = folded(project);
        let identity = scope.identities.identity(scope.fs, project);
        let working = self.views.iter().enumerate().find_map(|(at, view)| {
            let cwd = view.cwd.as_ref().filter(|_| runs(at))?;
            let under = folded(cwd).starts_with(&folded_project)
                || identity
                    .is_some_and(|identity| view.cwd_lineage(scope, cwd).contains(&identity));
            under.then(|| (at, cwd.clone()))
        });
        let naming = self
            .shown_at(project, identity)
            .filter(|shown| matches!(shown.kind, Kind::Argument | Kind::Environment))
            .filter(|shown| runs(shown.view))
            .min_by_key(|shown| shown.rank())
            .map(|shown| (shown.view, shown.reported(project)));
        [working, naming]
            .into_iter()
            .flatten()
            .min_by_key(|(view, _)| *view)
    }
}

/// The paths one process shows, and the kinds of path it couldn't read.
#[derive(Debug, Default)]
struct Paths {
    shown: Vec<(Kind, PathBuf)>,
    /// Which of `shown` the process named itself, from its own root: its arguments and
    /// environment. The kernel shows the others as kipple names them.
    named: Vec<usize>,
    /// Paths of a process in another mount namespace, each with the identities it and
    /// each directory above it have there.
    resolved: Vec<(Kind, PathBuf, Vec<FileIdentity>)>,
    unread: BTreeSet<Kind>,
}

impl Paths {
    fn add(&mut self, kind: Kind, path: PathBuf) {
        self.shown.push((kind, path));
    }

    fn add_named(&mut self, kind: Kind, path: PathBuf) {
        self.named.push(self.shown.len());
        self.add(kind, path);
    }

    /// The fact, or none and `kind` unread.
    fn read<'p, T>(&mut self, kind: Kind, fact: &'p Probed<T>) -> Option<&'p T> {
        if fact.is_err() {
            self.unread.insert(kind);
        }
        fact.as_ref().ok()
    }

    /// Adds the paths `args` name (see [`readings`]): absolute ones as they are, relative
    /// ones in `cwd`, and a bare program name in each directory of its own `PATH` in `env`.
    /// Whether every one could be placed: a token is placed when one of its readings
    /// is, and a relative path can't be without `cwd`.
    fn arguments(&mut self, args: &[OsString], cwd: Option<&PathBuf>, env: &[EnvPath]) -> bool {
        let mut placed = true;
        for (i, arg) in args.iter().enumerate() {
            let slot = if i == 0 {
                Slot::Program
            } else {
                Slot::Argument
            };
            if !self.token(slot, arg, cwd, env) {
                placed = false;
                self.unread.insert(Kind::Argument);
            }
        }
        placed
    }

    /// Adds the paths every reading of one token gives, and whether it could be placed:
    /// by each of its words when it holds whitespace, since any one of them may be a path,
    /// or else whole. Every name is added, whether or not one before it could be placed.
    fn token(&mut self, slot: Slot, arg: &OsStr, cwd: Option<&PathBuf>, env: &[EnvPath]) -> bool {
        let (whole, words) = readings(arg);
        let whole = self.argument(slot, whole, cwd, env);
        let Some(words) = words else {
            return whole;
        };
        words
            .into_iter()
            .map(|word| self.argument(slot, word, cwd, env))
            .fold(true, |all, placed| all & placed)
    }

    /// Adds the path one name on a command line gives, and whether it could be placed.
    fn argument(
        &mut self,
        slot: Slot,
        name: &OsStr,
        cwd: Option<&PathBuf>,
        env: &[EnvPath],
    ) -> bool {
        let named = Path::new(name);
        if !name.to_string_lossy().contains(is_separator) {
            let path = env
                .iter()
                .filter(|e| slot == Slot::Program && e.var == "PATH");
            for entry in path {
                self.add_named(Kind::Argument, entry.path.join(named));
            }
        } else if named.is_absolute() {
            self.add_named(Kind::Argument, named.to_owned());
        } else if let Some(cwd) = cwd {
            self.add(Kind::Argument, cwd.join(named));
        } else {
            return false;
        }
        true
    }
}

/// Where a token is on a command line: a bare name is looked up on `PATH` only as the
/// program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// The first token, the program.
    Program,
    /// Any token after it.
    Argument,
}

/// The readings of one command-line token: the name it gives whole, and when it holds
/// whitespace, as a process title rewritten into one string does, the name each word
/// gives on its own. An option such as `--state=/var/lib/x` gives its value. A token
/// that isn't valid Unicode is only read whole.
fn readings(token: &OsStr) -> (&OsStr, Option<Vec<&OsStr>>) {
    let Some(text) = token.to_str() else {
        return (token, None);
    };
    let whole = OsStr::new(option_value(text));
    let words = text.contains(char::is_whitespace).then(|| {
        text.split_whitespace()
            .map(option_value)
            .map(OsStr::new)
            .collect()
    });
    (whole, words)
}

/// What follows the first `=` of an option (a piece starting with `-`), or the piece.
fn option_value(piece: &str) -> &str {
    piece
        .strip_prefix('-')
        .and_then(|_| piece.split_once('='))
        .map_or(piece, |(_, value)| value)
}

/// How the paths a process shows relate to kipple's.
#[derive(Debug, Clone, Copy)]
enum Frame<'proc> {
    /// They are kipple's own: the same mount namespace and root, or an OS without either.
    Ours,
    /// Kipple's mount namespace with another root, which kipple names this. The kernel
    /// shows the executable, working directory and mapped files as kipple names them;
    /// what the process named itself is below its root.
    Chroot(&'proc Path),
    /// Another mount namespace, whose files kipple reaches through the path given, when
    /// that and the identity of its root directory could be read.
    Elsewhere(Option<(&'proc Path, FileIdentity)>),
    /// The process refused its mount namespace or root.
    Refused,
    /// Kipple's own mount namespace couldn't be read.
    Unknown,
}

impl<'p> Frame<'p> {
    fn of(process: &'p ProcessRecord, observer: &Observer) -> Self {
        match (&observer.mount_namespace, &process.mount_namespace) {
            (Err(Unread::Unsupported), _) => Self::root(process, observer),
            (Ok(ours), Ok(theirs)) if ours == theirs => Self::root(process, observer),
            (Ok(_), Ok(_)) => match (&process.root_entry, &process.root_dir) {
                (Ok(entry), Ok(root)) => Self::Elsewhere(Some((entry, *root))),
                _ => Self::Elsewhere(None),
            },
            (Ok(_), Err(_)) => Self::Refused,
            (Err(Unread::Failed(_)), _) => Self::Unknown,
        }
    }

    /// The frame of a process in kipple's mount namespace, from its root.
    fn root(process: &'p ProcessRecord, observer: &Observer) -> Self {
        if same(&observer.root_dir, &process.root_dir) {
            return Self::Ours;
        }
        process
            .root_path
            .as_ref()
            .map_or(Self::Refused, |root| Self::Chroot(root))
    }
}

/// What `process` shows, and every path it shows, each with its kind.
fn view(process: &ProcessRecord, observer: &Observer, fs: &dyn FsProbe) -> (View, Paths) {
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
        paths.add_named(Kind::Environment, entry.path.clone());
    }
    for file in paths
        .read(Kind::MappedFile, &process.mapped_files)
        .into_iter()
        .flatten()
    {
        paths.add(Kind::MappedFile, file.clone());
    }
    // A command line with no token that isn't blank names nothing, not even its program:
    // it is as good as unread.
    let argv = paths
        .read(Kind::Argument, &process.argv)
        .filter(|args| args.iter().any(|arg| !blank(arg)));
    if argv.is_none() {
        paths.unread.insert(Kind::Argument);
    }
    let placed = argv.map_or(observer.command_lines == CommandLines::Private, |args| {
        paths.arguments(args, cwd, env)
    });
    let first = argv.and_then(|args| args.first()).map(Path::new);
    let frame = Frame::of(process, observer);
    let placing = place(&mut paths, frame, &process.outcome, fs);
    let view = View {
        pid: process.pid,
        owner: process.owner.clone(),
        unread: std::mem::take(&mut paths.unread),
        placing,
        cwd: cwd.cloned(),
        cwd_resolved: paths
            .resolved
            .iter()
            .find(|(kind, ..)| *kind == Kind::WorkingDirectory)
            .map(|(.., ids)| ids.clone()),
        names: names(executable.map(PathBuf::as_path), first),
        command_line_placed: placed,
        modes: match &process.capabilities {
            Ok(caps) if caps.0 & READS_PAST_MODES != 0 => Modes::ReadsPast,
            Ok(_) | Err(Unread::Unsupported) => Modes::Bound,
            Err(Unread::Failed(_)) => Modes::Unread,
        },
    };
    (view, paths)
}

/// Places the paths a process shows in `frame` as kipple sees them, adding what that
/// takes to `paths`.
fn place(
    paths: &mut Paths,
    frame: Frame<'_>,
    outcome: &ProcessOutcome,
    fs: &dyn FsProbe,
) -> Placing {
    match frame {
        Frame::Ours => Placing::Placed,
        Frame::Chroot(root) => {
            let named = paths.named.iter().filter_map(|&at| paths.shown.get(at));
            let below: Vec<_> = named
                .filter_map(|(kind, path)| Some((*kind, below_root(root, path)?)))
                .collect();
            paths.shown.extend(below);
            Placing::Placed
        }
        Frame::Elsewhere(through) => Placing::Elsewhere {
            placed: through.is_some_and(|(entry, root)| elsewhere(paths, entry, root, outcome, fs)),
        },
        Frame::Refused => Placing::Refused,
        Frame::Unknown => Placing::Unplaced,
    }
}

/// `path`, named from a process's own root, as kipple names it when that root is `root`.
fn below_root(root: &Path, path: &Path) -> Option<PathBuf> {
    path.has_root()
        .then(|| root.join(from_root(path).as_path()))
}

/// The components of a path below its root.
fn from_root(path: &Path) -> std::path::Components<'_> {
    let mut parts = path.components();
    while matches!(
        parts.clone().next(),
        Some(Component::Prefix(_) | Component::RootDir)
    ) {
        parts.next();
    }
    parts
}

/// Resolves every path a process in another mount namespace shows through its root
/// directory, reached at `entry`. Its view is placed only when its executable resolves to
/// the file it runs; a path that can't be resolved leaves its kind unread.
fn elsewhere(
    paths: &mut Paths,
    entry: &Path,
    root: FileIdentity,
    outcome: &ProcessOutcome,
    fs: &dyn FsProbe,
) -> bool {
    let ProcessOutcome::Identified {
        path: executable,
        identity,
        ..
    } = outcome
    else {
        return false;
    };
    let mut walk = Walk {
        fs,
        entry,
        root,
        seen: HashMap::new(),
    };
    let running = walk.resolve(executable).filter(|(_, exists)| *exists);
    if running.and_then(|(ids, _)| ids.first().copied()) != Some(*identity) {
        return false;
    }
    for (kind, path) in &paths.shown {
        match walk.resolve(path) {
            Some((ids, _)) => paths.resolved.push((*kind, path.clone(), ids)),
            None => {
                paths.unread.insert(*kind);
            }
        }
    }
    true
}

/// One step of a walk below a process's root: what is there, or that nothing is.
#[derive(Debug, Clone, Copy)]
enum Step {
    Found(EntryKind, FileIdentity),
    Missing,
    Unreadable,
}

/// Paths of one process in another mount namespace, resolved through its root directory.
struct Walk<'probe> {
    fs: &'probe dyn FsProbe,
    entry: &'probe Path,
    root: FileIdentity,
    seen: HashMap<PathBuf, Step>,
}

impl Walk<'_> {
    /// The identities of `path` and of each directory above it up to the process's root,
    /// as the process sees them, the deepest first, and whether `path` itself exists.
    /// Where a directory on the way doesn't exist, nothing below it does. `None` when it
    /// can't be resolved: it isn't absolute, climbs with `..`, holds a link anywhere, the
    /// last part included (which would be followed from kipple's root, not the
    /// process's), or a part of it can't be read.
    fn resolve(&mut self, path: &Path) -> Option<(Vec<FileIdentity>, bool)> {
        if !path.has_root() {
            return None;
        }
        let mut ids = vec![self.root];
        let mut at = self.entry.to_owned();
        for part in from_root(path).filter(|part| *part != Component::CurDir) {
            let Component::Normal(name) = part else {
                return None;
            };
            at.push(name);
            match self.step(&at) {
                Step::Found(EntryKind::Symlink, _) | Step::Unreadable => return None,
                Step::Found(_, identity) => ids.push(identity),
                Step::Missing => return Some((rev(ids), false)),
            }
        }
        Some((rev(ids), true))
    }

    fn step(&mut self, at: &Path) -> Step {
        if let Some(step) = self.seen.get(at) {
            return *step;
        }
        let step = match self.fs.metadata(at) {
            Ok(meta) => meta.identity.map_or(Step::Unreadable, |identity| {
                Step::Found(meta.kind, identity)
            }),
            Err(error) if error.io_kind() == Some(io::ErrorKind::NotFound) => Step::Missing,
            Err(_) => Step::Unreadable,
        };
        self.seen.insert(at.to_owned(), step);
        step
    }
}

fn rev(mut ids: Vec<FileIdentity>) -> Vec<FileIdentity> {
    ids.reverse();
    ids
}

/// The file names of a process's executable and of the name it was started by, and of
/// that name's first word when it holds whitespace (a rewritten title), lowercase.
fn names(executable: Option<&Path>, first: Option<&Path>) -> Vec<String> {
    let word = first
        .and_then(|first| first.to_str())
        .and_then(|first| first.split_whitespace().next())
        .map(Path::new);
    [executable, first, word]
        .into_iter()
        .flatten()
        .filter_map(Path::file_name)
        .filter(|name| !blank(name))
        .map(|name| name.to_string_lossy().to_lowercase())
        .collect()
}

/// Whether `text` is empty or only whitespace.
fn blank(text: &OsStr) -> bool {
    text.to_string_lossy().trim().is_empty()
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
/// directory (D-050, D-052), for `doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessCensus {
    /// Processes with root's reach that are left out: root's, `SYSTEM`'s, and on Linux
    /// root's rights taken by the current user or another user's process that reads past
    /// the mode bits.
    pub unchecked_system: usize,
    /// The current user's processes that are judged by their command line alone.
    pub by_command_line: usize,
    /// Processes that count for every such artifact, so every one stays unknown: the
    /// current user's that can't be judged by their command line, those of an unknown
    /// owner, other users' whose capabilities couldn't be read, and those with root's
    /// reach whose command line names a path kipple couldn't place.
    pub counted: usize,
}

/// The [`ProcessCensus`] of `snapshot`, for an interpreted layout, which needs the most.
/// `fs` reads the processes in another mount namespace through their root.
#[must_use]
pub fn census(snapshot: &ProcessSnapshot, fs: &dyn FsProbe) -> ProcessCensus {
    let mut census = ProcessCensus {
        unchecked_system: 0,
        by_command_line: 0,
        counted: 0,
    };
    let command_lines = snapshot.observer.command_lines;
    let public = command_lines == CommandLines::Public;
    for process in snapshot.processes.iter().filter(|p| running(p)) {
        let (view, _) = view(process, &snapshot.observer, fs);
        if view.inspected(&EVERY_KIND) {
            continue;
        }
        if view.roots_reach(command_lines) && view.command_line_placed {
            census.unchecked_system += 1;
            continue;
        }
        match view.owner {
            ProcessOwner::CurrentUser if public && view.judged_by_command_line() => {
                census.by_command_line += 1;
            }
            ProcessOwner::OtherUser(_) if view.modes == Modes::Bound => {}
            ProcessOwner::Root
            | ProcessOwner::System
            | ProcessOwner::Elevated
            | ProcessOwner::OtherUser(_)
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
pub(crate) struct Subject<'finding> {
    /// Its path.
    pub(crate) path: &'finding Path,
    /// The identities of it and every directory above it.
    pub(crate) lineage: &'finding Lineage,
    /// What its integration says can show it in use.
    pub(crate) evidence: ActivityEvidence,
    /// The directory of its project: a runtime working there or below uses it.
    pub(crate) project: &'finding Path,
}

/// What the scan knows besides the processes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scope<'scan> {
    pub(crate) fs: &'scan dyn FsProbe,
    pub(crate) identities: &'scan Identities,
    /// The identity of the home directory, unless it is unknown or `/`.
    pub(crate) home: Option<FileIdentity>,
}

/// The processes that couldn't be inspected and didn't count for one finding.
#[derive(Debug, Default)]
struct Excused {
    /// Root or `SYSTEM` processes left out.
    system: Vec<u32>,
    /// The current user's processes judged by their command line.
    by_command_line: Vec<u32>,
}

/// The processes of one scan, listed once when the first finding needs them.
#[derive(Debug, Default)]
pub(crate) struct Processes {
    index: OnceLock<Result<Index, ProbeError>>,
    access: Mutex<HashMap<PathBuf, Option<DirAccess>>>,
    left_out: Mutex<BTreeSet<u32>>,
    by_command_line: Mutex<BTreeSet<u32>>,
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
                .map(|snapshot| Index::new(&snapshot, scope.fs, &lineage))
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
        let needs_paths = kinds(subject.evidence) != [Kind::Executable];
        if needs_paths && !index.process_paths {
            return Activity::Unknown(ActivityGap::Unsupported);
        }
        self.uninspected(index, scope, subject)
    }

    /// How many processes with root's reach (root's, `SYSTEM`'s, and on Linux root's
    /// rights taken or a capability to read past the mode bits) were left out of some
    /// finding that was idle apart from them.
    pub(crate) fn left_out(&self) -> usize {
        self.left_out
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// How many of the current user's processes were judged by their command line alone
    /// for some finding that was idle apart from them.
    pub(crate) fn by_command_line(&self) -> usize {
        self.by_command_line
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
        let mut excused = Excused::default();
        for view in index.views.iter().filter(|view| !view.inspected(kinds)) {
            if !scoped || !in_home {
                counted += 1;
                continue;
            }
            let public = index.command_lines == CommandLines::Public;
            let counts = match &view.owner {
                _ if view.roots_reach(index.command_lines) => {
                    excused
                        .system
                        .extend(view.command_line_placed.then_some(view.pid));
                    !view.command_line_placed
                }
                ProcessOwner::CurrentUser => {
                    let judged = public && view.judged_by_command_line();
                    excused.by_command_line.extend(judged.then_some(view.pid));
                    !judged
                }
                ProcessOwner::OtherUser(_) => {
                    view.modes != Modes::Bound
                        || !*shut_out.get_or_insert_with(|| self.others_shut_out(scope, subject))
                }
                ProcessOwner::Root
                | ProcessOwner::System
                | ProcessOwner::Elevated
                | ProcessOwner::Unknown => true,
            };
            counted += usize::from(counts);
        }
        if counted > 0 {
            return Activity::Unknown(ActivityGap::Uninspected(counted));
        }
        self.excused(excused)
    }

    /// Idle, recording the processes that were left out or judged by their command line.
    fn excused(&self, excused: Excused) -> Activity {
        if excused.system.is_empty() && excused.by_command_line.is_empty() {
            return Activity::Idle;
        }
        let activity = Activity::IdleExceptSystem {
            unchecked: excused.system.len(),
            by_command_line: excused.by_command_line.len(),
        };
        let record = |set: &Mutex<BTreeSet<u32>>, pids: Vec<u32>| {
            set.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(pids);
        };
        record(&self.left_out, excused.system);
        record(&self.by_command_line, excused.by_command_line);
        activity
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
