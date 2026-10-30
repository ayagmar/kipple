//! Freezing a target: reading it and everything below it from open handles, read-only.

use std::ffi::OsString;

use kipple_core::{
    Anchor, ApplyError, CancelToken, Component, EntryKind, ErrorCode, Freeze, FrozenTree, RelPath,
    Scope, ShapeError, Slot, TreeBuilder, Volume,
};

use super::{Form, Seen, reach, sys};

/// Freezes `anchor`'s target. See [`Freezer`](kipple_core::Freezer).
pub(super) fn freeze(anchor: &Anchor, cancel: &CancelToken) -> Result<Freeze, ApplyError> {
    let reached = reach(anchor)?;
    let volume = reached.volume;
    let file_system = reached.file_system.clone();
    let Some(last) = anchor.relative.components().last() else {
        let tree = walk(
            reached.root,
            &reached.root_seen,
            Scope::Contents,
            volume,
            cancel,
        )?;
        return Ok(Freeze {
            tree,
            file_system,
            parent: None,
        });
    };
    let parent = Some(reached.holder_seen().snapshot.identity);
    let at = || anchor.relative.clone();
    let name = sys::name(last.as_os_str()).map_err(|error| io(ErrorCode::Io, error, at()))?;
    let holder = reached.holder();
    let named = holder.stat(&name).map_err(|error| {
        let code = if error.kind() == std::io::ErrorKind::NotFound {
            ErrorCode::TargetMissing
        } else {
            ErrorCode::Unreadable
        };
        io(code, error, at())
    })?;
    admissible(&named, volume).map_err(|code| ApplyError::new(code).at(at()))?;
    let tree = if named.snapshot.kind == EntryKind::Dir {
        let dir = holder
            .open_dir(&name)
            .map_err(|error| io(ErrorCode::Unreadable, error, at()))?;
        let opened = dir
            .seen()
            .map_err(|error| io(ErrorCode::Unreadable, error, at()))?;
        admissible(&opened, volume).map_err(|code| ApplyError::new(code).at(at()))?;
        walk(dir, &opened, Scope::Whole, volume, cancel)?
    } else {
        let builder =
            TreeBuilder::new(Scope::Whole, volume, &named.snapshot).map_err(|e| shape(&e))?;
        builder.finish().map_err(|e| shape(&e))?
    };
    Ok(Freeze {
        tree,
        file_system,
        parent,
    })
}

/// Whether an entry may be frozen: on the tree's volume, and not a placeholder.
fn admissible(seen: &Seen, volume: Volume) -> Result<(), ErrorCode> {
    if !seen.on(volume) {
        return Err(ErrorCode::MountBoundary);
    }
    match seen.form {
        Form::Plain => Ok(()),
        Form::Placeholder => Err(ErrorCode::UnknownEntry),
        Form::MountPoint => Err(ErrorCode::MountBoundary),
    }
}

/// A directory being read: its handle, where it goes in the tree, and the names it
/// holds that aren't read yet.
struct Frame {
    dir: sys::Dir,
    slot: Slot,
    names: std::vec::IntoIter<OsString>,
}

/// Reads the directory `top` and everything below it, depth first, so only one handle
/// per level is open at a time.
fn walk(
    top: sys::Dir,
    seen: &Seen,
    scope: Scope,
    volume: Volume,
    cancel: &CancelToken,
) -> Result<FrozenTree, ApplyError> {
    let mut builder = TreeBuilder::new(scope, volume, &seen.snapshot).map_err(|e| shape(&e))?;
    let names = top
        .names()
        .map_err(|error| io(ErrorCode::Unreadable, error, RelPath::default()))?;
    let mut stack = vec![Frame {
        dir: top,
        slot: builder.top(),
        names: names.into_iter(),
    }];
    let mut path: Vec<Component> = Vec::new();
    while let Some(frame) = stack.last_mut() {
        if cancel.is_cancelled() {
            return Err(ApplyError::new(ErrorCode::Cancelled));
        }
        let Some(name) = frame.names.next() else {
            stack.pop();
            path.pop();
            continue;
        };
        let component = Component::new(name)
            .map_err(|_| ApplyError::new(ErrorCode::UnknownEntry).at(RelPath::new(path.clone())))?;
        path.push(component.clone());
        if let Some(below) = read(frame, component, &mut builder, volume, &path)? {
            stack.push(below);
        } else {
            path.pop();
        }
    }
    builder.finish().map_err(|error| shape(&error))
}

/// Reads one entry of `frame` into `builder`. A directory comes back as the next frame,
/// with its names listed.
fn read(
    frame: &Frame,
    component: Component,
    builder: &mut TreeBuilder,
    volume: Volume,
    path: &[Component],
) -> Result<Option<Frame>, ApplyError> {
    let at = || RelPath::new(path.to_vec());
    let failed = |code| move |error| io(code, error, at());
    let name = sys::name(component.as_os_str()).map_err(failed(ErrorCode::Io))?;
    let named = frame
        .dir
        .stat(&name)
        .map_err(failed(ErrorCode::Unreadable))?;
    admissible(&named, volume).map_err(|code| ApplyError::new(code).at(at()))?;
    if named.snapshot.kind != EntryKind::Dir {
        builder
            .add(frame.slot, component, &named.snapshot)
            .map_err(|error| shape(&error).at(at()))?;
        return Ok(None);
    }
    // The directory is read from the handle that lists it, so what is recorded is what
    // was listed even if it was swapped since the name was read.
    let dir = frame
        .dir
        .open_dir(&name)
        .map_err(failed(ErrorCode::Unreadable))?;
    let seen = dir.seen().map_err(failed(ErrorCode::Unreadable))?;
    admissible(&seen, volume).map_err(|code| ApplyError::new(code).at(at()))?;
    if seen.snapshot.kind != EntryKind::Dir {
        return Err(ApplyError::new(ErrorCode::Unreadable).at(at()));
    }
    let slot = builder
        .add(frame.slot, component, &seen.snapshot)
        .map_err(|error| shape(&error).at(at()))?;
    let listed = dir.names().map_err(failed(ErrorCode::Unreadable))?;
    Ok(Some(Frame {
        dir,
        slot,
        names: listed.into_iter(),
    }))
}

fn io(code: ErrorCode, error: std::io::Error, at: RelPath) -> ApplyError {
    ApplyError::io(code, error).at(at)
}

/// The error for a tree the builder refused.
const fn shape(error: &ShapeError) -> ApplyError {
    ApplyError::new(match error {
        ShapeError::TopIsLink => ErrorCode::ThroughLink,
        ShapeError::ContentsOfFile => ErrorCode::RootChanged,
        ShapeError::OtherVolume => ErrorCode::MountBoundary,
        ShapeError::TooLarge => ErrorCode::TooLarge,
        ShapeError::NotADirectory | ShapeError::Duplicate(_) | ShapeError::UnknownParent => {
            ErrorCode::Unreadable
        }
    })
}
