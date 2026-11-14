//! Running a tool's own reviewed command (`docs/04-safety-model.md` §3.4, §6, §7; spec M4
//! pinned 26–30). It starts through the runner, never through a shell, with a closed
//! environment, from a directory handle the confined walk opened, and the executable
//! checked by identity right before the spawn. Its deadline kills only its own child.
//!
//! The child starts in `/proc/self/fd/<n>`, so its working directory is the checked inode
//! whatever its ancestors have become. Only Linux names an open handle that way: on macOS
//! and Windows every start is refused, and nothing runs.
//!
//! After the command ends the runner reads its postcondition through handles it opened
//! before the spawn, and syncs what it changed. A failed sync never reads as done.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use kipple_core::{
    Anchor, ApplyError, EntryKind, ErrorCode, FileIdentity, NativeArg, NativeCommand, NativeEnd,
    NativeExit, NativeOp, NativeOutput, NativeRunner, NativeStatus, Postcondition, ResolvedTool,
    ToolFile, WorktreeWitness,
};

use crate::environment::Environment;
use crate::executor::{self, sys};
use crate::os;
use crate::os::folder::Folder;
use crate::runner::{self, CheckedDir, Finish, Invocation, OUTPUT_LIMIT, Ran, Var};
use crate::tools;

/// A file that is always empty and that no one can replace: the global config, the
/// hooks directory and, on Linux and macOS, the XDG config directory all point here, so
/// git finds nothing in them.
#[cfg(unix)]
const NULL_DEVICE: &str = "/dev/null";
#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";

/// How long `git worktree remove` may take (pinned 28).
pub(crate) const WORKTREE_DEADLINE: Duration = Duration::from_mins(5);

/// How long `npm cache clean` may take: pinned 28's 10 minutes, kept until the contract
/// test (`tests/native/npm.rs`) has measured real npm in CI, which fails above a twentieth
/// of it. Only a proxy was measured here: node's `fs.rmSync` removing a 20,000-file
/// `_cacache` (cacache's index and content, half each) took 0.41 to 0.52 s on a btrfs SSD
/// and 0.11 to 0.16 s on tmpfs (Ryzen 9 9950X3D, node 26.13.0), without npm's own start.
/// Missing it leaves a partial cache, which npm reads as misses.
pub(crate) const NPM_DEADLINE: Duration = Duration::from_mins(10);

/// Where npm's configuration boundary lives below kipple's data directory, one name at a
/// time: the prefix and logs directories and the two config files `NPM_CACHE_CLEAN`
/// names, all empty.
const NPM_BOUNDARY: [&str; 2] = ["native", "npm"];
const NPM_DIRS: [&str; 2] = ["prefix", "logs"];
const NPM_FILES: [&str; 2] = ["userconfig", "globalconfig"];

/// The variables git gets beside the runner's, for a query and a removal alike: no system
/// or global config, no XDG config directory, no optional locks, no pager and no lazy
/// fetch. Only some releases honour that last one and no version number says which, so it
/// backs the repository config gate (`git/worktree.rs`) and never replaces it.
pub(crate) fn git_isolation() -> Vec<Var<'static>> {
    let var = |name, value| Var {
        name,
        value: OsStr::new(value),
    };
    Vec::from([
        var("GIT_CONFIG_NOSYSTEM", "1"),
        var("GIT_CONFIG_GLOBAL", NULL_DEVICE),
        var("GIT_OPTIONAL_LOCKS", "0"),
        var("GIT_PAGER", "cat"),
        var("GIT_NO_LAZY_FETCH", "1"),
        // Both of these are second lines. What holds is that a promisor is refused, so no
        // fetch starts: no version number says which gits honour the line above, and a
        // fetch that has started can reach what this list never sees, as `fetch.bundleURI`
        // does. Git can't be told to list nothing, so it lists one name no alias can hold:
        // empty or `:` would list the empty name, which `::x` as a remote's URL picks.
        var("GIT_ALLOW_PROTOCOL", "\n"),
        // Windows has no directory that can't hold a file: its XDG directory stays unset,
        // as `HOME` is, so git looks for none.
        #[cfg(unix)]
        var("XDG_CONFIG_HOME", NULL_DEVICE),
    ])
}

/// `args` as a process gets them, with `data` for kipple's own paths. `None` when one
/// needs `data` and there is none, so nothing runs without its boundary.
pub(crate) fn rendered(args: &[NativeArg], data: Option<&Path>) -> Option<Vec<OsString>> {
    args.iter()
        .map(|arg| match arg {
            NativeArg::Fixed(text) => Some(OsString::from(text)),
            NativeArg::NullDevice(key) => {
                let mut text = OsString::from(key);
                text.push(NULL_DEVICE);
                Some(text)
            }
            NativeArg::Data(key, below) => {
                let mut text = OsString::from(key);
                text.push(data?.join(below));
                Some(text)
            }
        })
        .collect()
}

/// The platform's [`NativeRunner`].
#[derive(Debug)]
pub struct NativeCommands {
    env: Environment,
    data: Option<PathBuf>,
    deadlines: Deadlines,
}

/// How long each command may take.
#[derive(Debug, Clone, Copy)]
struct Deadlines {
    worktree: Duration,
    npm: Duration,
}

impl NativeCommands {
    /// How long `npm cache clean` may run before it is killed. Public for the contract
    /// test, which judges the measured clean against it.
    #[doc(hidden)]
    pub const NPM_DEADLINE: Duration = NPM_DEADLINE;

    /// The runner for the machine `env` describes, with kipple's data directory `data`
    /// when it has one.
    #[must_use]
    pub fn new(env: &Environment, data: Option<PathBuf>) -> Self {
        Self {
            env: env.clone(),
            data,
            deadlines: Deadlines {
                worktree: WORKTREE_DEADLINE,
                npm: NPM_DEADLINE,
            },
        }
    }

    /// This runner with every command's deadline `deadline`, for the test that misses one.
    #[cfg(all(test, target_os = "linux"))]
    const fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadlines = Deadlines {
            worktree: deadline,
            npm: deadline,
        };
        self
    }

    /// npm's variables beside the runner's: `HOME` alone, where it has one (pinned 27).
    fn npm_vars(&self) -> Vec<Var<'_>> {
        self.env
            .get("HOME")
            .map(|value| Var {
                name: "HOME",
                value,
            })
            .into_iter()
            .collect()
    }
}

impl NativeRunner for NativeCommands {
    fn run(&self, op: &NativeOp) -> Result<NativeExit, ApplyError> {
        let spec = op.spec();
        let (vars, deadline) = match &spec.command {
            NativeCommand::NpmCacheClean { .. } => (self.npm_vars(), self.deadlines.npm),
            NativeCommand::GitWorktreeRemove { .. } => (git_isolation(), self.deadlines.worktree),
        };
        let args = rendered(&spec.command.argv(), self.data.as_deref())
            .ok_or_else(|| ApplyError::new(ErrorCode::Unsupported))?;
        // A shim is refused before anything else is opened, on every OS.
        let launch = Launch::of(&spec.tool, vars, deadline, args)?;
        match &spec.command {
            NativeCommand::NpmCacheClean { cache } => self.clean_npm(op, cache, &launch),
            NativeCommand::GitWorktreeRemove { checkout, witness } => {
                self.remove_worktree(op, checkout, witness, &launch)
            }
        }
    }
}

/// What a command runs as: the program that starts, the arguments before the command's
/// own, the command's, the variables beside the runner's, and how long it may take.
#[derive(Debug)]
struct Launch<'tool> {
    program: &'tool Path,
    lead: Option<&'tool OsStr>,
    args: Vec<OsString>,
    vars: Vec<Var<'tool>>,
    deadline: Duration,
}

impl<'tool> Launch<'tool> {
    /// `tool` started directly, or as `<runtime> <script>` when it is a script. A shell
    /// shim never starts: kipple runs no shell (pinned 27).
    fn of(
        tool: &'tool ResolvedTool,
        vars: Vec<Var<'tool>>,
        deadline: Duration,
        args: Vec<OsString>,
    ) -> Result<Self, ApplyError> {
        let files = std::iter::once(tool.path.as_path())
            .chain(tool.runtime.as_ref().map(|runtime| runtime.path.as_path()));
        for file in files {
            if is_shim(file) {
                return Err(ApplyError::new(ErrorCode::ToolChanged));
            }
        }
        Ok(match &tool.runtime {
            Some(runtime) => Self {
                program: &runtime.path,
                lead: Some(tool.path.as_os_str()),
                args,
                vars,
                deadline,
            },
            None => Self {
                program: &tool.path,
                lead: None,
                args,
                vars,
                deadline,
            },
        })
    }

    /// Runs it from `dir`, the executable checked against `tool` first, and waits for it
    /// to end. `Err` only when it never started.
    fn run(
        &self,
        tool: &ResolvedTool,
        dir: &CheckedDir,
        env: &Environment,
    ) -> Result<Ran, ApplyError> {
        let args: Vec<&OsStr> = self
            .lead
            .into_iter()
            .chain(self.args.iter().map(OsString::as_os_str))
            .collect();
        let invocation = Invocation {
            program: self.program,
            args: &args,
            vars: &self.vars,
            deadline: self.deadline,
            limit: OUTPUT_LIMIT,
            // Ctrl+C waits for the command, as for any running item (04 §7).
            cancel: None,
            dir: Some(dir),
        };
        still_reviewed(tool)?;
        // Only a start fails: once the child runs, how it ended comes back in `Ran`.
        runner::invoke_to_end(&invocation, env).map_err(|error| {
            ApplyError::io(
                ErrorCode::Io,
                io::Error::other(format!("its tool's command didn't start: {error}")),
            )
        })
    }
}

/// Whether `path` names a Windows shell shim, which `cmd.exe` would run.
fn is_shim(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        ["cmd", "bat"]
            .iter()
            .any(|shim| extension.eq_ignore_ascii_case(shim))
    })
}

/// Whether each file `tool` runs from is still the file a person reviewed, by identity
/// and fingerprint, so neither another file in its inode nor a rewrite in place passes:
/// the last check before the spawn.
fn still_reviewed(tool: &ResolvedTool) -> Result<(), ApplyError> {
    let files = std::iter::once(ToolFile {
        path: tool.path.clone(),
        identity: tool.identity,
        fingerprint: tool.fingerprint,
    })
    .chain(tool.runtime.clone());
    for reviewed in files {
        let now = tools::tool_file(&reviewed.path)
            .map_err(|error| ApplyError::io(ErrorCode::ToolChanged, error))?;
        let same = now.kind == EntryKind::File
            && now.identity == reviewed.identity
            && now.fingerprint == reviewed.fingerprint;
        if !same {
            return Err(ApplyError::new(ErrorCode::ToolChanged));
        }
    }
    Ok(())
}

/// A directory the child can start in: one this OS can name by its open handle.
fn startable(dir: &CheckedDir) -> Result<(), ApplyError> {
    dir.start_path()
        .map(drop)
        .ok_or_else(|| ApplyError::new(ErrorCode::Unsupported))
}

/// The last name of `anchor`'s path, as the OS takes it: what its target is called in the
/// directory holding it.
fn last_name(anchor: &Anchor) -> Result<sys::Name, ApplyError> {
    let last = anchor
        .relative
        .components()
        .last()
        .ok_or_else(|| ApplyError::new(ErrorCode::TargetChanged))?;
    sys::name(last.as_os_str()).map_err(|error| ApplyError::io(ErrorCode::Io, error))
}

/// `name` as the OS takes it.
fn os_name(name: &str) -> Result<sys::Name, ApplyError> {
    sys::name(OsStr::new(name)).map_err(|error| ApplyError::io(ErrorCode::Io, error))
}

/// What failing to reach a reviewed entry says: missing when it isn't there, changed when
/// something else is.
fn unreached(error: io::Error) -> ApplyError {
    let code = if error.kind() == io::ErrorKind::NotFound {
        ErrorCode::TargetMissing
    } else {
        ErrorCode::TargetChanged
    };
    ApplyError::io(code, error)
}

/// Fails unless `seen` is of `kind` with `identity`: what was reviewed, still there.
fn is_reviewed(
    seen: io::Result<executor::Seen>,
    kind: EntryKind,
    identity: FileIdentity,
) -> Result<(), ApplyError> {
    let seen = seen.map_err(unreached)?;
    if seen.snapshot.kind == kind && seen.snapshot.identity == identity {
        Ok(())
    } else {
        Err(ApplyError::new(ErrorCode::TargetChanged))
    }
}

/// The reviewed identity of `op`'s target's top.
fn reviewed_top(op: &NativeOp) -> Result<FileIdentity, ApplyError> {
    op.target()
        .tree()
        .top()
        .snapshot()
        .map(|top| top.identity)
        .ok_or_else(|| ApplyError::new(ErrorCode::TargetChanged))
}

/// Whether `name` is gone from `dir`, as only an answer of "not found" says, and the
/// directory `held`, opened as `name` before the run, has no name left anywhere: no
/// rename, of it or of an ancestor, passes for a removal.
fn removed(dir: &sys::Dir, name: &sys::Name, held: &sys::Dir) -> Postcondition {
    let gone = matches!(dir.stat(name), Err(error) if error.kind() == io::ErrorKind::NotFound);
    let unlinked = held.seen().is_ok_and(|seen| seen.snapshot.links == 0);
    if gone && unlinked {
        Postcondition::Met
    } else {
        Postcondition::Unmet
    }
}

/// How a process that ended with `status` ended.
fn end_of(status: ExitStatus) -> NativeEnd {
    NativeEnd {
        exit: status.code(),
        signal: os::exit_signal(status),
    }
}

/// What the runner saw of a command that ran as `ran`.
fn exit(ran: Ran, postcondition: Postcondition, synced: io::Result<()>) -> NativeExit {
    NativeExit {
        status: match ran.finish {
            Finish::Exited(status) => NativeStatus::Ended(end_of(status)),
            Finish::MissedDeadline(status) | Finish::Cancelled(status) => {
                NativeStatus::TimedOut(end_of(status))
            }
            Finish::Lost(lost) => NativeStatus::Lost {
                stopped: lost.stopped.ok().map(end_of),
            },
        },
        postcondition,
        synced: synced.map_err(|error| ApplyError::io(ErrorCode::Unsynced, error)),
        output: NativeOutput {
            stdout: ran.stdout,
            stderr: ran.stderr,
        },
    }
}

impl NativeCommands {
    /// `npm cache clean --force`, started in the cache root's handle with `--cache` naming
    /// its own working directory, so no swapped ancestor can rebind it. Done when
    /// `_cacache` is gone from that handle and the `_cacache` held open since the check has
    /// no name left.
    fn clean_npm(
        &self,
        op: &NativeOp,
        cache: &Anchor,
        launch: &Launch<'_>,
    ) -> Result<NativeExit, ApplyError> {
        let tool = &op.spec().tool;
        let root = CheckedDir::of(cache)?;
        startable(&root)?;
        let name = last_name(op.target().anchor())?;
        let cacache = root.dir().open_dir(&name).map_err(unreached)?;
        is_reviewed(cacache.seen(), EntryKind::Dir, reviewed_top(op)?)?;
        self.npm_boundary()?;
        let ran = launch.run(tool, &root, &self.env)?;
        let postcondition = removed(root.dir(), &name, &cacache);
        Ok(exit(ran, postcondition, root.dir().sync()))
    }

    /// Makes npm's configuration boundary ready: `<data>/native/npm`, reached from the
    /// data directory without following a link, holding its two directories empty and its
    /// two config files empty. Anything else there, or a link on the way, is a failed
    /// start: no file a person edits chooses what the command does (pinned 33).
    fn npm_boundary(&self) -> Result<(), ApplyError> {
        let data = self
            .data
            .as_deref()
            .ok_or_else(|| ApplyError::new(ErrorCode::Unsupported))?;
        let failed = |error| ApplyError::io(ErrorCode::Io, error);
        os::create_private_dir(data).map_err(failed)?;
        let mut boundary = Folder::open(data).map_err(failed)?;
        for dir in NPM_BOUNDARY {
            boundary = made(&boundary, dir).map_err(failed)?;
        }
        for dir in NPM_DIRS {
            let names = made(&boundary, dir).and_then(|made| made.names());
            if !names.map_err(failed)?.is_empty() {
                return Err(failed(io::ErrorKind::DirectoryNotEmpty.into()));
            }
        }
        for file in NPM_FILES {
            empty_file(&boundary, file).map_err(failed)?;
        }
        Ok(())
    }

    /// `git worktree remove .`, started in the checkout's handle once the checkout, its
    /// `.git`, the repository's common directory, the admin directory and its `gitdir`
    /// are the files the witness names. Done when the checkout's name is gone from its
    /// parent and its own handle has no name left, which no rename of an ancestor can fake.
    fn remove_worktree(
        &self,
        op: &NativeOp,
        checkout: &Anchor,
        witness: &WorktreeWitness,
        launch: &Launch<'_>,
    ) -> Result<NativeExit, ApplyError> {
        let tool = &op.spec().tool;
        let name = last_name(checkout)?;
        let reached = executor::reach(checkout)?;
        let last = checkout
            .relative
            .components()
            .last()
            .ok_or_else(|| ApplyError::new(ErrorCode::TargetChanged))?;
        let dir = CheckedDir::below(&reached, last)?;
        startable(&dir)?;
        is_reviewed(dir.dir().seen(), EntryKind::Dir, reviewed_top(op)?)?;
        is_reviewed(
            dir.dir().stat(&os_name(".git")?),
            EntryKind::File,
            witness.gitfile,
        )?;
        let held = Registered::open(op, witness)?;
        let ran = launch.run(tool, &dir, &self.env)?;
        let parent = reached.holder();
        let postcondition = removed(parent, &name, dir.dir());
        let synced = parent.sync().and_then(|()| held.sync());
        Ok(exit(ran, postcondition, synced))
    }
}

/// The repository's side of a worktree, held open across the run: `<common>`, its
/// `worktrees` directory, and the admin directory checked against the witness.
#[derive(Debug)]
struct Registered {
    common: sys::Dir,
    worktrees: sys::Dir,
}

impl Registered {
    /// Opens the admin path `op` was admitted with, whose grandparent is `<common>`. That
    /// may sit behind a link, so it is opened by path and proved by identity; below it
    /// nothing is followed (pinned 29).
    fn open(op: &NativeOp, witness: &WorktreeWitness) -> Result<Self, ApplyError> {
        let changed = || ApplyError::new(ErrorCode::TargetChanged);
        let admin = op.admin().ok_or_else(changed)?;
        let admin_name = admin.file_name().ok_or_else(changed)?;
        let worktrees_path = admin.parent().ok_or_else(changed)?;
        if worktrees_path.file_name() != Some(OsStr::new("worktrees")) {
            return Err(changed());
        }
        let common_path = worktrees_path.parent().ok_or_else(changed)?;
        let common = sys::Dir::open_followed(common_path)
            .map_err(|error| ApplyError::io(ErrorCode::TargetChanged, error))?;
        is_reviewed(common.seen(), EntryKind::Dir, witness.common)?;
        let worktrees = common
            .open_dir(&os_name("worktrees")?)
            .map_err(|error| ApplyError::io(ErrorCode::TargetChanged, error))?;
        let id = sys::name(admin_name).map_err(|error| ApplyError::io(ErrorCode::Io, error))?;
        let registration = worktrees
            .open_dir(&id)
            .map_err(|error| ApplyError::io(ErrorCode::TargetChanged, error))?;
        is_reviewed(registration.seen(), EntryKind::Dir, witness.admin)?;
        is_reviewed(
            registration.stat(&os_name("gitdir")?),
            EntryKind::File,
            witness.gitdir_file,
        )?;
        Ok(Self { common, worktrees })
    }

    /// Makes the registration's removal durable: `worktrees`, unless git removed it with
    /// its last entry, then always `<common>`, which lost that entry (pinned 30).
    fn sync(&self) -> io::Result<()> {
        if self.worktrees.seen()?.snapshot.links != 0 {
            self.worktrees.sync()?;
        }
        self.common.sync()
    }
}

/// Makes sure `name` in `folder` is an empty file, made for its owner when it is
/// missing; a link, or a file that holds anything, fails.
fn empty_file(folder: &Folder, name: &str) -> io::Result<()> {
    match folder.read(name) {
        Ok(opened) => {
            let meta = opened.metadata()?;
            if meta.is_file() && meta.len() == 0 {
                Ok(())
            } else {
                Err(io::ErrorKind::AlreadyExists.into())
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => folder.create(name).map(drop),
        Err(error) => Err(error),
    }
}

/// The directory `name` in `folder`, made for its owner when it is missing; never one
/// reached through a link.
fn made(folder: &Folder, name: &str) -> io::Result<Folder> {
    match folder.open_in(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            folder.create_dir(name)?;
            folder.open_in(name)
        }
        opened => opened,
    }
}

#[cfg(test)]
mod tests;
