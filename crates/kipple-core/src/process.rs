//! Running-executable identity (`docs/04-safety-model.md` §4, D-035).

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

/// One process in a snapshot.
#[derive(Debug, Clone)]
pub struct ProcessRecord {
    /// Its process ID.
    pub pid: u32,
    /// What probing it found.
    pub outcome: ProcessOutcome,
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

/// Every process the OS listed, each with its outcome.
#[derive(Debug, Clone)]
pub struct ProcessSnapshot {
    /// The processes, in the order the OS listed them.
    pub processes: Vec<ProcessRecord>,
    /// Whether that listing is complete.
    pub inventory: Inventory,
}

/// Takes a snapshot of running executables.
pub trait ProcessProbe: fmt::Debug + Send + Sync {
    /// Lists every visible process and probes each one.
    ///
    /// # Errors
    /// When the processes can't be listed at all.
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError>;
}
