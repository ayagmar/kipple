//! Windows: the run store's directories opened as themselves and checked to be plain
//! directories, never a link or junction; files opened by path as themselves
//! (`FILE_FLAG_OPEN_REPARSE_POINT`), written through (`FILE_FLAG_WRITE_THROUGH`), and the
//! plan renamed with `MOVEFILE_WRITE_THROUGH`. Windows has no directory sync: NTFS's
//! metadata log, which a write-through forces to disk, stands for one (D-065).

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::ptr;

use kipple_core::FileSystemClass;
use windows_sys::Win32::Foundation::ERROR_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_WRITE_THROUGH, GetDiskFreeSpaceExW,
    MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

use crate::executor::sys::{self, Dir};

/// An open directory of the store, and its path, which its files are opened by.
#[derive(Debug)]
pub(super) struct Folder {
    dir: Dir,
    path: PathBuf,
}

/// Whether opening failed because the name is a link or junction, or not a directory.
pub(super) fn is_link(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_DIRECTORY.cast_signed())
}

/// `dir`, if it is a plain directory: not a link, junction or mount point.
fn plain(dir: Dir) -> io::Result<Dir> {
    if dir.is_plain()? {
        Ok(dir)
    } else {
        Err(io::Error::from_raw_os_error(ERROR_DIRECTORY.cast_signed()))
    }
}

/// Options that open a file as itself, never through a link, with `flags`.
fn options(flags: u32) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | flags);
    options
}

/// `path` with a final NUL, as the wide-string calls take it.
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

impl Folder {
    /// The directory at `path`, opened as itself.
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            dir: plain(Dir::open_root(path)?)?,
            path: path.to_owned(),
        })
    }

    /// The directory `name` in this one, opened as itself relative to its handle.
    pub(super) fn open_in(&self, name: &str) -> io::Result<Self> {
        let dir = self.dir.open_dir(&sys::name(OsStr::new(name))?)?;
        Ok(Self {
            dir: plain(dir)?,
            path: self.path.join(name),
        })
    }

    /// Creates the directory `name` here. It takes this directory's access rules.
    pub(super) fn create_dir(&self, name: &str) -> io::Result<()> {
        fs::create_dir(self.path.join(name))
    }

    /// A new file `name` here, for writing, written through.
    pub(super) fn create(&self, name: &str) -> io::Result<File> {
        options(FILE_FLAG_WRITE_THROUGH)
            .write(true)
            .create_new(true)
            .open(self.path.join(name))
    }

    /// The file `name` here, for reading.
    pub(super) fn read(&self, name: &str) -> io::Result<File> {
        options(0).read(true).open(self.path.join(name))
    }

    /// The file `name` here, for writing, written through.
    pub(super) fn write(&self, name: &str) -> io::Result<File> {
        options(FILE_FLAG_WRITE_THROUGH)
            .write(true)
            .open(self.path.join(name))
    }

    /// The lock file `name` here, created if it is missing.
    pub(super) fn lock_file(&self, name: &str) -> io::Result<File> {
        options(0)
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.join(name))
    }

    /// Renames `from` here to `to`, written through, never replacing anything.
    #[expect(unsafe_code, reason = "windows-sys exposes MoveFileExW as a raw call")]
    pub(super) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let (from, to) = (wide(&self.path.join(from)), wide(&self.path.join(to)));
        // SAFETY: both names are NUL-terminated wide strings that outlive the call.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Nothing: see the module.
    #[expect(
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "the Unix version can fail, and callers share one signature"
    )]
    pub(super) const fn sync(&self) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn file_system(&self) -> io::Result<FileSystemClass> {
        self.dir.file_system()
    }

    pub(super) fn names(&self) -> io::Result<Vec<OsString>> {
        self.dir.names()
    }

    /// The bytes free to this user on its volume.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes GetDiskFreeSpaceExW as a raw call"
    )]
    pub(super) fn free(&self) -> io::Result<u64> {
        let path = wide(&self.path);
        let mut free = 0_u64;
        // SAFETY: the path is a NUL-terminated wide string; the totals not asked for are
        // null, which the call allows.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                path.as_ptr(),
                &raw mut free,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(free)
    }
}
