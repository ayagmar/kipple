//! Linux and macOS: the run store's directories as open descriptors, every name opened
//! relative to them without following a link, and `fsync` of each (`F_FULLFSYNC` on
//! macOS).

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::path::Path;

use kipple_core::FileSystemClass;

use crate::executor::sys::{self, Dir, Name, no_replace};

/// An open directory of the store.
#[derive(Debug)]
pub(super) struct Folder(Dir);

/// Whether opening failed because the name is a link, or not what was expected.
pub(super) fn is_link(error: &io::Error) -> bool {
    sys::is_link_or_file(error)
}

/// The file system of the directory at `path`, reached through any link on the way.
pub(super) fn file_system_at(path: &Path) -> io::Result<FileSystemClass> {
    Dir::open_followed(path)?.file_system()
}

fn name(name: &str) -> io::Result<Name> {
    sys::name(OsStr::new(name))
}

impl Folder {
    /// The directory at `path`, never through a link in its last component.
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        Dir::open_root(path).map(Self)
    }

    /// The directory `name` in this one, never through a link.
    pub(super) fn open_in(&self, entry: &str) -> io::Result<Self> {
        self.0.open_dir(&name(entry)?).map(Self)
    }

    /// Creates the directory `name` here, for its owner only.
    pub(super) fn create_dir(&self, entry: &str) -> io::Result<()> {
        self.0.create_dir(&name(entry)?)
    }

    /// A new file `name` here, for writing, for its owner only.
    pub(super) fn create(&self, entry: &str) -> io::Result<File> {
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL;
        self.0.open_file(&name(entry)?, flags)
    }

    /// The file `name` here, for reading. A pipe doesn't block the open.
    pub(super) fn read(&self, entry: &str) -> io::Result<File> {
        self.0
            .open_file(&name(entry)?, libc::O_RDONLY | libc::O_NONBLOCK)
    }

    /// The file `name` here, for writing.
    pub(super) fn write(&self, entry: &str) -> io::Result<File> {
        self.0
            .open_file(&name(entry)?, libc::O_WRONLY | libc::O_NONBLOCK)
    }

    /// The lock file `name` here, created for its owner only if it is missing.
    pub(super) fn lock_file(&self, entry: &str) -> io::Result<File> {
        let flags = libc::O_RDWR | libc::O_CREAT | libc::O_NONBLOCK;
        self.0.open_file(&name(entry)?, flags)
    }

    /// Renames `from` here to `to`, never replacing anything.
    pub(super) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        no_replace(&self.0, &name(from)?, &self.0, &name(to)?)
    }

    pub(super) fn sync(&self) -> io::Result<()> {
        self.0.sync()
    }

    pub(super) fn file_system(&self) -> io::Result<FileSystemClass> {
        self.0.file_system()
    }

    pub(super) fn names(&self) -> io::Result<Vec<OsString>> {
        self.0.names()
    }

    /// The bytes free to this user on its file system.
    #[expect(unsafe_code, reason = "libc's fstatvfs is a raw call")]
    #[cfg_attr(
        target_os = "linux",
        expect(
            clippy::useless_conversion,
            reason = "the block count is 64-bit on Linux and 32-bit on macOS"
        )
    )]
    pub(super) fn free(&self) -> io::Result<u64> {
        let mut stat = MaybeUninit::<libc::statvfs>::zeroed();
        // SAFETY: the descriptor is open and `stat` is large enough.
        if unsafe { libc::fstatvfs(self.0.fd(), stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fstatvfs` succeeded, so it filled `stat`.
        let stat = unsafe { stat.assume_init() };
        Ok(u64::from(stat.f_bavail).saturating_mul(stat.f_frsize))
    }
}
