//! Read-only filesystem ports, implemented by `kipple-platform` and faked in tests.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cancel::CancelToken;
use crate::size::SpaceEstimate;

/// A file's identity: device and inode on Unix, volume serial and file index on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    /// Device or volume.
    pub volume: u64,
    /// Inode or file index.
    pub file: u64,
}

/// What an entry is, without following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A directory.
    Dir,
    /// A regular file.
    File,
    /// A symbolic link, or on Windows any name-surrogate reparse point (junctions too).
    Symlink,
    /// Anything else: sockets, devices, FIFOs.
    Other,
}

/// What a no-follow probe learned about one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryMeta {
    /// The entry itself, never a link's target.
    pub kind: EntryKind,
    /// Its identity, where the platform reports one.
    pub identity: Option<FileIdentity>,
}

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntryMeta {
    /// The entry's name inside the directory.
    pub name: OsString,
    /// The entry itself, never a link's target.
    pub kind: EntryKind,
}

/// A failed read-only probe. The upstream OS error is kept.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProbeError {
    /// The operating system refused or failed the call.
    #[error("{}: {source}", path.display())]
    Io {
        /// What was probed.
        path: PathBuf,
        /// The upstream error.
        source: Arc<io::Error>,
    },
    /// The path is not inside a root the caller may read.
    #[error("{} is outside the roots this integration may read", path.display())]
    OutsideRoots {
        /// What was asked for.
        path: PathBuf,
    },
}

impl ProbeError {
    /// Wraps an OS error for `path`.
    #[must_use]
    pub fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_owned(),
            source: Arc::new(source),
        }
    }

    /// The upstream error kind, if the OS reported one.
    #[must_use]
    pub fn io_kind(&self) -> Option<io::ErrorKind> {
        match self {
            Self::Io { source, .. } => Some(source.kind()),
            Self::OutsideRoots { .. } => None,
        }
    }
}

/// Read-only filesystem probes. None of them follows a symbolic link or junction.
pub trait FsProbe: fmt::Debug + Send + Sync {
    /// What `path` itself is.
    ///
    /// # Errors
    /// When the entry can't be read.
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError>;

    /// The entries of the directory at `dir`.
    ///
    /// # Errors
    /// When the directory or any of its entries can't be read: a partial listing is
    /// never returned as if it were whole.
    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError>;
}

/// Measures what removing an item would free.
pub trait Sizer: fmt::Debug + Send + Sync {
    /// Sizes `path` and everything below it, without following links or leaving its file
    /// system. Stops at the next directory once `cancel` is set.
    ///
    /// # Errors
    /// When `path` itself can't be read. Unreadable entries below it make the estimate
    /// [`Incomplete`](crate::Completeness::Incomplete) instead.
    fn size(&self, path: &Path, cancel: &CancelToken) -> Result<SpaceEstimate, ProbeError>;
}
