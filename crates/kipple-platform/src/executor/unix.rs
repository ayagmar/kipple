//! Linux and macOS: directory file descriptors, `openat` with `O_NOFOLLOW`, and
//! `unlinkat` relative to the checked directory. A file has no handle to remove it by, so
//! it is read with `fstatat` without following a link right before `unlinkat` removes the
//! same name from the same open directory.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::Path;

use kipple_core::{EntryKind, FileIdentity, FileSystemClass};

#[cfg(target_os = "linux")]
pub(crate) use super::linux::NoMaterialize;
#[cfg(target_os = "linux")]
use super::linux::{clear_errno, errno, file_system, stat_at, stat_fd};
#[cfg(target_os = "macos")]
pub(crate) use super::macos::NoMaterialize;
#[cfg(target_os = "macos")]
use super::macos::{clear_errno, errno, file_system, stat_at, stat_fd};
use super::{Form, Seen};

/// A name as the OS takes it.
pub(crate) type Name = CString;

/// `name` as the OS takes it.
pub(crate) fn name(name: &OsStr) -> io::Result<Name> {
    CString::new(name.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}

/// How every directory is opened: for reading, only if it is a directory, never through
/// a link in its last component.
const DIR_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// An open directory.
#[derive(Debug)]
pub(crate) struct Dir(OwnedFd);

/// Why removing an emptied directory was refused.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// Another directory has its name now.
    Replaced,
    /// The OS refused.
    Os(io::Error),
}

/// A file or link checked and ready to remove: the name in its open directory, and what
/// it was when read.
#[derive(Debug)]
pub(crate) struct Victim<'dir> {
    dir: &'dir Dir,
    name: CString,
    seen: Seen,
}

impl Victim<'_> {
    /// What it was when read.
    pub(super) const fn seen(&self) -> &Seen {
        &self.seen
    }

    /// Removes its name from its directory.
    pub(super) fn remove(self) -> io::Result<()> {
        self.dir.unlink(&self.name, 0)
    }
}

impl Dir {
    /// The directory at `path`, without following a link in its last component.
    pub(crate) fn open_root(path: &Path) -> io::Result<Self> {
        let path = name(path.as_os_str())?;
        Self::open(libc::AT_FDCWD, &path)
    }

    /// The directory `name` in this one, without following a link.
    pub(crate) fn open_dir(&self, name: &Name) -> io::Result<Self> {
        Self::open(self.0.as_raw_fd(), name)
    }

    #[expect(unsafe_code, reason = "libc's openat is a raw call")]
    fn open(at: RawFd, path: &CStr) -> io::Result<Self> {
        // SAFETY: `path` is NUL-terminated and `at` is an open directory or AT_FDCWD.
        let fd = unsafe { libc::openat(at, path.as_ptr(), DIR_FLAGS) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openat` returned a new descriptor that nothing else owns.
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// What this directory is, from its descriptor.
    pub(crate) fn seen(&self) -> io::Result<Seen> {
        stat_fd(self.0.as_raw_fd())
    }

    /// What `name` in this directory is, without following a link.
    pub(crate) fn stat(&self, name: &Name) -> io::Result<Seen> {
        stat_at(self.0.as_raw_fd(), name)
    }

    /// The file system this directory is on.
    pub(crate) fn file_system(&self) -> io::Result<FileSystemClass> {
        file_system(self.0.as_raw_fd())
    }

    /// The names in this directory, without `.` and `..`. Fails rather than return part
    /// of a listing.
    #[expect(unsafe_code, reason = "libc's directory stream calls are raw")]
    pub(super) fn names(&self) -> io::Result<Vec<OsString>> {
        // SAFETY: duplicating an open descriptor; the copy is owned by the stream below.
        let copy = unsafe { libc::fcntl(self.0.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if copy < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `copy` is an open directory descriptor; on success the stream owns it.
        let stream = unsafe { libc::fdopendir(copy) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: the stream didn't take `copy`, so it is still ours to close.
            unsafe { libc::close(copy) };
            return Err(error);
        }
        let listed = read_stream(stream);
        // SAFETY: `stream` came from fdopendir and is closed once, closing `copy`.
        unsafe { libc::closedir(stream) };
        listed
    }

    /// `name`, checked for removal: what it is now, read without following a link.
    pub(super) fn take(&self, name: &Name) -> io::Result<Victim<'_>> {
        let seen = self.stat(name)?;
        Ok(Victim {
            dir: self,
            name: name.clone(),
            seen,
        })
    }

    /// Removes the emptied directory `name`, opened as `opened`, if it still has the
    /// identity `frozen` and is empty.
    pub(super) fn remove_dir(
        &self,
        name: &Name,
        opened: Self,
        frozen: FileIdentity,
    ) -> Result<(), Refusal> {
        drop(opened);
        let now = self.stat(name).map_err(Refusal::Os)?;
        if now.snapshot.kind != EntryKind::Dir || now.snapshot.identity != frozen {
            return Err(Refusal::Replaced);
        }
        self.unlink(name, libc::AT_REMOVEDIR).map_err(|error| {
            // POSIX lets `rmdir` of a directory that isn't empty fail with EEXIST.
            if error.raw_os_error() == Some(libc::EEXIST) {
                Refusal::Os(io::ErrorKind::DirectoryNotEmpty.into())
            } else {
                Refusal::Os(error)
            }
        })
    }

    /// Opens the file `name` in this one with `flags`, never through a link, and never
    /// left open across an `exec`. A file it creates is for its owner only (mode 0600).
    #[expect(unsafe_code, reason = "libc's openat is a raw call")]
    pub(crate) fn open_file(&self, name: &Name, flags: libc::c_int) -> io::Result<File> {
        let flags = flags | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: the descriptor is open and `name` is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                flags,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openat` returned a new descriptor that nothing else owns.
        Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// Makes what changed in this directory durable: the names removed from it, added to
    /// it or renamed in it. `fsync` on Linux, `F_FULLFSYNC` on macOS, as std's `sync_all`
    /// does for a file; never a whole-file-system sync.
    pub(crate) fn sync(&self) -> io::Result<()> {
        File::from(self.0.try_clone()?).sync_all()
    }

    /// Syncs the directory `name` in this one, which must still be the directory with the
    /// identity `frozen`: one moved away or replaced since can't be synced from here.
    pub(super) fn sync_named(&self, name: &Name, frozen: FileIdentity) -> io::Result<()> {
        let dir = self.open_dir(name)?;
        if dir.seen()?.snapshot.identity != frozen {
            return Err(io::Error::other(
                "the directory isn't the one frozen any more",
            ));
        }
        dir.sync()
    }

    /// Its descriptor, for the calls the trash makes relative to it.
    pub(crate) fn fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }

    /// Removes `name` from this directory: a file or link, or with `AT_REMOVEDIR` an empty
    /// directory.
    #[expect(unsafe_code, reason = "libc's unlinkat is a raw call")]
    #[expect(
        clippy::disallowed_methods,
        reason = "the executor's one removal, by name in a checked directory"
    )]
    pub(crate) fn unlink(&self, name: &CStr, flags: libc::c_int) -> io::Result<()> {
        // SAFETY: the descriptor is open and `name` is NUL-terminated.
        if unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), flags) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Renames `name` in `from` to `into` in `to`, failing if anything is at `into`: the
/// trash's moves.
#[cfg(target_os = "linux")]
#[expect(unsafe_code, reason = "libc's renameat2 is a raw call")]
#[expect(
    clippy::disallowed_methods,
    reason = "a move between checked directories that never replaces anything"
)]
pub(crate) fn no_replace(from: &Dir, name: &Name, to: &Dir, into: &Name) -> io::Result<()> {
    // SAFETY: both descriptors are open and both names are NUL-terminated.
    let done = unsafe {
        libc::renameat2(
            from.fd(),
            name.as_ptr(),
            to.fd(),
            into.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if done != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Renames `name` in `from` to `into` in `to`, failing if anything is at `into`: the
/// trash's moves.
#[cfg(target_os = "macos")]
#[expect(unsafe_code, reason = "libc's renameatx_np is a raw call")]
#[expect(
    clippy::disallowed_methods,
    reason = "a move between checked directories that never replaces anything"
)]
pub(crate) fn no_replace(from: &Dir, name: &Name, to: &Dir, into: &Name) -> io::Result<()> {
    // SAFETY: both descriptors are open and both names are NUL-terminated.
    let done = unsafe {
        libc::renameatx_np(
            from.fd(),
            name.as_ptr(),
            to.fd(),
            into.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if done != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Every name `stream` lists but `.` and `..`.
#[expect(unsafe_code, reason = "reads entries libc's readdir returns")]
fn read_stream(stream: *mut libc::DIR) -> io::Result<Vec<OsString>> {
    let mut names = Vec::new();
    loop {
        clear_errno();
        // SAFETY: `stream` is an open directory stream.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            return match errno() {
                0 => Ok(names),
                code => Err(io::Error::from_raw_os_error(code)),
            };
        }
        // SAFETY: a non-null entry holds a NUL-terminated name until the next readdir.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(OsString::from_vec(name.to_vec()));
        }
    }
}

/// Whether opening a directory failed because the name is a link or not a directory.
pub(crate) fn is_link_or_file(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR))
}

/// Whether the OS refused because the entry is in use.
pub(super) fn is_busy(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EBUSY | libc::ETXTBSY))
}

/// The kind of an entry from its `st_mode`.
pub(super) const fn kind(mode: libc::mode_t) -> EntryKind {
    match mode & libc::S_IFMT {
        libc::S_IFDIR => EntryKind::Dir,
        libc::S_IFREG => EntryKind::File,
        libc::S_IFLNK => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

/// A plain entry read from `snapshot`, with its mount and allocation.
pub(super) const fn plain(
    snapshot: kipple_core::EntrySnapshot,
    mount: Option<u64>,
    blocks: u64,
) -> Seen {
    Seen {
        snapshot,
        mount,
        // `st_blocks` counts 512-byte units on Linux and macOS, whatever the block size.
        allocated: Some(blocks.saturating_mul(512)),
        form: Form::Plain,
    }
}
