//! macOS: `fstatat` with the file flags that mark a dataless file, the file system's
//! type name, and materialization of dataless files turned off for the walking thread.

use std::ffi::{CStr, c_int};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::RawFd;

use kipple_core::{
    EntrySnapshot, FileIdentity, FileSystemClass, Fingerprint, LocalFileSystem, Stamp, Unsupported,
};

use super::unix::{kind, plain};
use super::{Form, Seen};

/// `SF_DATALESS` from `<sys/stat.h>`: the file's contents are in the cloud, and reading
/// them would download them.
const SF_DATALESS: u32 = 0x4000_0000;

/// From `<sys/resource.h>`.
const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3;
const IOPOL_SCOPE_THREAD: c_int = 1;
const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: c_int = 1;

#[expect(
    unsafe_code,
    reason = "declares the I/O policy calls, which libc doesn't bind"
)]
unsafe extern "C" {
    fn getiopolicy_np(iotype: c_int, scope: c_int) -> c_int;
    fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
}

/// Turns materialization of dataless files off for this thread while it lives, so a walk
/// never downloads anything: touching a dataless file's contents fails instead. The
/// thread's earlier policy comes back on drop.
#[derive(Debug)]
pub(crate) struct NoMaterialize {
    before: Option<c_int>,
}

impl NoMaterialize {
    #[expect(unsafe_code, reason = "the I/O policy calls are raw")]
    pub(crate) fn new() -> Self {
        // SAFETY: plain calls with constant arguments.
        let before = unsafe {
            getiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_THREAD,
            )
        };
        // SAFETY: as above.
        unsafe {
            setiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_THREAD,
                IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            )
        };
        Self {
            before: (before >= 0).then_some(before),
        }
    }
}

impl Drop for NoMaterialize {
    #[expect(unsafe_code, reason = "the I/O policy calls are raw")]
    fn drop(&mut self) {
        if let Some(before) = self.before {
            // SAFETY: restores the policy this thread had.
            unsafe {
                setiopolicy_np(
                    IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                    IOPOL_SCOPE_THREAD,
                    before,
                )
            };
        }
    }
}

/// What the open `fd` is.
#[expect(unsafe_code, reason = "libc's fstat is a raw call")]
pub(super) fn stat_fd(fd: RawFd) -> io::Result<Seen> {
    let mut stat = MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `fd` is open and `stat` is large enough.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fstat` succeeded, so it filled `stat`.
    seen(&unsafe { stat.assume_init() })
}

/// What `name` in the open directory `fd` is, without following a link.
#[expect(unsafe_code, reason = "libc's fstatat is a raw call")]
pub(super) fn stat_at(fd: RawFd, name: &CStr) -> io::Result<Seen> {
    let mut stat = MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `fd` is open, `name` is NUL-terminated and `stat` is large enough.
    let done = unsafe {
        libc::fstatat(
            fd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if done != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fstatat` succeeded, so it filled `stat`.
    seen(&unsafe { stat.assume_init() })
}

fn seen(stat: &libc::stat) -> io::Result<Seen> {
    let time = |secs: i64, nanos: i64| {
        u32::try_from(nanos)
            .ok()
            .and_then(|nanos| Stamp::new(secs, nanos))
            .ok_or_else(|| io::Error::other("a time out of range"))
    };
    let snapshot = EntrySnapshot {
        kind: kind(stat.st_mode),
        identity: FileIdentity {
            // Widened as std's `MetadataExt::dev` does, so it matches the scan's identity.
            volume: i64::from(stat.st_dev).cast_unsigned(),
            file: u128::from(stat.st_ino),
        },
        fingerprint: Fingerprint {
            changed: time(stat.st_ctime, stat.st_ctime_nsec)?,
            modified: time(stat.st_mtime, stat.st_mtime_nsec)?,
            born: Some(time(stat.st_birthtime, stat.st_birthtime_nsec)?),
            size: u64::try_from(stat.st_size).map_err(io::Error::other)?,
        },
        links: u64::from(stat.st_nlink),
    };
    let blocks = u64::try_from(stat.st_blocks).map_err(io::Error::other)?;
    let mut seen = plain(snapshot, None, blocks);
    if stat.st_flags & SF_DATALESS != 0 {
        seen.form = Form::Placeholder;
    }
    Ok(seen)
}

/// The file system the open `fd` is on, by its type name and local flag.
#[expect(unsafe_code, reason = "libc's fstatfs is a raw call")]
pub(super) fn file_system(fd: RawFd) -> io::Result<FileSystemClass> {
    let mut stat = MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `fd` is open and `stat` is large enough for the result.
    if unsafe { libc::fstatfs(fd, stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fstatfs` succeeded, so it filled `stat`.
    let stat = unsafe { stat.assume_init() };
    let name: Vec<u8> = stat
        .f_fstypename
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| byte.cast_unsigned())
        .collect();
    Ok(classify(&String::from_utf8_lossy(&name), stat.f_flags))
}

/// The class of a file system with the type name `name` and mount flags `flags`.
fn classify(name: &str, flags: u32) -> FileSystemClass {
    let local = flags & libc::MNT_LOCAL.cast_unsigned() != 0;
    match name {
        "apfs" if local => return FileSystemClass::Supported(LocalFileSystem::Apfs),
        "hfs" if local => return FileSystemClass::Supported(LocalFileSystem::HfsPlus),
        _ => {}
    }
    FileSystemClass::Unsupported(match name {
        "msdos" | "exfat" => Unsupported::NoPosixDelete,
        "nfs" | "smbfs" | "afpfs" | "webdav" | "cifs" => Unsupported::Network,
        _ if name.contains("fuse") => Unsupported::UserSpace,
        _ if !local => Unsupported::Network,
        _ => Unsupported::Unrecognised(name.to_owned()),
    })
}

#[expect(
    unsafe_code,
    reason = "errno is a thread-local the C library exposes raw"
)]
pub(super) fn clear_errno() {
    // SAFETY: the pointer is this thread's errno, always valid.
    unsafe { *libc::__error() = 0 };
}

#[expect(
    unsafe_code,
    reason = "errno is a thread-local the C library exposes raw"
)]
pub(super) fn errno() -> i32 {
    // SAFETY: the pointer is this thread's errno, always valid.
    unsafe { *libc::__error() }
}
