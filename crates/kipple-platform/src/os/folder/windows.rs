//! Windows: kipple's own folders opened as themselves and checked to be plain
//! directories, never a link or junction, and every name in them opened, renamed and
//! removed relative to the checked handle, as itself (`NtCreateFile` with
//! `FILE_OPEN_REPARSE_POINT`). Files are written through (`FILE_WRITE_THROUGH`), and each
//! directory that gained or lost a name is flushed as the executor flushes one: opened
//! again for writing, then `FlushFileBuffers` (D-065).

use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io;
use std::os::windows::fs::MetadataExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;
use std::ptr;

use kipple_core::FileSystemClass;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_OPEN, FILE_OPEN_IF, FILE_RENAME_INFORMATION, FILE_RENAME_POSIX_SEMANTICS,
    FILE_RENAME_REPLACE_IF_EXISTS, FILE_WRITE_THROUGH, FileFsFullSizeInformation,
    FileRenameInformationEx, NtQueryVolumeInformationFile, NtSetInformationFile,
};
use windows_sys::Wdk::System::SystemServices::FILE_FS_FULL_SIZE_INFORMATION;
use windows_sys::Win32::Foundation::{ERROR_DIRECTORY, NTSTATUS, RtlNtStatusToDosError};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO_EX, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES,
    FileDispositionInfoEx, SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use crate::executor::sys::{self, Dir, Name, no_replace};

/// An open folder of kipple's own.
#[derive(Debug)]
pub(crate) struct Folder(Dir);

/// Whether opening failed because the name is a link or junction, or not a directory.
pub(crate) fn is_link(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_DIRECTORY.cast_signed())
}

/// What opening a link, junction or other reparse point where kipple wants a plain
/// entry fails with.
fn not_plain() -> io::Error {
    io::Error::from_raw_os_error(ERROR_DIRECTORY.cast_signed())
}

/// `dir`, if it is a plain directory: not a link, junction or mount point.
fn plain(dir: Dir) -> io::Result<Folder> {
    if dir.is_plain()? {
        Ok(Folder(dir))
    } else {
        Err(not_plain())
    }
}

/// The file system of the directory at `path`, reached through any link on the way.
pub(crate) fn file_system_at(path: &Path) -> io::Result<FileSystemClass> {
    Dir::open_followed(path)?.file_system()
}

fn name(name: &str) -> io::Result<Name> {
    sys::name(OsStr::new(name))
}

impl Folder {
    /// The directory at `path`, opened as itself.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        plain(Dir::open_root(path)?)
    }

    /// The directory `name` in this one, opened as itself relative to its handle.
    pub(crate) fn open_in(&self, entry: &str) -> io::Result<Self> {
        plain(self.0.open_dir(&name(entry)?)?)
    }

    /// Creates the directory `name` here. It takes this directory's access rules.
    pub(crate) fn create_dir(&self, entry: &str) -> io::Result<()> {
        self.0.create_dir(&name(entry)?)
    }

    /// A new file `name` here, for writing, written through.
    pub(crate) fn create(&self, entry: &str) -> io::Result<File> {
        let access = FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES;
        self.0
            .open_file(&name(entry)?, access, FILE_CREATE, FILE_WRITE_THROUGH)
    }

    /// The file `name` here, for reading.
    pub(crate) fn read(&self, entry: &str) -> io::Result<File> {
        self.0
            .open_file(&name(entry)?, FILE_GENERIC_READ, FILE_OPEN, 0)
    }

    /// The file `name` here, for writing, written through.
    pub(crate) fn write(&self, entry: &str) -> io::Result<File> {
        let access = FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES;
        self.0
            .open_file(&name(entry)?, access, FILE_OPEN, FILE_WRITE_THROUGH)
    }

    /// The lock file `name` here, created if it is missing.
    pub(crate) fn lock_file(&self, entry: &str) -> io::Result<File> {
        let access = FILE_GENERIC_READ | FILE_GENERIC_WRITE;
        self.0.open_file(&name(entry)?, access, FILE_OPEN_IF, 0)
    }

    /// Renames `from` here to `to`, never replacing anything.
    pub(crate) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        no_replace(&self.0, &name(from)?, &self.0, &name(to)?)
    }

    /// Renames `from` here to `to`, replacing what `to` names in one step: a file, or a
    /// link, which is replaced and never followed. `from` is opened as itself relative to
    /// this folder's handle and renamed on that handle, relative to it again:
    /// `NtSetInformationFile(FileRenameInformationEx)` replacing with POSIX semantics, so
    /// a reader holding the old file open keeps it and doesn't stop the rename.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes NtSetInformationFile as a raw call"
    )]
    #[expect(clippy::disallowed_methods, reason = "kipple's own profile file")]
    pub(crate) fn rename_replace(&self, from: &str, to: &str) -> io::Result<()> {
        let into = name(to)?;
        let file = self.0.open_file(&name(from)?, DELETE, FILE_OPEN, 0)?;
        let length = size_of_val(into.as_slice());
        let bytes = size_of::<FILE_RENAME_INFORMATION>() + length;
        // `u64`s keep the buffer aligned for the struct.
        let mut buf = vec![0_u64; bytes.div_ceil(size_of::<u64>())];
        let rename = buf.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        // SAFETY: `buf` is aligned for the struct and holds it with room for `into` from
        // its `FileName` on.
        unsafe {
            (*rename).Anonymous.Flags = FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS;
            (*rename).RootDirectory = self.0.handle().as_raw_handle();
            (*rename).FileNameLength = u32::try_from(length).map_err(io::Error::other)?;
            let to_name = (&raw mut (*rename).FileName).cast::<u16>();
            ptr::copy_nonoverlapping(into.as_ptr(), to_name, into.len());
        }
        let mut status = IO_STATUS_BLOCK::default();
        // SAFETY: `file` is open with DELETE access, and `buf` holds the struct the class
        // names and its name, `bytes` long.
        let result = unsafe {
            NtSetInformationFile(
                file.as_raw_handle(),
                &raw mut status,
                buf.as_ptr().cast::<c_void>(),
                u32::try_from(bytes).map_err(io::Error::other)?,
                FileRenameInformationEx,
            )
        };
        nt(result)
    }

    /// Removes the file `name` here, on the handle that checked it: a link, junction or
    /// other reparse point is refused and stays, and so is a directory.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes SetFileInformationByHandle as a raw call"
    )]
    #[expect(clippy::disallowed_methods, reason = "kipple's own profile file")]
    pub(crate) fn remove(&self, entry: &str) -> io::Result<()> {
        let access = DELETE | FILE_READ_ATTRIBUTES;
        let file = self.0.open_file(&name(entry)?, access, FILE_OPEN, 0)?;
        if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(not_plain());
        }
        let info = FILE_DISPOSITION_INFO_EX {
            Flags: FILE_DISPOSITION_FLAG_DELETE
                | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
                | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
        };
        // SAFETY: `file` is open with DELETE access and `info` is the struct the class
        // names.
        let ok = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfoEx,
                (&raw const info).cast::<c_void>(),
                u32::try_from(size_of::<FILE_DISPOSITION_INFO_EX>()).unwrap_or(u32::MAX),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
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

    /// The bytes free to this user on its volume, read from its handle.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes NtQueryVolumeInformationFile as a raw call"
    )]
    pub(crate) fn free(&self) -> io::Result<u64> {
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
        nt(result)?;
        let units = u64::try_from(size.CallerAvailableAllocationUnits).unwrap_or(0);
        let unit = u64::from(size.SectorsPerAllocationUnit) * u64::from(size.BytesPerSector);
        Ok(units.saturating_mul(unit))
    }
}

/// An NTSTATUS as a result, its error as the Win32 error it stands for.
#[expect(
    unsafe_code,
    reason = "windows-sys exposes RtlNtStatusToDosError as a raw call"
)]
fn nt(status: NTSTATUS) -> io::Result<()> {
    if status >= 0 {
        return Ok(());
    }
    // SAFETY: a plain conversion.
    let code = unsafe { RtlNtStatusToDosError(status) };
    Err(io::Error::from_raw_os_error(code.cast_signed()))
}
