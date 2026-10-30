//! The freedesktop home trash before and after a move (spec M3b pinned 11): created where
//! it is missing, before the first trash intent, and the `.trashinfo` of a move a crash
//! kept from happening removed again. macOS has neither: `~/.Trash` is never created and
//! a move there writes no record.

use std::ffi::OsStr;
use std::io::{self, Read as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use kipple_core::{Anchor, ApplyError, ErrorCode, Forgotten, TrashSlot, Volume};

use super::{
    ConfinedTrash, Dir, FREEDESKTOP, info_name, open, open_in, open_path, opening, owned, sys, uid,
};

/// The largest `.trashinfo` read: kipple's own hold a path and a date.
const RECORD_LIMIT: u64 = 64 * 1024;

/// The home trash's place: the directory above it, which is never created, and its own
/// name there, with `files` and `info` inside.
struct Layout<'home> {
    base: &'home Path,
    trash: &'home OsStr,
}

impl<'home> Layout<'home> {
    /// From the resolved `SymbolicRoot::Trash`, which names its `files`.
    fn of(trash: &'home ConfinedTrash) -> Result<Self, ApplyError> {
        let home = trash
            .home
            .as_deref()
            .ok_or_else(|| ApplyError::new(ErrorCode::TrashMissing))?;
        let top = home
            .parent()
            .filter(|_| home.file_name() == Some(OsStr::new("files")));
        match top.and_then(|top| Some((top.parent()?, top.file_name()?))) {
            Some((base, name)) => Ok(Self { base, trash: name }),
            None => Err(ApplyError::new(ErrorCode::Unsupported)),
        }
    }
}

/// Creates what is missing of the home trash, its directory and `files` and `info` in it,
/// each for the user only, from the directory above it, which must exist and be the
/// user's. Each new directory is synced into the one holding it.
pub(in crate::trash) fn prepare_home(trash: &ConfinedTrash) -> Result<(), ApplyError> {
    if !FREEDESKTOP {
        return Ok(());
    }
    let layout = Layout::of(trash)?;
    let base = open_path(layout.base)?;
    owned(&base)?;
    let top = ensure(&base, layout.trash)?;
    ensure(&top, OsStr::new("files"))?;
    ensure(&top, OsStr::new("info"))?;
    Ok(())
}

/// The directory `name` in `dir`, created first if nothing has that name: never through a
/// link, and the user's own.
fn ensure(dir: &Dir, name: &OsStr) -> Result<Dir, ApplyError> {
    let name = sys::name(name).map_err(|error| ApplyError::io(ErrorCode::Io, error))?;
    match dir.create_dir(&name) {
        Ok(()) => dir
            .sync()
            .map_err(|error| ApplyError::io(ErrorCode::Unsynced, error))?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(ApplyError::io(ErrorCode::Io, error)),
    }
    let opened = dir.open_dir(&name).map_err(opening)?;
    owned(&opened)?;
    Ok(opened)
}

/// Whether [`prepare_home`] would make a home trash on `volume`: part of it is missing,
/// the directory above it is the user's and on `volume`, and whatever of it exists is the
/// user's own directories there.
pub(super) fn creatable(trash: &ConfinedTrash, volume: Volume) -> bool {
    let Ok(layout) = Layout::of(trash) else {
        return false;
    };
    let Ok(base) = open_path(layout.base) else {
        return false;
    };
    if !usable(&base, volume) {
        return false;
    }
    let top = match present(&base, layout.trash, volume) {
        Presence::Usable(top) => top,
        Presence::Missing => return FREEDESKTOP,
        Presence::Unusable => return false,
    };
    let inside = ["files", "info"].map(|name| present(&top, OsStr::new(name), volume));
    FREEDESKTOP
        && inside
            .iter()
            .all(|found| !matches!(found, Presence::Unusable))
        && inside
            .iter()
            .any(|found| matches!(found, Presence::Missing))
}

/// What one directory of the home trash is.
enum Presence {
    Usable(Dir),
    Missing,
    Unusable,
}

fn present(dir: &Dir, name: &OsStr, volume: Volume) -> Presence {
    let Some(name) = name.to_str() else {
        return Presence::Unusable;
    };
    match open_in(dir, name) {
        Ok(found) if usable(&found, volume) => Presence::Usable(found),
        Err(error) if error.code == ErrorCode::TrashMissing => Presence::Missing,
        Ok(_) | Err(_) => Presence::Unusable,
    }
}

fn usable(dir: &Dir, volume: Volume) -> bool {
    owned(dir).is_ok() && dir.seen().is_ok_and(|seen| seen.on(volume))
}

/// Removes the `.trashinfo` a move wrote for `slot` before a crash kept the item at
/// `original` from moving, when it is a regular file of the user's, reached without a
/// link, in the trash directory the slot names, recording exactly `original`'s path.
pub(in crate::trash) fn forget_unmoved(
    trash: &ConfinedTrash,
    slot: &TrashSlot,
    original: &Anchor,
) -> Result<Forgotten, ApplyError> {
    if !FREEDESKTOP {
        return Ok(Forgotten::Absent);
    }
    let opened = match open(trash, &slot.trash.path) {
        Ok(opened) => opened,
        Err(error) if error.code == ErrorCode::TrashMissing => return Ok(Forgotten::Absent),
        Err(error) => return Err(error),
    };
    let Some(info) = opened
        .info
        .filter(|_| opened.dir.identity == slot.trash.identity)
    else {
        return Ok(Forgotten::Absent);
    };
    let io = |error: io::Error| ApplyError::io(ErrorCode::Io, error);
    let name = info_name(&slot.name).map_err(io)?;
    let path = original.path();
    if !records(&info, &name, path.as_os_str().as_bytes()).map_err(io)? {
        return Ok(Forgotten::Absent);
    }
    info.unlink(&name, 0).map_err(io)?;
    info.sync()
        .map_err(|error| ApplyError::io(ErrorCode::Unsynced, error))?;
    Ok(Forgotten::Removed)
}

/// Whether `name` in `info` is a regular file of the user's, not a link, recording the
/// path `original`, and still the same file by that name after it was read.
fn records(info: &Dir, name: &sys::Name, original: &[u8]) -> io::Result<bool> {
    let file = match info.open_file(name, libc::O_RDONLY | libc::O_NONBLOCK) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        // A link (`ELOOP`) is left alone.
        Err(error) if sys::is_link_or_file(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != uid() {
        return Ok(false);
    }
    let mut text = Vec::new();
    file.take(RECORD_LIMIT).read_to_end(&mut text)?;
    if recorded_path(&text).as_deref() != Some(original) {
        return Ok(false);
    }
    let now = info.stat(name)?.snapshot.identity;
    Ok(now.volume == meta.dev() && now.file == u128::from(meta.ino()))
}

/// The path a `.trashinfo` records, unescaped: its one `Path=` key in its
/// `[Trash Info]` group. `None` when it has none, more than one, or a bad escape.
fn recorded_path(text: &[u8]) -> Option<Vec<u8>> {
    let mut lines = text.split(|byte| *byte == b'\n');
    if lines.next()? != b"[Trash Info]" {
        return None;
    }
    let mut paths = lines
        .take_while(|line| !line.starts_with(b"["))
        .filter_map(|line| line.strip_prefix(b"Path="));
    let path = paths.next()?;
    if paths.next().is_some() {
        return None;
    }
    unescape(path)
}

/// `escaped` with each `%XX` turned back into its byte.
fn unescape(escaped: &[u8]) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(escaped.len());
    let mut rest = escaped.iter();
    while let Some(byte) = rest.next() {
        if *byte == b'%' {
            let pair = [*rest.next()?, *rest.next()?];
            let hex = std::str::from_utf8(&pair).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            bytes.push(*byte);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::super::trash_info;
    use super::*;

    /// Catches a record kipple wrote that it then can't match to the item, which would
    /// leave every unmoved item listed in the trash, or one matched by a prefix.
    #[test]
    fn a_written_record_names_exactly_its_path() {
        let path = Path::new(OsStr::from_bytes(b"/home/u/a b%/\xff"));
        let text = trash_info(path, UNIX_EPOCH).unwrap();

        assert_eq!(
            recorded_path(&text).as_deref(),
            Some(path.as_os_str().as_bytes())
        );
        let two = b"[Trash Info]\nPath=/a\nPath=/b\n";
        assert_eq!(recorded_path(two), None, "two paths name no one item");
        assert_eq!(recorded_path(b"[Other]\nPath=/a\n"), None);
        assert_eq!(recorded_path(b"[Trash Info]\nPath=/a%2\n"), None);
    }
}
