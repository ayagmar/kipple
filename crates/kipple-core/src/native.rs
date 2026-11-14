//! The closed set of native operations (`docs/04-safety-model.md` §6).
//!
//! A native operation is a tool's own command, built here and nowhere else: no integration
//! or rule constructs one, and [`NativeCommand::argv`] is the one source for what runs and
//! what a person reviews. A plan's digest covers the command, which decides its arguments.
//! Only the platform's [`NativeRunner`] starts one, and only [`NativeOp`](crate::NativeOp)
//! reaches it.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::authorize::NativeOp;
use crate::cancel::CancelToken;
use crate::capability::NativeTool;
use crate::frozen::Fingerprint;
use crate::outcome::{ApplyError, ErrorCode};
use crate::plan::Anchor;
use crate::probe::FileIdentity;
use crate::store_lock::{HeldLocks, LockGap, StoreLocks};
use crate::tracked::GitGap;
use crate::worktree::{Registration, WorktreeQuery, WorktreeState, WorktreeWitness};

named! {
    /// A first-party native operation: a tool's own command, reviewed as a semantic plan.
    NativeKind {
        /// `git worktree remove`.
        GitWorktreeRemove = "git-worktree-remove",
        /// `npm cache clean`.
        NpmCacheClean = "native.npm-cache-clean",
    }
}

named! {
    /// Why a native tool can't be used.
    ToolGap {
        /// It wasn't found.
        Missing = "tool-missing",
        /// It was found, and can't be read or identified.
        Unreadable = "tool-unreadable",
        /// Its version couldn't be read.
        UnknownVersion = "tool-unknown-version",
        /// Its version isn't one kipple verified.
        UnverifiedVersion = "tool-unverified-version",
        /// It isn't the file, or the version, a person reviewed.
        Changed = "tool-changed",
        /// It is a script that names its interpreter (`#!`), such as a wrapper or a
        /// version manager's shim: what runs is another program, so kipple never starts it.
        Script = "tool-script",
        /// It is laid out in a way kipple doesn't know, such as npm packaged outside
        /// `lib/node_modules/npm`, so what it would run can't be told.
        UnknownLayout = "tool-unknown-layout",
        /// The runtime it runs with isn't there: npm with no `node` in its own install.
        NoRuntime = "tool-no-runtime",
        /// Its runtime is a version the tool itself doesn't support: a node outside npm's
        /// `engines`.
        UnsupportedRuntime = "tool-unsupported-runtime",
    }
}

/// A file a native tool runs from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFile {
    /// Where it is.
    pub path: PathBuf,
    /// Its identity, which a run compares before it starts.
    pub identity: FileIdentity,
    /// What changes when it is rewritten in place, or another file takes its inode, which
    /// a run compares beside its identity.
    pub fingerprint: Fingerprint,
}

/// A native tool as found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTool {
    /// Which tool.
    pub tool: NativeTool,
    /// Its executable.
    pub path: PathBuf,
    /// Its executable's identity.
    pub identity: FileIdentity,
    /// What changes when its executable is rewritten in place, or another file takes its
    /// inode.
    pub fingerprint: Fingerprint,
    /// Its version, as it reported it.
    pub version: String,
    /// What runs it, when it is a script.
    pub runtime: Option<ToolFile>,
}

/// What an integration read about the target of a semantic operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticFacts {
    /// The operation it is for.
    pub kind: NativeKind,
    /// The tool that would run it, or why there is none.
    pub tool: Result<ResolvedTool, ToolGap>,
}

/// One argument of a native command. The program isn't one: it comes from the
/// [`ResolvedTool`] the plan recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeArg {
    /// This text, exactly.
    Fixed(&'static str),
    /// This key, then a path below kipple's own data directory, which only the platform
    /// knows. What is there is kipple's, so review shows the key alone.
    Data(&'static str, &'static str),
    /// This key, then the OS's null device, which only the platform names. Review shows the
    /// key and "(none)".
    NullDevice(&'static str),
}

/// What keeps git from running a hook or a monitor, taking an optional lock, starting a
/// pager or correcting a command it doesn't know into an alias a repository chose.
///
/// Both the check and the removal start from this list, so neither can drift from the
/// other. The environment half stays in the platform.
pub const GIT_ISOLATION: &[NativeArg] = &[
    NativeArg::Fixed("--no-pager"),
    NativeArg::Fixed("-c"),
    NativeArg::Fixed("core.fsmonitor=false"),
    NativeArg::Fixed("-c"),
    NativeArg::NullDevice("core.hooksPath="),
    NativeArg::Fixed("-c"),
    NativeArg::Fixed("help.autocorrect=0"),
    NativeArg::Fixed("--no-optional-locks"),
];

/// What follows [`GIT_ISOLATION`] in a worktree's removal.
///
/// No `-C`: it would make git go back through the path, so `.` would be whatever the path
/// names by then. No `--force`, ever, and no branch.
const GIT_WORKTREE_REMOVE: &[NativeArg] = &[
    NativeArg::Fixed("-c"),
    NativeArg::Fixed("status.showUntrackedFiles=all"),
    NativeArg::Fixed("worktree"),
    NativeArg::Fixed("remove"),
    NativeArg::Fixed("."),
];

/// npm's arguments.
///
/// The runner starts it in the cache root's handle, so `--cache` can't be rebound by a
/// swapped ancestor, and every flag below outranks every npmrc, so no file a person edits
/// chooses where the command writes or rotates. Never `--dry-run`, which deletes anyway.
const NPM_CACHE_CLEAN: &[NativeArg] = &[
    NativeArg::Fixed("cache"),
    NativeArg::Fixed("clean"),
    NativeArg::Fixed("--force"),
    NativeArg::Fixed("--cache=/proc/self/cwd"),
    NativeArg::Data("--prefix=", "native/npm/prefix"),
    NativeArg::Data("--userconfig=", "native/npm/userconfig"),
    NativeArg::Data("--globalconfig=", "native/npm/globalconfig"),
    NativeArg::Data("--logs-dir=", "native/npm/logs"),
    NativeArg::Fixed("--logs-max=0"),
    NativeArg::Fixed("--no-update-notifier"),
    NativeArg::Fixed("--loglevel=error"),
];

/// A native command, closed: these two, with nothing a rule or a config can add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeCommand {
    /// `npm cache clean --force` on the cache root at `cache`.
    NpmCacheClean {
        /// The cache root, whose handle the command starts in.
        cache: Anchor,
    },
    /// `git worktree remove .` from the checkout at `checkout`.
    GitWorktreeRemove {
        /// The checkout, whose handle the command starts in.
        checkout: Anchor,
        /// What ties it to its repository, compared with one built fresh at apply.
        witness: WorktreeWitness,
    },
}

impl NativeCommand {
    /// Which operation it is.
    #[must_use]
    pub const fn kind(&self) -> NativeKind {
        match self {
            Self::NpmCacheClean { .. } => NativeKind::NpmCacheClean,
            Self::GitWorktreeRemove { .. } => NativeKind::GitWorktreeRemove,
        }
    }

    /// Its arguments: what runs and what a person reviews. They follow from the command
    /// alone, so a plan's digest covers them by covering the command.
    #[must_use]
    pub fn argv(&self) -> Vec<NativeArg> {
        match self {
            Self::NpmCacheClean { .. } => NPM_CACHE_CLEAN.to_vec(),
            Self::GitWorktreeRemove { .. } => [GIT_ISOLATION, GIT_WORKTREE_REMOVE].concat(),
        }
    }
}

/// A native item's command as a person reviews it: what runs, with which tool, on what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSpec {
    /// The command.
    pub command: NativeCommand,
    /// The tool that runs it, as found when the item was planned.
    pub tool: ResolvedTool,
    /// What it changes: the item's target.
    pub scope: Anchor,
}

/// How a native command's process ended, as the OS said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeEnd {
    /// Its exit code, when it exited.
    pub exit: Option<i32>,
    /// The signal that ended it, where the OS has signals.
    pub signal: Option<i32>,
}

/// How a native command's run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeStatus {
    /// It ended by itself.
    Ended(NativeEnd),
    /// It missed its deadline, and kipple killed its own child (D-038).
    TimedOut(NativeEnd),
    /// It started, and then kipple couldn't wait for it or read what it printed, so it
    /// killed its own child: `stopped` is how that child ended, or `None` when even that
    /// failed and it may still run.
    Lost {
        /// How the child ended once kipple stopped it.
        stopped: Option<NativeEnd>,
    },
}

named! {
    /// Whether what a native command was to do is done, by what the platform can see.
    Postcondition {
        /// It is.
        Met = "met",
        /// It isn't, or it can't be told.
        Unmet = "unmet",
    }
}

/// What a native command printed, up to the runner's bound on each stream. Kept in memory
/// only and shown once the command ends: never stored, never in JSON (04 §8a).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeOutput {
    /// Its standard output.
    pub stdout: Printed,
    /// Its standard error.
    pub stderr: Printed,
}

/// One stream a native command printed on, as the runner kept it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Printed {
    /// The bytes kept.
    pub bytes: Vec<u8>,
    /// Whether they are all it printed.
    pub kept: Kept,
}

/// How much of a stream the runner kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Kept {
    /// All of it.
    #[default]
    All,
    /// Only its start: it went past the runner's bound, or the rest couldn't be read.
    Start,
}

impl Printed {
    /// `bytes`, all a stream printed.
    #[must_use]
    pub const fn all(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            kept: Kept::All,
        }
    }
}

/// What the runner saw of a native command it started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeExit {
    /// How it ended.
    pub status: NativeStatus,
    /// Whether what it was to do is done.
    pub postcondition: Postcondition,
    /// Whether its effect is durable: nothing is done without this barrier.
    pub synced: Result<(), ApplyError>,
    /// What it printed.
    pub output: NativeOutput,
}

impl NativeStatus {
    /// How the process ended, whether or not it missed its deadline: neither code nor
    /// signal when it was lost and couldn't be stopped.
    #[must_use]
    pub const fn end(self) -> NativeEnd {
        match self {
            Self::Ended(end) | Self::TimedOut(end) | Self::Lost { stopped: Some(end) } => end,
            Self::Lost { stopped: None } => NativeEnd {
                exit: None,
                signal: None,
            },
        }
    }
}

impl NativeEnd {
    /// Whether it exited by itself with code 0.
    #[must_use]
    pub const fn succeeded(self) -> bool {
        matches!(
            self,
            Self {
                exit: Some(0),
                signal: None
            }
        )
    }
}

/// Finds a native tool. Read-only and never cached, so apply sees the present.
pub trait ToolResolver: fmt::Debug + Send + Sync {
    /// `tool` as this machine has it now.
    ///
    /// # Errors
    /// When it can't be used, and why.
    fn resolve(&self, tool: NativeTool) -> Result<ResolvedTool, ToolGap>;
}

/// Runs an authorized native command. Only the platform implements it, and nothing else
/// starts one.
pub trait NativeRunner: fmt::Debug + Send + Sync {
    /// Starts `op`, waits for it and makes its effect durable. Once the command started
    /// this returns how it ended, [`NativeStatus::Lost`] included.
    ///
    /// # Errors
    /// Only when it didn't start: nothing ran.
    fn run(&self, op: &NativeOp) -> Result<NativeExit, ApplyError>;
}

/// A machine with none of the native ports.
///
/// No tool is found, no worktree can be asked about, no lock held and no command started.
/// What a host that lacks one is given, and where a test doesn't reach them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unavailable;

impl ToolResolver for Unavailable {
    fn resolve(&self, _: NativeTool) -> Result<ResolvedTool, ToolGap> {
        Err(ToolGap::Missing)
    }
}

impl NativeRunner for Unavailable {
    fn run(&self, _: &NativeOp) -> Result<NativeExit, ApplyError> {
        Err(ApplyError::new(ErrorCode::Unsupported))
    }
}

impl StoreLocks for Unavailable {
    fn try_hold(&self, _: &[PathBuf]) -> Result<HeldLocks, LockGap> {
        Err(LockGap::Unsupported)
    }
}

impl WorktreeQuery for Unavailable {
    fn list(&self, _: &Path, _: &CancelToken) -> Result<Vec<Registration>, GitGap> {
        Err(GitGap::Unsupported)
    }

    fn inspect(&self, _: &Registration, _: &CancelToken) -> WorktreeState {
        WorktreeState::unknown(GitGap::Unsupported)
    }
}
