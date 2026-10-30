//! Checking an item against its frozen tree before it moves, with the executor's checks:
//! each entry read from its open directory without following a link, on the tree's
//! volume, not a placeholder, and unchanged (`EntrySnapshot::drift`; a directory's
//! fingerprint compared when it is opened, amendment B).

use std::io;

use kipple_core::{ApplyError, CancelToken, Entry, EntryKind, ErrorCode, FrozenTree, Volume};

use crate::executor::sys::{self, Dir, Name};
use crate::executor::{Form, Seen, drift};

/// Checks every entry of `tree`, the item called `name` in `holder`. Any entry that
/// differs, is gone, or can't be read refuses the whole move.
pub(super) fn verify(
    holder: &Dir,
    name: &Name,
    tree: &FrozenTree,
    volume: Volume,
    cancel: &CancelToken,
) -> Result<(), ApplyError> {
    let top = tree.top();
    if top.kind() != EntryKind::Dir {
        let seen = holder.stat(name).map_err(|error| failed(top, error))?;
        return check(top, &seen, volume);
    }
    let mut stack = vec![(enter(holder, name, top, volume)?, top.children())];
    while let Some((dir, children)) = stack.last_mut() {
        if cancel.is_cancelled() {
            return Err(ApplyError::new(ErrorCode::Cancelled));
        }
        let Some(child) = children.next() else {
            stack.pop();
            continue;
        };
        let child_name = child
            .name()
            .and_then(|component| sys::name(component.as_os_str()).ok())
            .ok_or_else(|| ApplyError::new(ErrorCode::Io).at(child.path()))?;
        if child.kind() == EntryKind::Dir {
            let below = enter(dir, &child_name, child, volume)?;
            stack.push((below, child.children()));
        } else {
            let seen = dir
                .stat(&child_name)
                .map_err(|error| failed(child, error))?;
            check(child, &seen, volume)?;
        }
    }
    Ok(())
}

/// Opens the frozen directory `entry`, called `name` in `dir`, and checks it.
fn enter(dir: &Dir, name: &Name, entry: Entry<'_>, volume: Volume) -> Result<Dir, ApplyError> {
    let opened = dir.open_dir(name).map_err(|error| failed(entry, error))?;
    let seen = opened.seen().map_err(|error| failed(entry, error))?;
    check(entry, &seen, volume)?;
    Ok(opened)
}

/// Whether `seen` is still the frozen `entry`, on `volume`, and a plain entry.
fn check(entry: Entry<'_>, seen: &Seen, volume: Volume) -> Result<(), ApplyError> {
    let code = if !seen.on(volume) {
        ErrorCode::MountBoundary
    } else if seen.form != Form::Plain {
        ErrorCode::UnknownEntry
    } else if drift(entry, seen).is_some() {
        ErrorCode::TargetChanged
    } else {
        return Ok(());
    };
    Err(ApplyError::new(code).at(entry.path()))
}

/// Reading `entry` failed: it is unreadable, or something else is there now.
fn failed(entry: Entry<'_>, error: io::Error) -> ApplyError {
    let code = if error.kind() == io::ErrorKind::PermissionDenied {
        ErrorCode::Unreadable
    } else {
        ErrorCode::TargetChanged
    };
    ApplyError::io(code, error).at(entry.path())
}
