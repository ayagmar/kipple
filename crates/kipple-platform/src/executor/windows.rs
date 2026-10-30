//! Windows: every entry is opened by name relative to its directory's handle
//! (`NtCreateFile` with a `RootDirectory`) with `FILE_OPEN_REPARSE_POINT`, so a link or
//! junction is opened as itself, never followed. Identity is `FILE_ID_INFO` (64-bit
//! volume serial, 128-bit file ID), read on the open handle with its times, size and
//! link count. Directories are listed with `FILE_ID_EXTD_DIR_INFO`. An entry is removed
//! on the very handle that was checked, with `FileDispositionInfoEx`: POSIX semantics, so
//! the name goes at once even while others hold it open, and the read-only attribute
//! ignored rather than cleared first.

use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io;
use std::mem::{offset_of, size_of, size_of_val};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::ptr;

use kipple_core::{
    EntryKind, EntrySnapshot, FileIdentity, FileSystemClass, Fingerprint, LocalFileSystem, Stamp,
    Unsupported,
};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT, FileFsDeviceInformation, NtCreateFile,
    NtQueryVolumeInformationFile,
};
use windows_sys::Wdk::System::SystemServices::{FILE_FS_DEVICE_INFORMATION, FILE_REMOTE_DEVICE};
use windows_sys::Win32::Foundation::{
    ERROR_DIRECTORY, ERROR_LOCK_VIOLATION, ERROR_NO_MORE_FILES, ERROR_SHARING_VIOLATION,
    ERROR_USER_MAPPED_FILE, HANDLE, NTSTATUS, RtlNtStatusToDosError, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
    FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_EXTD_DIR_INFO, FILE_INFO_BY_HANDLE_CLASS,
    FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_TRAVERSE,
    FILE_WRITE_DATA, FileAttributeTagInfo, FileBasicInfo, FileDispositionInfoEx,
    FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo, FileRenameInfo, FileStandardInfo,
    FlushFileBuffers, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
    GetVolumeInformationByHandleW, MAXIMUM_REPARSE_DATA_BUFFER_SIZE, SYNCHRONIZE,
    SetFileInformationByHandle, VOLUME_NAME_NONE,
};
use windows_sys::Win32::System::IO::{DeviceIoControl, IO_STATUS_BLOCK};
use windows_sys::Win32::System::Ioctl::{FILE_DEVICE_NETWORK_FILE_SYSTEM, FSCTL_GET_REPARSE_POINT};
use windows_sys::Win32::System::SystemServices::IO_REPARSE_TAG_MOUNT_POINT;

use super::{Form, Seen};
use crate::os::handle_identity;

/// A name as the OS takes it: UTF-16 code units, exactly.
pub(crate) type Name = Vec<u16>;

/// `name` as the OS takes it. A name holding `:` would open a stream of another entry.
pub(crate) fn name(name: &OsStr) -> io::Result<Name> {
    let wide: Name = name.encode_wide().collect();
    if wide.contains(&u16::from(b':')) || wide.len() > usize::from(u16::MAX / 2) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(wide)
}

/// Others may read, write and delete what kipple has open: kipple never blocks them.
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// What a directory is opened for: reading its attributes, listing it and opening its
/// entries.
const DIR_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// What an entry about to be removed is opened for.
const REMOVE_ACCESS: u32 = DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// What a directory is opened for to flush it: NTFS and `ReFS` flush only a handle that
/// may write, which for a directory means adding entries to it.
const FLUSH_ACCESS: u32 = FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// The name-surrogate bit of a reparse tag: links, junctions and mount points.
const NAME_SURROGATE: u32 = 0x2000_0000;

/// What a placeholder is marked with: its contents aren't on disk.
const NOT_ON_DISK: u32 =
    FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;

/// An open directory.
#[derive(Debug)]
pub(crate) struct Dir(OwnedHandle);

/// Why removing an emptied directory was refused.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// It isn't the directory frozen.
    Replaced,
    /// The OS refused.
    Os(io::Error),
}

/// A file or link opened for removal and read on that handle.
#[derive(Debug)]
pub(crate) struct Victim {
    handle: OwnedHandle,
    seen: Seen,
}

impl Victim {
    /// What it was when read.
    pub(super) const fn seen(&self) -> &Seen {
        &self.seen
    }

    /// Removes it, on the handle it was read from.
    pub(super) fn remove(self) -> io::Result<()> {
        dispose(&self.handle)
    }
}

impl Dir {
    /// The directory at `path`, opened as itself if it is a reparse point.
    pub(crate) fn open_root(path: &Path) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .access_mode(DIR_ACCESS)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        Ok(Self(OwnedHandle::from(file)))
    }

    /// The directory `name` in this one, opened as itself if it is a reparse point.
    pub(crate) fn open_dir(&self, name: &Name) -> io::Result<Self> {
        open_at(self, name, DIR_ACCESS, FILE_DIRECTORY_FILE).map(Self)
    }

    /// What this directory is, from its handle.
    pub(crate) fn seen(&self) -> io::Result<Seen> {
        seen(&self.0)
    }

    /// Whether this is a plain directory, not a link, junction or other reparse point.
    /// Read from its attributes alone, which FAT has even where it gives no file ID.
    pub(crate) fn is_plain(&self) -> io::Result<bool> {
        let tag: FILE_ATTRIBUTE_TAG_INFO = info(&self.0, FileAttributeTagInfo)?;
        let attributes = tag.FileAttributes;
        Ok(attributes & FILE_ATTRIBUTE_DIRECTORY != 0
            && attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0)
    }

    /// Creates the directory `name` in this one. It takes this directory's access rules.
    /// Fails if anything has that name.
    pub(crate) fn create_dir(&self, name: &Name) -> io::Result<()> {
        create_at(self, name, DIR_ACCESS, FILE_CREATE, FILE_DIRECTORY_FILE).map(drop)
    }

    /// Opens the file `name` in this one, as itself, never a directory, for `access`, with
    /// the create `disposition` and the create `options` (such as `FILE_WRITE_THROUGH`)
    /// `NtCreateFile` takes. A file it creates takes this directory's access rules.
    pub(crate) fn open_file(
        &self,
        name: &Name,
        access: u32,
        disposition: u32,
        options: u32,
    ) -> io::Result<File> {
        let options = options | FILE_NON_DIRECTORY_FILE;
        create_at(self, name, access | SYNCHRONIZE, disposition, options).map(File::from)
    }

    /// Its handle, for the calls the run store makes on it.
    pub(crate) const fn handle(&self) -> &OwnedHandle {
        &self.0
    }

    /// What `name` in this directory is, read from a handle that opens it as itself.
    pub(crate) fn stat(&self, name: &Name) -> io::Result<Seen> {
        seen(&open_at(self, name, FILE_READ_ATTRIBUTES | SYNCHRONIZE, 0)?)
    }

    /// The file system this directory is on.
    pub(crate) fn file_system(&self) -> io::Result<FileSystemClass> {
        if remote(&self.0)? {
            return Ok(FileSystemClass::Unsupported(Unsupported::Network));
        }
        Ok(classify(&volume_type(&self.0)?))
    }

    /// The name this directory is stored under in its parent, as the file system's
    /// normalized path gives it: the long name, never an 8.3 short one, in its stored case.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes GetFinalPathNameByHandleW as a raw call"
    )]
    pub(crate) fn stored_name(&self) -> io::Result<OsString> {
        // The longest path Windows has, in UTF-16 units, with its NUL.
        let mut path = vec![0_u16; 32_768];
        let capacity = u32::try_from(path.len()).map_err(io::Error::other)?;
        // SAFETY: the handle is open and `path` is writable for `capacity` units.
        let written = unsafe {
            GetFinalPathNameByHandleW(
                self.0.as_raw_handle(),
                path.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_NONE,
            )
        };
        let written = usize::try_from(written).map_err(io::Error::other)?;
        if written == 0 {
            return Err(io::Error::last_os_error());
        }
        let path = path.get(..written).ok_or(io::ErrorKind::InvalidData)?;
        let last = path.rsplit(|unit| *unit == u16::from(b'\\')).next();
        match last {
            Some(name) if !name.is_empty() => Ok(OsString::from_wide(name)),
            _ => Err(io::ErrorKind::InvalidData.into()),
        }
    }

    /// The names in this directory, without `.` and `..`. Fails rather than return part
    /// of a listing, and on a name holding `:`, which no supported file system allows.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes GetFileInformationByHandleEx as a raw call"
    )]
    pub(crate) fn names(&self) -> io::Result<Vec<OsString>> {
        let mut names = Vec::new();
        // `u64`s keep the buffer aligned for the entries.
        let mut buf = vec![0_u64; 8 * 1024];
        let bytes = u32::try_from(size_of_val(buf.as_slice())).map_err(io::Error::other)?;
        let mut class: FILE_INFO_BY_HANDLE_CLASS = FileIdExtdDirectoryRestartInfo;
        loop {
            // SAFETY: the handle is an open directory and `buf` is writable for `bytes`.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    self.0.as_raw_handle(),
                    class,
                    buf.as_mut_ptr().cast::<c_void>(),
                    bytes,
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                return match error.raw_os_error() {
                    Some(code) if code == ERROR_NO_MORE_FILES.cast_signed() => Ok(names),
                    _ => Err(error),
                };
            }
            // SAFETY: the same initialised memory, read as bytes, which need no alignment.
            let raw = unsafe {
                std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), size_of_val(buf.as_slice()))
            };
            listed(raw, &mut names)?;
            class = FileIdExtdDirectoryInfo;
        }
    }

    /// Makes what changed in this directory durable: the names removed from it, added to
    /// it or renamed in it. The directory is opened again relative to its own handle, for
    /// writing, and flushed (`FlushFileBuffers`), which commits the volume's metadata log
    /// on NTFS and `ReFS`; never the whole volume.
    pub(crate) fn sync(&self) -> io::Result<()> {
        #[cfg(test)]
        super::faults::sync()?;
        flush(&open_at(
            self,
            &Name::new(),
            FLUSH_ACCESS,
            FILE_DIRECTORY_FILE,
        )?)
    }

    /// Syncs the directory `name` in this one, which must still be the directory with the
    /// identity `frozen`: one moved away or replaced since can't be synced from here.
    pub(super) fn sync_named(&self, name: &Name, frozen: FileIdentity) -> io::Result<()> {
        let handle = open_at(self, name, FLUSH_ACCESS, FILE_DIRECTORY_FILE)?;
        if seen(&handle)?.snapshot.identity != frozen {
            return Err(io::Error::other(
                "the directory isn't the one frozen any more",
            ));
        }
        flush(&handle)
    }

    /// `name`, opened for removal and read on that handle.
    pub(super) fn take(&self, name: &Name) -> io::Result<Victim> {
        let handle = open_at(self, name, REMOVE_ACCESS, 0)?;
        let seen = seen(&handle)?;
        Ok(Victim { handle, seen })
    }

    /// Removes the emptied directory `name`, entered as `opened`, if it is empty and still
    /// the directory with the identity `frozen`. It is opened again by name for removal,
    /// checked on that handle and removed on it, as a file is: a directory moved away
    /// since it was entered, or another put in its place, stays.
    pub(super) fn remove_dir(
        &self,
        name: &Name,
        opened: Self,
        frozen: FileIdentity,
    ) -> Result<(), Refusal> {
        drop(opened);
        let handle = open_at(self, name, REMOVE_ACCESS, FILE_DIRECTORY_FILE).map_err(|error| {
            if is_link_or_file(&error) {
                Refusal::Replaced
            } else {
                Refusal::Os(error)
            }
        })?;
        let now = seen(&handle).map_err(Refusal::Os)?;
        if now.snapshot.kind != EntryKind::Dir
            || now.form != Form::Plain
            || now.snapshot.identity != frozen
        {
            return Err(Refusal::Replaced);
        }
        dispose(&handle).map_err(Refusal::Os)
    }
}

/// Opens `name` relative to `dir`, as itself if it is a reparse point.
fn open_at(dir: &Dir, name: &Name, access: u32, options: u32) -> io::Result<OwnedHandle> {
    create_at(dir, name, access, FILE_OPEN, options)
}

/// Opens or creates `name` relative to `dir` as `disposition` says, as itself if it is a
/// reparse point.
#[expect(unsafe_code, reason = "windows-sys exposes NtCreateFile as a raw call")]
fn create_at(
    dir: &Dir,
    name: &Name,
    access: u32,
    disposition: u32,
    options: u32,
) -> io::Result<OwnedHandle> {
    let length = u16::try_from(name.len() * 2).map_err(|_| io::ErrorKind::InvalidInput)?;
    let object = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: name.as_ptr().cast_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>()).unwrap_or(u32::MAX),
        RootDirectory: dir.0.as_raw_handle(),
        ObjectName: &raw const object,
        Attributes: 0,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut handle: HANDLE = ptr::null_mut();
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer is valid for the call; `object` borrows `name`, which
    // outlives it. On success `handle` is a new handle nothing else owns.
    let result = unsafe {
        NtCreateFile(
            &raw mut handle,
            access,
            &raw const attributes,
            &raw mut status,
            ptr::null(),
            0,
            SHARE_ALL,
            disposition,
            options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    nt(result)?;
    // SAFETY: NtCreateFile succeeded, so `handle` is open and ours.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// Writes what `handle` has open, and the metadata it changed, to disk.
#[expect(
    unsafe_code,
    reason = "windows-sys exposes FlushFileBuffers as a raw call"
)]
fn flush(handle: &OwnedHandle) -> io::Result<()> {
    // SAFETY: the handle is open.
    if unsafe { FlushFileBuffers(handle.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Removes the file `handle` has open: its name goes at once, others' open handles
/// stay valid, and a read-only attribute doesn't stop it.
#[expect(
    unsafe_code,
    reason = "windows-sys exposes SetFileInformationByHandle as a raw call"
)]
#[expect(
    clippy::disallowed_methods,
    reason = "the executor's one removal, on a handle it checked"
)]
fn dispose(handle: &OwnedHandle) -> io::Result<()> {
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: `handle` is open with DELETE access and `info` is the struct the class
    // names.
    let ok = unsafe {
        SetFileInformationByHandle(
            handle.as_raw_handle(),
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

/// Renames `name` in `from` to `into` in `to`, failing if anything is at `into`: the run
/// store's one rename. `name` is opened as itself, relative to `from`, and renamed on that
/// handle, relative to `to`'s.
#[expect(
    unsafe_code,
    reason = "windows-sys exposes SetFileInformationByHandle as a raw call"
)]
#[expect(
    clippy::disallowed_methods,
    reason = "a rename between checked directories that never replaces anything"
)]
pub(crate) fn no_replace(from: &Dir, name: &Name, to: &Dir, into: &Name) -> io::Result<()> {
    let handle = open_at(from, name, DELETE | SYNCHRONIZE, 0)?;
    let length = size_of_val(into.as_slice());
    let bytes = size_of::<FILE_RENAME_INFO>() + length;
    // `u64`s keep the buffer aligned for the struct; it is zeroed, so `ReplaceIfExists`
    // is false.
    let mut buf = vec![0_u64; bytes.div_ceil(size_of::<u64>())];
    let rename = buf.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: `buf` is aligned for the struct and holds it with room for `into` from its
    // `FileName` on.
    unsafe {
        (*rename).RootDirectory = to.0.as_raw_handle();
        (*rename).FileNameLength = u32::try_from(length).map_err(io::Error::other)?;
        let to_name = (&raw mut (*rename).FileName).cast::<u16>();
        ptr::copy_nonoverlapping(into.as_ptr(), to_name, into.len());
    }
    // SAFETY: `handle` is open with DELETE access, and `buf` holds the struct the class
    // names and its name, `bytes` long.
    let ok = unsafe {
        SetFileInformationByHandle(
            handle.as_raw_handle(),
            FileRenameInfo,
            buf.as_ptr().cast::<c_void>(),
            u32::try_from(bytes).map_err(io::Error::other)?,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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

/// One fixed-size record about `handle`.
#[expect(
    unsafe_code,
    reason = "windows-sys exposes GetFileInformationByHandleEx as a raw call"
)]
fn info<T: Default>(handle: &OwnedHandle, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    let mut value = T::default();
    // SAFETY: the handle is open and `value` is the struct `class` names, writable.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            class,
            (&raw mut value).cast::<c_void>(),
            u32::try_from(size_of::<T>()).unwrap_or(u32::MAX),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}

/// What the entry `handle` has open is: its kind, identity, times, size, links and
/// allocation, and whether it is a placeholder or another volume's mount point.
fn seen(handle: &OwnedHandle) -> io::Result<Seen> {
    let identity = handle_identity(handle.as_raw_handle())?;
    let basic: FILE_BASIC_INFO = info(handle, FileBasicInfo)?;
    let standard: FILE_STANDARD_INFO = info(handle, FileStandardInfo)?;
    let tag: FILE_ATTRIBUTE_TAG_INFO = info(handle, FileAttributeTagInfo)?;
    let attributes = tag.FileAttributes;
    let reparse = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    let surrogate = reparse && tag.ReparseTag & NAME_SURROGATE != 0;
    let kind = if surrogate {
        EntryKind::Symlink
    } else if standard.Directory {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    let form = if surrogate && tag.ReparseTag == IO_REPARSE_TAG_MOUNT_POINT {
        if names_a_volume(handle) {
            Form::MountPoint
        } else {
            Form::Plain
        }
    } else if (reparse && !surrogate) || attributes & NOT_ON_DISK != 0 {
        Form::Placeholder
    } else {
        Form::Plain
    };
    let snapshot = EntrySnapshot {
        kind,
        identity,
        fingerprint: Fingerprint {
            changed: Stamp::from_windows_ticks(basic.ChangeTime),
            modified: Stamp::from_windows_ticks(basic.LastWriteTime),
            born: Some(Stamp::from_windows_ticks(basic.CreationTime)),
            size: u64::try_from(standard.EndOfFile).map_err(io::Error::other)?,
        },
        links: u64::from(standard.NumberOfLinks),
    };
    Ok(Seen {
        snapshot,
        mount: None,
        allocated: u64::try_from(standard.AllocationSize).ok(),
        form,
    })
}

/// Whether the mount-point reparse point `handle` has open names a volume (a volume
/// mounted on a folder) rather than a directory (a junction). One that can't be read is
/// taken for a volume, which stops a walk rather than enter it.
#[expect(
    unsafe_code,
    reason = "windows-sys exposes DeviceIoControl as a raw call"
)]
fn names_a_volume(handle: &OwnedHandle) -> bool {
    let mut buf = vec![0_u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize];
    let mut returned = 0_u32;
    // SAFETY: the handle is open, and `buf` is writable for its length.
    let ok = unsafe {
        DeviceIoControl(
            handle.as_raw_handle(),
            FSCTL_GET_REPARSE_POINT,
            ptr::null(),
            0,
            buf.as_mut_ptr().cast::<c_void>(),
            u32::try_from(buf.len()).unwrap_or(u32::MAX),
            &raw mut returned,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return true;
    }
    buf.truncate(returned as usize);
    substitute_name(&buf).is_none_or(|name| name.starts_with(&wide(r"\??\Volume{")))
}

/// The substitute name in a mount-point reparse buffer: its tag, data length and two
/// reserved bytes, then the substitute name's offset and length and the print name's,
/// then the names, offsets counted from the end of that header.
fn substitute_name(buf: &[u8]) -> Option<Vec<u16>> {
    let field = |at: usize| {
        buf.get(at..)
            .and_then(<[u8]>::first_chunk::<2>)
            .map(|bytes| usize::from(u16::from_le_bytes(*bytes)))
    };
    let (offset, length) = (field(8)?, field(10)?);
    let names = buf.get(16..)?;
    let name = names.get(offset..offset + length)?;
    let (units, _) = name.as_chunks::<2>();
    Some(units.iter().map(|unit| u16::from_le_bytes(*unit)).collect())
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// Whether `handle` is on a network share or a remote device.
#[expect(
    unsafe_code,
    reason = "windows-sys exposes NtQueryVolumeInformationFile as a raw call"
)]
fn remote(handle: &OwnedHandle) -> io::Result<bool> {
    let mut device = FILE_FS_DEVICE_INFORMATION::default();
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: the handle is open and `device` is the struct the class names, writable.
    let result = unsafe {
        NtQueryVolumeInformationFile(
            handle.as_raw_handle(),
            &raw mut status,
            (&raw mut device).cast::<c_void>(),
            u32::try_from(size_of::<FILE_FS_DEVICE_INFORMATION>()).unwrap_or(u32::MAX),
            FileFsDeviceInformation,
        )
    };
    nt(result)?;
    Ok(device.DeviceType == FILE_DEVICE_NETWORK_FILE_SYSTEM
        || device.Characteristics & FILE_REMOTE_DEVICE != 0)
}

/// The name of the file system `handle` is on, such as `NTFS`.
#[expect(
    unsafe_code,
    reason = "windows-sys exposes GetVolumeInformationByHandleW as a raw call"
)]
fn volume_type(handle: &OwnedHandle) -> io::Result<String> {
    let mut name = [0_u16; 64];
    // SAFETY: the handle is open; every buffer is null with size 0 but the name's.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            handle.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            name.as_mut_ptr(),
            u32::try_from(name.len()).unwrap_or(u32::MAX),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let end = name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(name.len());
    Ok(String::from_utf16_lossy(
        name.get(..end).unwrap_or_default(),
    ))
}

fn classify(name: &str) -> FileSystemClass {
    match name {
        "NTFS" => FileSystemClass::Supported(LocalFileSystem::Ntfs),
        "ReFS" => FileSystemClass::Supported(LocalFileSystem::Refs),
        "FAT" | "FAT32" | "exFAT" => FileSystemClass::Unsupported(Unsupported::NoPosixDelete),
        other => FileSystemClass::Unsupported(Unsupported::Unrecognised(other.to_owned())),
    }
}

/// Appends the names in one buffer of `FILE_ID_EXTD_DIR_INFO` entries to `names`.
fn listed(buf: &[u8], names: &mut Vec<OsString>) -> io::Result<()> {
    let malformed = || io::Error::other("a directory listing that doesn't hold together");
    let u32_at = |at: usize| {
        buf.get(at..)
            .and_then(<[u8]>::first_chunk::<4>)
            .map(|bytes| u32::from_ne_bytes(*bytes) as usize)
    };
    let mut at = 0;
    loop {
        let next = u32_at(at).ok_or_else(malformed)?;
        let length =
            u32_at(at + offset_of!(FILE_ID_EXTD_DIR_INFO, FileNameLength)).ok_or_else(malformed)?;
        let start = at + offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
        let bytes = buf.get(start..start + length).ok_or_else(malformed)?;
        let (units, _) = bytes.as_chunks::<2>();
        let name: Vec<u16> = units.iter().map(|unit| u16::from_ne_bytes(*unit)).collect();
        if name.contains(&u16::from(b':')) {
            return Err(io::Error::other("a name holding `:`"));
        }
        if name != [u16::from(b'.')] && name != [u16::from(b'.'); 2] {
            names.push(OsString::from_wide(&name));
        }
        if next == 0 {
            return Ok(());
        }
        at += next;
    }
}

/// Whether opening a directory failed because the name isn't one.
pub(super) fn is_link_or_file(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_DIRECTORY.cast_signed())
}

/// Whether the OS refused because the entry is open or locked elsewhere.
pub(super) fn is_busy(error: &io::Error) -> bool {
    [
        ERROR_SHARING_VIOLATION,
        ERROR_LOCK_VIOLATION,
        ERROR_USER_MAPPED_FILE,
    ]
    .iter()
    .any(|code| error.raw_os_error() == Some(code.cast_signed()))
}

/// Nothing to turn off: a handle opened without data access, as every one here is,
/// never recalls a cloud file.
#[derive(Debug)]
pub(crate) struct NoMaterialize;

impl NoMaterialize {
    pub(crate) const fn new() -> Self {
        Self
    }
}
