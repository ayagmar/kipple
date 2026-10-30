//! Running-executable identity (`docs/04-safety-model.md` §4, D-035).

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::probe::{FileIdentity, ProbeError};

/// What probing one process found. Every process gets exactly one outcome.
#[derive(Debug, Clone)]
pub enum ProcessOutcome {
    /// The path and identity of the file the process is running.
    Identified {
        /// The executable's path as the OS reports it.
        path: PathBuf,
        /// The identity of the file actually running, which survives the path being
        /// deleted or replaced.
        identity: FileIdentity,
        /// The running file has no links left: it was deleted or replaced.
        deleted: bool,
    },
    /// A path, but no identity that is known to belong to the running file.
    PathOnly {
        /// The executable's path as the OS reports it.
        path: PathBuf,
        /// Why the identity could not be confirmed.
        error: Arc<io::Error>,
    },
    /// The OS refused to say, typically because the process belongs to another user.
    Denied(Arc<io::Error>),
    /// The process has exited but is still listed (a zombie, or a handle keeps it).
    Exited,
    /// The process went away between listing and probing, with positive evidence.
    Gone,
    /// A process with no executable file, such as a kernel thread.
    NoExecutable,
    /// Any other failure.
    Other(Arc<io::Error>),
}

/// What an outcome means when deciding whether something is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessClass {
    /// Running, with a known executable identity.
    Identified,
    /// Might be running anything. Never read as idle.
    Unknown,
    /// Not running anymore.
    NotRunning,
    /// Running, but no executable file can be involved.
    NoExecutable,
}

impl ProcessOutcome {
    /// The class the safety model uses: only exited and gone mean "not running".
    #[must_use]
    pub const fn class(&self) -> ProcessClass {
        match self {
            Self::Identified { .. } => ProcessClass::Identified,
            Self::PathOnly { .. } | Self::Denied(_) | Self::Other(_) => ProcessClass::Unknown,
            Self::Exited | Self::Gone => ProcessClass::NotRunning,
            Self::NoExecutable => ProcessClass::NoExecutable,
        }
    }
}

/// A user account as the OS names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserId {
    /// A Unix user ID.
    Uid(u32),
    /// A Windows security identifier, in its `S-1-5-…` string form.
    Sid(String),
}

/// Who a process runs as. Only a consistent, fully read identity is named, or root's
/// rights taken by the current user; anything else is [`ProcessOwner::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessOwner {
    /// The user kipple runs as.
    CurrentUser,
    /// Unix root: every user ID of the process is 0.
    Root,
    /// Windows `SYSTEM` (S-1-5-18).
    System,
    /// Some other single account.
    OtherUser(UserId),
    /// Root's rights taken by the current user: its effective user ID is 0, and every
    /// other one is 0 or the current user's, as with a setuid helper such as
    /// `fusermount3`.
    Elevated,
    /// Unreadable, or any other mix of IDs.
    Unknown,
}

/// Why a fact about a process is missing. Either way it is unknown, never empty.
#[derive(Debug, Clone)]
pub enum Unread {
    /// kipple doesn't read this fact on this OS.
    Unsupported,
    /// The OS refused or the read failed.
    Failed(Arc<io::Error>),
}

/// A fact about a process, or why it is unknown.
pub type Probed<T> = Result<T, Unread>;

/// One absolute path found in a process's environment, such as a `PATH` entry or the
/// value of `VIRTUAL_ENV`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvPath {
    /// The variable it came from.
    pub var: OsString,
    /// The path, as the variable holds it.
    pub path: PathBuf,
}

/// A Linux mount namespace, by the inode number `/proc/<pid>/ns/mnt` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MountNamespace(pub u64);

/// A Linux capability set, as the bit mask `/proc/<pid>/status` shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities(pub u64);

/// One process in a snapshot.
#[derive(Debug, Clone)]
pub struct ProcessRecord {
    /// Its process ID.
    pub pid: u32,
    /// What probing its executable found.
    pub outcome: ProcessOutcome,
    /// Who it runs as.
    pub owner: ProcessOwner,
    /// Its working directory.
    pub cwd: Probed<PathBuf>,
    /// The arguments that can name a file, in order: the name it was started by, then
    /// each argument with a path separator. Others are dropped, since a command line
    /// often holds secrets (docs/04 §8a).
    pub argv: Probed<Vec<OsString>>,
    /// Every absolute path its environment variables hold, each list split at the OS's
    /// path separator. Other values are dropped, like other arguments.
    pub env_paths: Probed<Vec<EnvPath>>,
    /// The files mapped into its memory, such as shared libraries, once each.
    pub mapped_files: Probed<Vec<PathBuf>>,
    /// The mount namespace it sees files through (Linux).
    pub mount_namespace: Probed<MountNamespace>,
    /// The identity of its root directory, which differs from `/` in a container or a
    /// chroot (Linux).
    pub root_dir: Probed<FileIdentity>,
    /// Its root directory as kipple names it: where `/proc/<pid>/root` leads (Linux). That
    /// name is kipple's own only for a process in kipple's mount namespace.
    pub root_path: Probed<PathBuf>,
    /// A path through which kipple reaches the files the process names, whatever its mount
    /// namespace: `/proc/<pid>/root` (Linux).
    pub root_entry: Probed<PathBuf>,
    /// Its effective capabilities (Linux).
    pub capabilities: Probed<Capabilities>,
}

impl ProcessRecord {
    /// A process with only its executable probed: every other fact is unknown.
    #[must_use]
    pub const fn new(pid: u32, outcome: ProcessOutcome) -> Self {
        Self {
            pid,
            outcome,
            owner: ProcessOwner::Unknown,
            cwd: Err(Unread::Unsupported),
            argv: Err(Unread::Unsupported),
            env_paths: Err(Unread::Unsupported),
            mapped_files: Err(Unread::Unsupported),
            mount_namespace: Err(Unread::Unsupported),
            root_dir: Err(Unread::Unsupported),
            root_path: Err(Unread::Unsupported),
            root_entry: Err(Unread::Unsupported),
            capabilities: Err(Unread::Unsupported),
        }
    }
}

/// Whether the listing itself saw every process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inventory {
    /// Every process on the system was listed.
    Complete,
    /// Some processes may be missing from the listing, without any error per process.
    Partial {
        /// Why, for `doctor`.
        reason: String,
    },
}

/// Who the OS shows a process's command line to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandLines {
    /// Every user, as Linux does. Then a root process whose command line couldn't be
    /// read is not one kipple may leave out.
    Public,
    /// Only its own user and the administrator.
    Private,
}

/// How kipple itself sees files, which the paths a process shows are compared with.
#[derive(Debug, Clone)]
pub struct Observer {
    /// kipple's own mount namespace (Linux). A process in another one, such as a
    /// container, names files by other paths.
    pub mount_namespace: Probed<MountNamespace>,
    /// The identity of kipple's own root directory (Linux). A process with another one,
    /// in a chroot, names files by other paths too.
    pub root_dir: Probed<FileIdentity>,
    /// Who the OS shows a process's command line to.
    pub command_lines: CommandLines,
    /// Whether kipple reads each process's working directory, arguments, environment and
    /// mapped files at all. Windows doesn't in M2, so no process there can show an
    /// interpreted layout in use.
    pub process_paths: bool,
}

impl Observer {
    /// An OS without mount namespaces or chroots to compare, that keeps other users'
    /// command lines to themselves: macOS, and a host faked in a test.
    pub const PLAIN: Self = Self {
        mount_namespace: Err(Unread::Unsupported),
        root_dir: Err(Unread::Unsupported),
        command_lines: CommandLines::Private,
        process_paths: true,
    };
}

/// Every process the OS listed, each with its outcome.
#[derive(Debug, Clone)]
pub struct ProcessSnapshot {
    /// The processes, in the order the OS listed them.
    pub processes: Vec<ProcessRecord>,
    /// Whether that listing is complete.
    pub inventory: Inventory,
    /// How kipple sees files, to compare each process's view with.
    pub observer: Observer,
}

/// Takes a snapshot of running executables.
pub trait ProcessProbe: fmt::Debug + Send + Sync {
    /// Lists every visible process and probes each one.
    ///
    /// # Errors
    /// When the processes can't be listed at all.
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError>;
}
