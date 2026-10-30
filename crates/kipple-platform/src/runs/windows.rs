//! Windows: the run store's directories opened as themselves and checked to be plain
//! directories, never a link or junction, and every name in them opened relative to the
//! checked handle, as itself (`NtCreateFile` with `FILE_OPEN_REPARSE_POINT`). Files are
//! written through (`FILE_WRITE_THROUGH`), and each directory that gained a name is
//! flushed as the executor flushes one: opened again for writing, then `FlushFileBuffers`
//! (D-065).

use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;

use kipple_core::FileSystemClass;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_OPEN, FILE_OPEN_IF, FILE_WRITE_THROUGH, FileFsFullSizeInformation,
    NtQueryVolumeInformationFile,
};
use windows_sys::Wdk::System::SystemServices::FILE_FS_FULL_SIZE_INFORMATION;
use windows_sys::Win32::Foundation::{ERROR_DIRECTORY, RtlNtStatusToDosError};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use crate::executor::sys::{self, Dir, Name, no_replace};

/// An open directory of the store.
#[derive(Debug)]
pub(super) struct Folder(Dir);

/// Whether opening failed because the name is a link or junction, or not a directory.
pub(super) fn is_link(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_DIRECTORY.cast_signed())
}

/// `dir`, if it is a plain directory: not a link, junction or mount point.
fn plain(dir: Dir) -> io::Result<Folder> {
    if dir.is_plain()? {
        Ok(Folder(dir))
    } else {
        Err(io::Error::from_raw_os_error(ERROR_DIRECTORY.cast_signed()))
    }
}

fn name(name: &str) -> io::Result<Name> {
    sys::name(OsStr::new(name))
}

impl Folder {
    /// The directory at `path`, opened as itself.
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        plain(Dir::open_root(path)?)
    }

    /// The directory `name` in this one, opened as itself relative to its handle.
    pub(super) fn open_in(&self, entry: &str) -> io::Result<Self> {
        plain(self.0.open_dir(&name(entry)?)?)
    }

    /// Creates the directory `name` here. It takes this directory's access rules.
    pub(super) fn create_dir(&self, entry: &str) -> io::Result<()> {
        self.0.create_dir(&name(entry)?)
    }

    /// A new file `name` here, for writing, written through.
    pub(super) fn create(&self, entry: &str) -> io::Result<File> {
        let access = FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES;
        self.0
            .open_file(&name(entry)?, access, FILE_CREATE, FILE_WRITE_THROUGH)
    }

    /// The file `name` here, for reading.
    pub(super) fn read(&self, entry: &str) -> io::Result<File> {
        self.0
            .open_file(&name(entry)?, FILE_GENERIC_READ, FILE_OPEN, 0)
    }

    /// The file `name` here, for writing, written through.
    pub(super) fn write(&self, entry: &str) -> io::Result<File> {
        let access = FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES;
        self.0
            .open_file(&name(entry)?, access, FILE_OPEN, FILE_WRITE_THROUGH)
    }

    /// The lock file `name` here, created if it is missing.
    pub(super) fn lock_file(&self, entry: &str) -> io::Result<File> {
        let access = FILE_GENERIC_READ | FILE_GENERIC_WRITE;
        self.0.open_file(&name(entry)?, access, FILE_OPEN_IF, 0)
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

    /// The bytes free to this user on its volume, read from its handle.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes NtQueryVolumeInformationFile as a raw call"
    )]
    pub(super) fn free(&self) -> io::Result<u64> {
        let mut size = FILE_FS_FULL_SIZE_INFORMATION::default();
        let mut status = IO_STATUS_BLOCK::default();
        // SAFETY: the handle is open and `size` is the struct the class names, writable.
        let result = unsafe {
            NtQueryVolumeInformationFile(
                self.0.handle().as_raw_handle(),
                &raw mut status,
                (&raw mut size).cast::<c_void>(),
                u32::try_from(size_of::<FILE_FS_FULL_SIZE_INFORMATION>()).unwrap_or(u32::MAX),
                FileFsFullSizeInformation,
            )
        };
        if result < 0 {
            // SAFETY: a plain conversion.
            let code = unsafe { RtlNtStatusToDosError(result) };
            return Err(io::Error::from_raw_os_error(code.cast_signed()));
        }
        let units = u64::try_from(size.CallerAvailableAllocationUnits).unwrap_or(0);
        let unit = u64::from(size.SectorsPerAllocationUnit) * u64::from(size.BytesPerSector);
        Ok(units.saturating_mul(unit))
    }
}
