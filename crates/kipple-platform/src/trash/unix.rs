//! Linux and macOS: the trash directory and the item's parent opened as handles, and one
//! rename between them that never replaces anything (Linux `renameat2(RENAME_NOREPLACE)`,
//! macOS `renameatx_np(RENAME_EXCL)`).

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::mem::MaybeUninit;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kipple_core::{
    Anchor, ApplyError, Authorized, CancelToken, Component, Digest, EntrySnapshot, ErrorCode,
    FileSystemClass, RelPath, RestoreRequest, Scope, SlotState, TrashDir, TrashOp, TrashSlot,
    Trashed, Volume,
};

mod home;

pub(super) use home::{forget_unmoved, prepare_home};

use super::ConfinedTrash;
use super::verify::verify;
use crate::executor::sys::{self, Dir, Name, no_replace};
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

/// Whether a move left the item at its destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrival {
    /// It is there.
    Arrived,
    /// It is where it was, or it can't be told.
    Stayed,
}

/// What [`shift`] did: where the item is, and how the move ended.
struct Shifted {
    arrival: Arrival,
    ended: Result<(), ApplyError>,
}

/// One rename: `name` in `from` to `into` in `to`.
struct Move<'dirs> {
    from: &'dirs Dir,
    name: &'dirs Name,
    to: &'dirs Dir,
    into: &'dirs Name,
}

/// Whether `anchor`'s item has a trash on its file system: one there now, or the home
/// trash that [`prepare_home`] would create on it.
pub(super) fn available(trash: &ConfinedTrash, anchor: &Anchor) -> Result<(), ApplyError> {
    match locate(trash, anchor) {
        Ok(_) => Ok(()),
        Err(error) => {
            let volume = reached(anchor)?.volume;
            if home::creatable(trash, volume) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
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
    let shifted = shift(&rename, &top, TRASHING);
    match shifted.ended {
        Ok(()) => Ok(Trashed { slot: slot.clone() }),
        // What moved in its place stays recorded, so the trash lists it.
        Err(error) if error.code == ErrorCode::Stranded => {
            Err(error.at(RelPath::new(vec![slot.name.clone()])))
        }
        // The item is in the trash though the move isn't durable: its record stays, so
        // the trash lists it and it can be restored.
        Err(error) if shifted.arrival == Arrival::Arrived => Err(error),
        Err(error) => match record.undo() {
            Ok(()) => Err(error),
            // A record that a power loss could bring back would list an item that never
            // moved.
            Err(undo) => Err(ApplyError::io(ErrorCode::Unsynced, undo)),
        },
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
    shift(&rename, request.target(), RESTORING)
        .ended
        .map_err(|error| {
            if error.code == ErrorCode::Stranded {
                error.at(anchor.relative.clone())
            } else {
                error
            }
        })?;
    if let (Some(info), Ok(described)) = (&opened.info, info_name(&slot.name)) {
        // The item is back; a `.trashinfo` left behind would only list it in the trash
        // without anything there.
        forget(info, &described).ok();
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
/// the rename, is moved back and the move refused; when it can't be moved back, the
/// error is [`ErrorCode::Stranded`], and the caller says where it is.
///
/// Once anything moved, both directories are synced before it returns. A sync that
/// fails makes it [`ErrorCode::Unsynced`]: a power loss could undo the move. However it
/// ends, it says whether the item arrived.
fn shift(rename: &Move<'_>, expected: &EntrySnapshot, codes: Codes) -> Shifted {
    let stayed = |ended| Shifted {
        arrival: Arrival::Stayed,
        ended,
    };
    let now = match rename.from.stat(rename.name) {
        Ok(now) => now,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return stayed(Err(ApplyError::io(codes.missing, error)));
        }
        Err(error) => return stayed(Err(ApplyError::io(ErrorCode::Io, error))),
    };
    if !same(&now.snapshot, expected) {
        return stayed(Err(ApplyError::new(codes.changed)));
    }
    if let Err(error) = no_replace(rename.from, rename.name, rename.to, rename.into) {
        return stayed(Err(refused(error, codes)));
    }
    let arrived = rename.to.stat(rename.into);
    let (arrival, moved) = if arrived.is_ok_and(|seen| same(&seen.snapshot, expected)) {
        (Arrival::Arrived, Ok(()))
    } else {
        // The rename moved what was there by then. Put it back where it was found.
        let back = match no_replace(rename.to, rename.into, rename.from, rename.name) {
            Ok(()) => Err(ApplyError::new(codes.changed)),
            Err(error) => Err(ApplyError::io(ErrorCode::Stranded, error)),
        };
        (Arrival::Stayed, back)
    };
    let synced = [rename.from, rename.to].map(Dir::sync);
    let ended = match (moved, synced) {
        (Err(error), _) if error.code == ErrorCode::Stranded => Err(error),
        (_, [Err(error), _] | [_, Err(error)]) => Err(ApplyError::io(ErrorCode::Unsynced, error)),
        (moved, _) => moved,
    };
    Shifted { arrival, ended }
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
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL;
        let mut file = info.open_file(&name, flags).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                ApplyError::io(ErrorCode::DestinationExists, error)
            } else {
                io(error)
            }
        })?;
        let record = Self {
            written: Some((info, name)),
        };
        // Durable before the move, so a crash after it leaves the item listed.
        let written = file
            .write_all(&contents)
            .and_then(|()| file.sync_all())
            .and_then(|()| info.sync());
        if let Err(error) = written {
            record.undo().ok();
            return Err(io(error));
        }
        Ok(record)
    }

    /// Removes the `.trashinfo` written, after a move that didn't happen, and syncs its
    /// directory.
    fn undo(self) -> io::Result<()> {
        match self.written {
            Some((info, name)) => forget(info, &name),
            None => Ok(()),
        }
    }
}

/// Removes the `.trashinfo` `name` from `info`, durably.
fn forget(info: &Dir, name: &Name) -> io::Result<()> {
    info.unlink(name, 0)?;
    info.sync()
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

    /// A failed sync during a move, on a disposable home trash: what the trash keeps.
    #[cfg(target_os = "linux")]
    mod unsynced {
        use std::fs;
        use std::sync::Arc;

        use kipple_core::{Clock, Freezer as _, Marks, TrashBackend as _};
        use tempfile::TempDir;

        use super::*;
        use crate::ConfinedFs;
        use crate::executor::faults::{anchor, fail_syncs_after};

        #[derive(Debug)]
        struct Epoch;

        impl Clock for Epoch {
            fn now(&self) -> SystemTime {
                UNIX_EPOCH
            }
        }

        /// A root holding `target/a`, a home trash beside it, and the move of `target`.
        struct Site {
            temp: TempDir,
            trash: ConfinedTrash,
            op: TrashOp,
        }

        impl Site {
            fn new() -> Self {
                let temp = TempDir::new().unwrap();
                let root = temp.path().join("root");
                fs::create_dir_all(root.join("target")).unwrap();
                fs::write(root.join("target/a"), b"a").unwrap();
                fs::create_dir_all(temp.path().join("Trash/files")).unwrap();
                fs::create_dir_all(temp.path().join("Trash/info")).unwrap();
                let anchor = anchor(&root, "target");
                let tree = ConfinedFs
                    .freeze(&anchor, &CancelToken::new())
                    .unwrap()
                    .tree;
                let marks = Marks::of(&tree);
                Self {
                    trash: ConfinedTrash::new(
                        Some(temp.path().join("Trash/files")),
                        Arc::new(Epoch),
                    ),
                    op: TrashOp::for_platform_tests(anchor, tree, marks),
                    temp,
                }
            }

            fn at(&self, relative: &str) -> PathBuf {
                self.temp.path().join(relative)
            }

            fn record(&self, slot: &TrashSlot) -> PathBuf {
                let mut name = slot.name.as_os_str().to_owned();
                name.push(INFO_SUFFIX);
                self.at("Trash/info").join(name)
            }
        }

        /// Catches the `.trashinfo` of an item that reached the trash removed because a
        /// sync after the rename failed: the item would sit in `files/` where no file
        /// manager lists or restores it. The move is unsynced, and the record stays.
        #[test]
        fn a_moved_item_keeps_its_record_when_a_sync_fails() {
            let site = Site::new();
            let slot = site.trash.destination(&site.op).unwrap();
            // The record's own sync passes; the first sync after the rename fails.
            let _failing = fail_syncs_after(1);

            let moved = site.trash.move_whole(&site.op, &slot, &CancelToken::new());

            assert_eq!(moved.map_err(|error| error.code), Err(ErrorCode::Unsynced));
            assert!(
                fs::symlink_metadata(slot.trash.path.join(slot.name.as_os_str())).is_ok(),
                "the item is in the trash"
            );
            assert!(
                fs::symlink_metadata(site.record(&slot)).is_ok(),
                "its record stays"
            );
        }

        /// Catches a refused move whose record removal isn't made durable: a power loss
        /// would bring the record back, listing an item that never moved. When syncing
        /// `info/` after removing it fails, the move is unsynced, not refused.
        #[test]
        fn a_refused_move_whose_record_removal_isnt_synced_is_unsynced() {
            let site = Site::new();
            let slot = site.trash.destination(&site.op).unwrap();
            // Something takes the slot after it was chosen, so the rename refuses.
            fs::write(slot.trash.path.join(slot.name.as_os_str()), b"taken").unwrap();
            let _failing = fail_syncs_after(1);

            let moved = site.trash.move_whole(&site.op, &slot, &CancelToken::new());

            assert_eq!(moved.map_err(|error| error.code), Err(ErrorCode::Unsynced));
            assert!(
                fs::symlink_metadata(site.at("root/target/a")).is_ok(),
                "the item stays"
            );
        }
    }

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
