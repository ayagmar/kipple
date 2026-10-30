//! Linux and macOS: the trash directory and the item's parent opened as handles, and one
//! rename between them that never replaces anything (Linux `renameat2(RENAME_NOREPLACE)`,
//! macOS `renameatx_np(RENAME_EXCL)`).

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Write as _};
use std::mem::MaybeUninit;
use std::os::fd::{FromRawFd as _, OwnedFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kipple_core::{
    Anchor, ApplyError, Authorized, CancelToken, Component, Digest, EntrySnapshot, ErrorCode,
    FileSystemClass, RestoreRequest, Scope, SlotState, TrashDir, TrashOp, TrashSlot, Trashed,
    Volume,
};

use super::ConfinedTrash;
use super::verify::verify;
use crate::executor::sys::{self, Dir, Name};
use crate::executor::{Reached, reach};

/// Whether the trash is freedesktop's (Linux): trashed items in `files`, each described
/// by a `.trashinfo` in `info` beside it, and a trash at the top of other mounts. On
/// macOS items go into `~/.Trash` alone.
const FREEDESKTOP: bool = cfg!(target_os = "linux");

/// What names an item's `.trashinfo`: the item's name in `files` and this.
const INFO_SUFFIX: &str = ".trashinfo";

/// The longest name Linux and macOS file systems take, in bytes.
const NAME_MAX: usize = 255;

/// How many names [`destination`] tries before it gives up.
const SLOT_TRIES: u32 = 100;

/// A trash directory, opened from its path and checked.
struct Opened {
    /// Where trashed items go.
    files: Dir,
    /// Where their `.trashinfo` go (freedesktop).
    info: Option<Dir>,
    /// What it is.
    dir: TrashDir,
    /// The volume it is on, which an item must be on to move there.
    volume: Volume,
}

/// The codes a refused move reports: the item gone, or another entry in its place.
#[derive(Clone, Copy)]
struct Codes {
    missing: ErrorCode,
    changed: ErrorCode,
}

/// An item moving into the trash.
const TRASHING: Codes = Codes {
    missing: ErrorCode::TargetMissing,
    changed: ErrorCode::TargetChanged,
};

/// An item moving back out of the trash.
const RESTORING: Codes = Codes {
    missing: ErrorCode::TrashMissing,
    changed: ErrorCode::TrashChanged,
};

/// One rename: `name` in `from` to `into` in `to`.
struct Move<'dirs> {
    from: &'dirs Dir,
    name: &'dirs Name,
    to: &'dirs Dir,
    into: &'dirs Name,
}

pub(super) fn available(trash: &ConfinedTrash, anchor: &Anchor) -> Result<(), ApplyError> {
    locate(trash, anchor).map(|_| ())
}

/// The slot `op` will move to: its name and a tag from the digest of what it moves, in
/// the trash on its file system, at a name free in `files` and `info` now.
pub(super) fn destination(trash: &ConfinedTrash, op: &TrashOp) -> Result<TrashSlot, ApplyError> {
    let target = op.target();
    let original = last(target.anchor())?;
    let (_, opened) = locate(trash, target.anchor())?;
    let mut tries = 0..SLOT_TRIES;
    let name = loop {
        let Some(n) = tries.next() else {
            return Err(ApplyError::new(ErrorCode::DestinationExists));
        };
        let name = slot_name(original.as_os_str(), target.digest(), n)?;
        if free(&opened, &name)? {
            break name;
        }
    };
    Ok(TrashSlot {
        trash: opened.dir,
        name,
    })
}

/// Checks `op`'s item against its tree, then moves it whole to `slot`: on Linux after
/// writing its `.trashinfo`, which goes again if the move doesn't happen.
pub(super) fn move_whole(
    trash: &ConfinedTrash,
    op: &TrashOp,
    slot: &TrashSlot,
    cancel: &CancelToken,
) -> Result<Trashed, ApplyError> {
    if cancel.is_cancelled() {
        return Err(ApplyError::new(ErrorCode::Cancelled));
    }
    let target = op.target();
    whole(target)?;
    let anchor = target.anchor();
    let name = os_name(last(anchor)?)?;
    let reached = reached(anchor)?;
    if target.tree().volume() != reached.volume {
        return Err(ApplyError::new(ErrorCode::RootChanged));
    }
    let opened = open(trash, &slot.trash.path)?;
    if opened.dir != slot.trash {
        return Err(ApplyError::new(ErrorCode::TrashChanged));
    }
    if opened.volume != reached.volume {
        return Err(ApplyError::new(ErrorCode::CrossFileSystem));
    }
    verify(
        reached.holder(),
        &name,
        target.tree(),
        reached.volume,
        cancel,
    )?;
    if cancel.is_cancelled() {
        return Err(ApplyError::new(ErrorCode::Cancelled));
    }
    let top = target
        .tree()
        .top()
        .snapshot()
        .ok_or_else(|| ApplyError::new(ErrorCode::TargetChanged))?;
    let into = os_name(&slot.name)?;
    let record = Record::write(&opened, &slot.name, &anchor.path(), trash.clock.now())?;
    let rename = Move {
        from: reached.holder(),
        name: &name,
        to: &opened.files,
        into: &into,
    };
    match shift(&rename, &top, TRASHING) {
        Ok(()) => Ok(Trashed { slot: slot.clone() }),
        Err(error) => {
            record.undo();
            Err(error)
        }
    }
}

/// What is at `slot` now: the item, by kind and identity, or something else, or nothing.
/// A trash directory that isn't there any more holds nothing; one replaced holds
/// something else.
pub(super) fn find(
    trash: &ConfinedTrash,
    slot: &TrashSlot,
    target: &EntrySnapshot,
) -> Result<SlotState, ApplyError> {
    let opened = match open(trash, &slot.trash.path) {
        Ok(opened) => opened,
        Err(error) if error.code == ErrorCode::TrashMissing => return Ok(SlotState::Empty),
        Err(error) => return Err(error),
    };
    if opened.dir.identity != slot.trash.identity {
        return Ok(SlotState::Other);
    }
    match opened.files.stat(&os_name(&slot.name)?) {
        Ok(seen) if same(&seen.snapshot, target) => Ok(SlotState::Holds),
        Ok(_) => Ok(SlotState::Other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SlotState::Empty),
        Err(error) => Err(ApplyError::io(ErrorCode::Io, error)),
    }
}

/// Moves the item at `request`'s slot back into its parent, reached from its verified
/// root, which must still have the identity recorded. On Linux its `.trashinfo` goes
/// after.
pub(super) fn restore(
    trash: &ConfinedTrash,
    request: &RestoreRequest,
    cancel: &CancelToken,
) -> Result<(), ApplyError> {
    if cancel.is_cancelled() {
        return Err(ApplyError::new(ErrorCode::Cancelled));
    }
    let (slot, anchor) = (request.slot(), request.anchor());
    let name = os_name(last(anchor)?)?;
    let opened = open(trash, &slot.trash.path)?;
    if opened.dir.identity != slot.trash.identity {
        return Err(ApplyError::new(ErrorCode::TrashChanged));
    }
    let reached = reach(anchor)?;
    if reached.holder_seen().snapshot.identity != request.parent() {
        return Err(ApplyError::new(ErrorCode::ParentChanged));
    }
    let from = os_name(&slot.name)?;
    let rename = Move {
        from: &opened.files,
        name: &from,
        to: reached.holder(),
        into: &name,
    };
    shift(&rename, request.target(), RESTORING)?;
    if let (Some(info), Ok(described)) = (&opened.info, info_name(&slot.name)) {
        // The item is back; a `.trashinfo` left behind would only list it in the trash
        // without anything there.
        info.unlink(&described, 0).ok();
    }
    Ok(())
}

/// The trash `anchor`'s item can move to, with the item's parent reached: the home trash
/// when it is on the item's mount, otherwise (Linux) one at the top of the item's mount.
fn locate(trash: &ConfinedTrash, anchor: &Anchor) -> Result<(Reached, Opened), ApplyError> {
    let reached = reached(anchor)?;
    let mut refused = None;
    if let Some(home) = &trash.home {
        match open(trash, home) {
            Ok(opened) if takes(&opened, reached.volume) => return Ok((reached, opened)),
            Ok(_) => {}
            Err(error) => refused = Some(error),
        }
    }
    if FREEDESKTOP && let Some(top) = top_of(&anchor.root.path, reached.volume) {
        for path in top_trashes(&top) {
            if let Ok(opened) = open(trash, &path)
                && takes(&opened, reached.volume)
            {
                return Ok((reached, opened));
            }
        }
    }
    Err(refused.unwrap_or_else(|| ApplyError::new(ErrorCode::CrossFileSystem)))
}

/// Whether `opened` can take an item on `volume`: it is on that volume, and on Linux has
/// the `info` directory its `.trashinfo` goes in.
fn takes(opened: &Opened, volume: Volume) -> bool {
    opened.volume == volume && (opened.info.is_some() || !FREEDESKTOP)
}

/// `anchor`'s parent reached from its verified root, on a file system kipple changes.
fn reached(anchor: &Anchor) -> Result<Reached, ApplyError> {
    let reached = reach(anchor)?;
    if let FileSystemClass::Unsupported(_) = reached.file_system {
        return Err(ApplyError::new(ErrorCode::UnsupportedFileSystem));
    }
    Ok(reached)
}

/// The highest directory above and at `root` still on `volume`: the top of its mount.
fn top_of(root: &Path, volume: Volume) -> Option<PathBuf> {
    root.ancestors()
        .take_while(|dir| {
            Dir::open_root(dir)
                .and_then(|dir| dir.seen())
                .is_ok_and(|seen| seen.on(volume))
        })
        .last()
        .map(Path::to_owned)
}

/// The freedesktop trashes at the top directory `top`, in the order the specification
/// tries them.
fn top_trashes(top: &Path) -> [PathBuf; 2] {
    let uid = uid().to_string();
    [
        top.join(".Trash").join(&uid).join("files"),
        top.join(format!(".Trash-{uid}")).join("files"),
    ]
}

/// Opens the trash directory `path`, which must be one this backend uses, and checks it:
/// never through a link, owned by the user, and on Linux with `info` beside it, if it is
/// there, on the same volume.
fn open(trash: &ConfinedTrash, path: &Path) -> Result<Opened, ApplyError> {
    let home = trash.home.as_deref() == Some(path);
    if !FREEDESKTOP {
        if !home {
            return Err(ApplyError::new(ErrorCode::Unsupported));
        }
        let files = open_path(path)?;
        owned(&files)?;
        return opened(path, files, None);
    }
    let (Some(base), Some("files")) = (path.parent(), path.file_name().and_then(OsStr::to_str))
    else {
        return Err(ApplyError::new(ErrorCode::Unsupported));
    };
    let base = if home {
        open_path(base)?
    } else {
        top_base(base)?
    };
    owned(&base)?;
    let files = open_in(&base, "files")?;
    owned(&files)?;
    // Only a move needs `info`: what is in `files` stays findable and restorable without.
    let info = match open_in(&base, "info") {
        Ok(info) => {
            owned(&info)?;
            Some(info)
        }
        Err(error) if error.code == ErrorCode::TrashMissing => None,
        Err(error) => return Err(error),
    };
    opened(path, files, info)
}

/// The trash directory `files` at `path`, with `info` beside it, as [`Opened`].
fn opened(path: &Path, files: Dir, info: Option<Dir>) -> Result<Opened, ApplyError> {
    let seen = files
        .seen()
        .map_err(|error| ApplyError::io(ErrorCode::Io, error))?;
    let volume = seen.volume();
    if let Some(info) = &info {
        let beside = info
            .seen()
            .map_err(|error| ApplyError::io(ErrorCode::Io, error))?;
        if !beside.on(volume) {
            return Err(ApplyError::new(ErrorCode::MountBoundary));
        }
    }
    Ok(Opened {
        files,
        info,
        dir: TrashDir {
            path: path.to_owned(),
            identity: seen.snapshot.identity,
        },
        volume,
    })
}

/// Opens a top directory's trash, `base` (`$topdir/.Trash/$uid` or `$topdir/.Trash-$uid`),
/// from `$topdir`: `.Trash` must be sticky.
fn top_base(base: &Path) -> Result<Dir, ApplyError> {
    let not_a_trash = || ApplyError::new(ErrorCode::Unsupported);
    let uid = uid().to_string();
    let name = base.file_name().ok_or_else(not_a_trash)?;
    let above = base.parent().ok_or_else(not_a_trash)?;
    if name == OsStr::new(&format!(".Trash-{uid}")) {
        return open_in(&open_path(above)?, &format!(".Trash-{uid}"));
    }
    if name == OsStr::new(&uid) && above.file_name() == Some(OsStr::new(".Trash")) {
        let top = above.parent().ok_or_else(not_a_trash)?;
        let shared = open_in(&open_path(top)?, ".Trash")?;
        if mode(&shared)? & libc::S_ISVTX == 0 {
            return Err(not_a_trash());
        }
        return open_in(&shared, &uid);
    }
    Err(not_a_trash())
}

/// The directory at `path`, never through a link in its last component.
fn open_path(path: &Path) -> Result<Dir, ApplyError> {
    Dir::open_root(path).map_err(opening)
}

/// The directory `name` in `dir`, never through a link.
fn open_in(dir: &Dir, name: &str) -> Result<Dir, ApplyError> {
    let name = sys::name(OsStr::new(name)).map_err(|error| ApplyError::io(ErrorCode::Io, error))?;
    dir.open_dir(&name).map_err(opening)
}

/// Why a trash directory didn't open.
fn opening(error: io::Error) -> ApplyError {
    let code = if error.kind() == io::ErrorKind::NotFound {
        ErrorCode::TrashMissing
    } else if sys::is_link_or_file(&error) {
        ErrorCode::ThroughLink
    } else {
        ErrorCode::Io
    };
    ApplyError::io(code, error)
}

/// Refuses a trash directory someone else owns: what is in it isn't the user's to restore.
fn owned(dir: &Dir) -> Result<(), ApplyError> {
    let stat = status(dir)?;
    if stat.st_uid == uid() {
        Ok(())
    } else {
        Err(ApplyError::new(ErrorCode::Unsupported))
    }
}

fn mode(dir: &Dir) -> Result<libc::mode_t, ApplyError> {
    status(dir).map(|stat| stat.st_mode)
}

#[expect(unsafe_code, reason = "libc's fstat is a raw call")]
fn status(dir: &Dir) -> Result<libc::stat, ApplyError> {
    let mut stat = MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: the descriptor is open and `stat` is large enough.
    if unsafe { libc::fstat(dir.fd(), stat.as_mut_ptr()) } != 0 {
        return Err(ApplyError::io(ErrorCode::Io, io::Error::last_os_error()));
    }
    // SAFETY: `fstat` succeeded, so it filled `stat`.
    Ok(unsafe { stat.assume_init() })
}

#[expect(unsafe_code, reason = "libc's getuid is a raw call")]
fn uid() -> libc::uid_t {
    // SAFETY: getuid has no preconditions and can't fail.
    unsafe { libc::getuid() }
}

/// Refuses anything but a whole item below its root with every entry still authorized
/// (amendment J): a root's contents, or a tree with an entry left out or dropped, can't
/// move whole. `admit` makes no other [`TrashOp`]; this holds even if it did.
fn whole(target: &Authorized) -> Result<(), ApplyError> {
    let tree = target.tree();
    let whole = tree.scope() == Scope::Whole
        && target.anchor().scope() == Scope::Whole
        && tree.entries().all(|entry| target.may_remove(entry.id()));
    if whole {
        Ok(())
    } else {
        Err(ApplyError::new(ErrorCode::Unauthorized))
    }
}

/// The target's name in its parent. A root has none, and is never moved.
fn last(anchor: &Anchor) -> Result<&Component, ApplyError> {
    anchor
        .relative
        .components()
        .last()
        .ok_or_else(|| ApplyError::new(ErrorCode::Unauthorized))
}

fn os_name(component: &Component) -> Result<Name, ApplyError> {
    sys::name(component.as_os_str()).map_err(|error| ApplyError::io(ErrorCode::Io, error))
}

/// Whether `now` is the entry `then` by kind and identity. A move changes its status
/// change time, so the fingerprint isn't compared.
fn same(now: &EntrySnapshot, then: &EntrySnapshot) -> bool {
    now.kind == then.kind && now.identity == then.identity
}

/// Renames `rename.name`, which must be `expected`, without replacing anything, then
/// checks what arrived is `expected`. Something else, swapped in between the check and
/// the rename, is moved back and the move refused.
fn shift(rename: &Move<'_>, expected: &EntrySnapshot, codes: Codes) -> Result<(), ApplyError> {
    let missing = |error: io::Error| {
        let code = if error.kind() == io::ErrorKind::NotFound {
            codes.missing
        } else {
            ErrorCode::Io
        };
        ApplyError::io(code, error)
    };
    let now = rename.from.stat(rename.name).map_err(missing)?;
    if !same(&now.snapshot, expected) {
        return Err(ApplyError::new(codes.changed));
    }
    no_replace(rename.from, rename.name, rename.to, rename.into)
        .map_err(|error| refused(error, codes))?;
    let arrived = rename.to.stat(rename.into);
    if arrived.is_ok_and(|seen| same(&seen.snapshot, expected)) {
        return Ok(());
    }
    // The rename moved what was there by then. Put it back where it was found.
    no_replace(rename.to, rename.into, rename.from, rename.name).ok();
    Err(ApplyError::new(codes.changed))
}

/// Why a rename was refused.
fn refused(error: io::Error, codes: Codes) -> ApplyError {
    let code = match error.raw_os_error() {
        Some(libc::EEXIST | libc::ENOTEMPTY) => ErrorCode::DestinationExists,
        Some(libc::EXDEV) => ErrorCode::CrossFileSystem,
        Some(libc::ENOENT) => codes.missing,
        Some(libc::EINVAL | libc::ENOTSUP | libc::ENOSYS) => ErrorCode::Unsupported,
        _ => ErrorCode::Io,
    };
    ApplyError::io(code, error)
}

/// Renames `name` in `from` to `into` in `to`, failing if anything is at `into`.
#[cfg(target_os = "linux")]
#[expect(unsafe_code, reason = "libc's renameat2 is a raw call")]
#[expect(
    clippy::disallowed_methods,
    reason = "the trash's one move, between checked directories, never replacing"
)]
fn no_replace(from: &Dir, name: &Name, to: &Dir, into: &Name) -> io::Result<()> {
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

/// Renames `name` in `from` to `into` in `to`, failing if anything is at `into`.
#[cfg(target_os = "macos")]
#[expect(unsafe_code, reason = "libc's renameatx_np is a raw call")]
#[expect(
    clippy::disallowed_methods,
    reason = "the trash's one move, between checked directories, never replacing"
)]
fn no_replace(from: &Dir, name: &Name, to: &Dir, into: &Name) -> io::Result<()> {
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

/// The name an item gets in the trash: its own, cut to leave room, then `.` and eight hex
/// digits of the digest of what moves, and `-n` after the first try. Its `.trashinfo`
/// name fits too.
fn slot_name(original: &OsStr, digest: Digest, n: u32) -> Result<Component, ApplyError> {
    let mut tag = format!(".{:08x}", digest.0 & 0xffff_ffff);
    if n > 0 {
        write!(tag, "-{n}").ok();
    }
    let bytes = original.as_bytes();
    let room = NAME_MAX - INFO_SUFFIX.len() - tag.len();
    let mut cut = bytes.len().min(room);
    // A name that is UTF-8 stays UTF-8, as macOS file systems require.
    if let Ok(text) = std::str::from_utf8(bytes) {
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
    }
    let mut name = bytes.get(..cut).unwrap_or(bytes).to_vec();
    name.extend_from_slice(tag.as_bytes());
    Component::new(OsString::from_vec(name)).map_err(|_| ApplyError::new(ErrorCode::Io))
}

/// Whether nothing has the name `name` in `files`, nor its `.trashinfo` in `info`.
fn free(opened: &Opened, name: &Component) -> Result<bool, ApplyError> {
    let absent = |dir: &Dir, entry: &Name| match dir.stat(entry) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(ApplyError::io(ErrorCode::Io, error)),
    };
    if !absent(&opened.files, &os_name(name)?)? {
        return Ok(false);
    }
    match &opened.info {
        Some(info) => {
            let described =
                info_name(name).map_err(|error| ApplyError::io(ErrorCode::Io, error))?;
            absent(info, &described)
        }
        None => Ok(true),
    }
}

/// The name of the `.trashinfo` describing the item called `slot` in `files`.
fn info_name(slot: &Component) -> io::Result<Name> {
    let mut name = slot.as_os_str().to_owned();
    name.push(INFO_SUFFIX);
    sys::name(&name)
}

/// The `.trashinfo` written for an item about to move, so it can go again if the move
/// doesn't happen. Nothing on macOS.
struct Record<'opened> {
    written: Option<(&'opened Dir, Name)>,
}

impl<'opened> Record<'opened> {
    /// Writes the `.trashinfo` for the item at `original`, moving to `slot` at `now`. Its
    /// name must be free: one already there is never replaced.
    fn write(
        opened: &'opened Opened,
        slot: &Component,
        original: &Path,
        now: SystemTime,
    ) -> Result<Self, ApplyError> {
        let Some(info) = &opened.info else {
            return if FREEDESKTOP {
                Err(ApplyError::new(ErrorCode::TrashMissing))
            } else {
                Ok(Self { written: None })
            };
        };
        let io = |error: io::Error| ApplyError::io(ErrorCode::Io, error);
        let name = info_name(slot).map_err(io)?;
        let contents = trash_info(original, now).map_err(io)?;
        let mut file = create(info, &name).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                ApplyError::io(ErrorCode::DestinationExists, error)
            } else {
                io(error)
            }
        })?;
        let record = Self {
            written: Some((info, name)),
        };
        if let Err(error) = file.write_all(&contents) {
            record.undo();
            return Err(io(error));
        }
        Ok(record)
    }

    /// Removes the `.trashinfo` written, after a move that didn't happen.
    fn undo(self) {
        if let Some((info, name)) = self.written {
            info.unlink(&name, 0).ok();
        }
    }
}

/// A new file `name` in `dir`, for writing. Fails if anything has that name.
#[expect(unsafe_code, reason = "libc's openat is a raw call")]
fn create(dir: &Dir, name: &Name) -> io::Result<File> {
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: the descriptor is open and `name` is NUL-terminated.
    let fd = unsafe { libc::openat(dir.fd(), name.as_ptr(), flags, 0o600 as libc::c_uint) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new descriptor that nothing else owns.
    Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
}

/// A freedesktop `.trashinfo`: the item's absolute path, escaped as a URI path, and when
/// it was trashed, in local time.
fn trash_info(original: &Path, now: SystemTime) -> io::Result<Vec<u8>> {
    let mut text = String::from("[Trash Info]\nPath=");
    for byte in original.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~/".contains(byte) {
            text.push(char::from(*byte));
        } else {
            write!(text, "%{byte:02X}").ok();
        }
    }
    write!(text, "\nDeletionDate={}\n", local_time(now)?).ok();
    Ok(text.into_bytes())
}

/// `now` as `YYYY-MM-DDThh:mm:ss` in local time.
#[expect(unsafe_code, reason = "libc's localtime_r is a raw call")]
fn local_time(now: SystemTime) -> io::Result<String> {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs();
    let time = libc::time_t::try_from(secs).map_err(io::Error::other)?;
    let mut tm = MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: both pointers are valid; `tm` is large enough.
    if unsafe { libc::localtime_r(&raw const time, tm.as_mut_ptr()) }.is_null() {
        return Err(io::Error::other("no local time"));
    }
    // SAFETY: `localtime_r` succeeded, so it filled `tm`.
    let tm = unsafe { tm.assume_init() };
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches a trash name too long for its `.trashinfo`, or a UTF-8 name cut inside a
    /// character, which macOS file systems refuse.
    #[test]
    fn a_long_name_is_cut_to_fit_whole_characters() {
        let original: String = "é".repeat(200);
        let name = slot_name(OsStr::new(&original), Digest(0xabcd_ef01), 3).unwrap();
        let bytes = name.as_os_str().as_bytes();
        assert!(
            bytes.len() + INFO_SUFFIX.len() <= NAME_MAX,
            "{}",
            bytes.len()
        );
        let text = std::str::from_utf8(bytes).unwrap();
        assert!(text.ends_with(".abcdef01-3"), "{text}");
    }

    /// Catches a `.trashinfo` path a file manager can't read back: bytes outside the
    /// unreserved set must be escaped.
    #[test]
    fn the_trashinfo_path_is_escaped() {
        let path = Path::new(OsStr::from_bytes(b"/home/u/a b%/\xff"));
        let text = String::from_utf8(trash_info(path, UNIX_EPOCH).unwrap()).unwrap();
        assert!(
            text.starts_with("[Trash Info]\nPath=/home/u/a%20b%25/%FF\nDeletionDate="),
            "{text}"
        );
    }
}
