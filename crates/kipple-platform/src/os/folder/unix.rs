//! Linux and macOS: kipple's own folders as open descriptors, every name opened, renamed
//! and removed relative to them without following a link, and `fsync` of each
//! (`F_FULLFSYNC` on macOS).

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::path::Path;

use kipple_core::{EntryKind, FileSystemClass};

use crate::executor::sys::{self, Dir, Name, no_replace};

/// An open folder of kipple's own.
#[derive(Debug)]
pub(crate) struct Folder(Dir);

/// Whether opening failed because the name is a link, or not what was expected.
pub(crate) fn is_link(error: &io::Error) -> bool {
    sys::is_link_or_file(error)
}

/// The file system of the directory at `path`, reached through any link on the way.
pub(crate) fn file_system_at(path: &Path) -> io::Result<FileSystemClass> {
    Dir::open_followed(path)?.file_system()
}

fn name(name: &str) -> io::Result<Name> {
    sys::name(OsStr::new(name))
}

impl Folder {
    /// The directory at `path`, never through a link in its last component.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        Dir::open_root(path).map(Self)
    }

    /// The directory `name` in this one, never through a link.
    pub(crate) fn open_in(&self, entry: &str) -> io::Result<Self> {
        self.0.open_dir(&name(entry)?).map(Self)
    }

    /// Creates the directory `name` here, for its owner only.
    pub(crate) fn create_dir(&self, entry: &str) -> io::Result<()> {
        self.0.create_dir(&name(entry)?)
    }

    /// A new file `name` here, for writing, for its owner only.
    pub(crate) fn create(&self, entry: &str) -> io::Result<File> {
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL;
        self.0.open_file(&name(entry)?, flags)
    }

    /// The file `name` here, for reading. A pipe doesn't block the open.
    pub(crate) fn read(&self, entry: &str) -> io::Result<File> {
        self.0
            .open_file(&name(entry)?, libc::O_RDONLY | libc::O_NONBLOCK)
    }

    /// The file `name` here, for writing.
    pub(crate) fn write(&self, entry: &str) -> io::Result<File> {
        self.0
            .open_file(&name(entry)?, libc::O_WRONLY | libc::O_NONBLOCK)
    }

    /// The lock file `name` here, created for its owner only if it is missing.
    pub(crate) fn lock_file(&self, entry: &str) -> io::Result<File> {
        let flags = libc::O_RDWR | libc::O_CREAT | libc::O_NONBLOCK;
        self.0.open_file(&name(entry)?, flags)
    }

    /// Renames `from` here to `to`, never replacing anything.
    pub(crate) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        no_replace(&self.0, &name(from)?, &self.0, &name(to)?)
    }

    /// Renames `from` here to `to`, replacing what `to` names in one step: a file, or a
    /// link, which is replaced and never followed.
    #[expect(unsafe_code, reason = "libc's renameat is a raw call")]
    #[expect(clippy::disallowed_methods, reason = "kipple's own profile file")]
    pub(crate) fn rename_replace(&self, from: &str, to: &str) -> io::Result<()> {
        let (from, to) = (name(from)?, name(to)?);
        let fd = self.0.fd();
        // SAFETY: the descriptor is open and both names are NUL-terminated.
        if unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Removes the file `name` here. A link or anything else that isn't a regular file is
    /// refused and stays.
    pub(crate) fn remove(&self, entry: &str) -> io::Result<()> {
        let entry = name(entry)?;
        match self.0.stat(&entry)?.snapshot.kind {
            EntryKind::File => self.0.unlink(&entry, 0),
            EntryKind::Symlink => Err(io::Error::from_raw_os_error(libc::ELOOP)),
            EntryKind::Dir | EntryKind::Other => Err(super::not_regular()),
        }
    }

    pub(crate) fn sync(&self) -> io::Result<()> {
        self.0.sync()
    }

    pub(crate) fn file_system(&self) -> io::Result<FileSystemClass> {
        self.0.file_system()
    }

    pub(crate) fn names(&self) -> io::Result<Vec<OsString>> {
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
    pub(crate) fn free(&self) -> io::Result<u64> {
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
