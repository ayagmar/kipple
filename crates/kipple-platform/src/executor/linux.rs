//! Linux: `statx`, whose mount ID tells a bind mount of the same device apart, and the
//! `statfs` magic number of the file system.

use std::ffi::CStr;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::RawFd;

use kipple_core::{
    EntrySnapshot, FileIdentity, FileSystemClass, Fingerprint, LocalFileSystem, Stamp, Unsupported,
};

use super::Seen;
use super::unix::{kind, plain};
use crate::os::{NETWORK_FILE_SYSTEMS, magic};

/// What every `statx` asks for. Of the two mount IDs, the kernel fills the unique one
/// where it has it (6.8 and later).
const WANTED: libc::c_uint =
    NEEDED | libc::STATX_BTIME | libc::STATX_MNT_ID | libc::STATX_MNT_ID_UNIQUE;

/// What a `statx` must return for the entry to be known.
const NEEDED: libc::c_uint = libc::STATX_TYPE
    | libc::STATX_MODE
    | libc::STATX_NLINK
    | libc::STATX_INO
    | libc::STATX_SIZE
    | libc::STATX_BLOCKS
    | libc::STATX_CTIME
    | libc::STATX_MTIME;

/// What the open `fd` is.
pub(super) fn stat_fd(fd: RawFd) -> io::Result<Seen> {
    statx(fd, c"", libc::AT_EMPTY_PATH)
}

/// What `name` in the open directory `fd` is, without following a link.
pub(super) fn stat_at(fd: RawFd, name: &CStr) -> io::Result<Seen> {
    statx(fd, name, libc::AT_SYMLINK_NOFOLLOW)
}

#[expect(unsafe_code, reason = "libc's statx is a raw call")]
fn statx(fd: RawFd, path: &CStr, flags: libc::c_int) -> io::Result<Seen> {
    let mut buf = MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: `path` is NUL-terminated, `fd` is open, and `buf` is large enough.
    let done = unsafe {
        libc::statx(
            fd,
            path.as_ptr(),
            flags | libc::AT_NO_AUTOMOUNT,
            WANTED,
            buf.as_mut_ptr(),
        )
    };
    if done != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `statx` succeeded, so it filled `buf`, which was zeroed besides.
    let stx = unsafe { buf.assume_init() };
    seen(&stx)
}

fn seen(stx: &libc::statx) -> io::Result<Seen> {
    if stx.stx_mask & NEEDED != NEEDED {
        return Err(io::Error::other("statx left out a field kipple needs"));
    }
    if stx.stx_mask & (libc::STATX_MNT_ID | libc::STATX_MNT_ID_UNIQUE) == 0 {
        return Err(io::Error::other("statx gave no mount ID"));
    }
    let born = if stx.stx_mask & libc::STATX_BTIME == 0 {
        None
    } else {
        Some(stamp(stx.stx_btime)?)
    };
    let snapshot = EntrySnapshot {
        kind: kind(libc::mode_t::from(stx.stx_mode)),
        identity: FileIdentity {
            volume: libc::makedev(stx.stx_dev_major, stx.stx_dev_minor),
            file: u128::from(stx.stx_ino),
        },
        fingerprint: Fingerprint {
            changed: stamp(stx.stx_ctime)?,
            modified: stamp(stx.stx_mtime)?,
            born,
            size: stx.stx_size,
        },
        links: u64::from(stx.stx_nlink),
    };
    Ok(plain(snapshot, Some(stx.stx_mnt_id), stx.stx_blocks))
}

fn stamp(time: libc::statx_timestamp) -> io::Result<Stamp> {
    Stamp::new(time.tv_sec, time.tv_nsec).ok_or_else(|| io::Error::other("a time out of range"))
}

/// The file system the open `fd` is on, by its `statfs` magic number.
#[expect(unsafe_code, reason = "libc's fstatfs is a raw call")]
pub(super) fn file_system(fd: RawFd) -> io::Result<FileSystemClass> {
    let mut stat = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `fd` is open and `stat` is large enough for the result.
    if unsafe { libc::fstatfs(fd, stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fstatfs` succeeded, so it filled `stat`.
    Ok(classify(magic(unsafe { stat.assume_init() }.f_type)))
}

/// The allowlist: ext4 (and ext2 and ext3, which share its magic number), XFS, btrfs,
/// tmpfs and F2FS.
const LOCAL: [(i64, LocalFileSystem); 5] = [
    (0xEF53, LocalFileSystem::Ext4),
    (0x5846_5342, LocalFileSystem::Xfs),
    (0x9123_683E, LocalFileSystem::Btrfs),
    (0x0102_1994, LocalFileSystem::Tmpfs),
    (0xF2F5_2010, LocalFileSystem::F2fs),
];

/// FAT (`msdos`, `vfat`) and exFAT, which can't remove an open file's name.
const NO_POSIX_DELETE: [i64; 2] = [0x4D44, 0x2011_BAB0];

/// FUSE.
const USER_SPACE: i64 = 0x6573_5546;

fn classify(magic: i64) -> FileSystemClass {
    if let Some((_, local)) = LOCAL.iter().find(|(number, _)| *number == magic) {
        return FileSystemClass::Supported(*local);
    }
    FileSystemClass::Unsupported(if NO_POSIX_DELETE.contains(&magic) {
        Unsupported::NoPosixDelete
    } else if magic == USER_SPACE {
        Unsupported::UserSpace
    } else if NETWORK_FILE_SYSTEMS.contains(&magic) {
        Unsupported::Network
    } else {
        Unsupported::Unrecognised(format!("{magic:#x}"))
    })
}

/// Nothing to turn off: Linux has no dataless files of its own.
#[derive(Debug)]
pub(crate) struct NoMaterialize;

impl NoMaterialize {
    pub(crate) const fn new() -> Self {
        Self
    }
}

#[expect(
    unsafe_code,
    reason = "errno is a thread-local the C library exposes raw"
)]
pub(super) fn clear_errno() {
    // SAFETY: the pointer is this thread's errno, always valid.
    unsafe { *libc::__errno_location() = 0 };
}

#[expect(
    unsafe_code,
    reason = "errno is a thread-local the C library exposes raw"
)]
pub(super) fn errno() -> i32 {
    // SAFETY: the pointer is this thread's errno, always valid.
    unsafe { *libc::__errno_location() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches FAT, FUSE or a network share classified as a file system kipple deletes on.
    #[test]
    fn only_the_allowlist_is_supported() {
        assert_eq!(
            classify(0xEF53),
            FileSystemClass::Supported(LocalFileSystem::Ext4)
        );
        assert_eq!(
            classify(0x4D44),
            FileSystemClass::Unsupported(Unsupported::NoPosixDelete)
        );
        assert_eq!(
            classify(0x6573_5546),
            FileSystemClass::Unsupported(Unsupported::UserSpace)
        );
        assert_eq!(
            classify(0x6969),
            FileSystemClass::Unsupported(Unsupported::Network)
        );
        assert_eq!(
            classify(0x794C_7630),
            FileSystemClass::Unsupported(Unsupported::Unrecognised("0x794c7630".to_owned()))
        );
    }
}
