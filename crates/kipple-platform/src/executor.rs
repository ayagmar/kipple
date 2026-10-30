//! The confined executor and its freezer (spike S1, D-061): the only code that removes
//! files (`docs/04-safety-model.md` §1, §3.4, §3.5; AGENTS.md 5).
//!
//! Everything here walks from open handles, one component at a time, never by a joined
//! path. A root is opened once, without following a link, and must have the identity the
//! scan recorded. Each component below it is opened relative to the directory before it,
//! without following a link or junction, and must be a directory on the root's mount.
//! Below the target the walk follows the frozen tree, never a fresh listing, so a child
//! that appeared after the freeze is never touched.
//!
//! The OS calls are direct (`libc` on Linux and macOS, `windows-sys` with the NT API on
//! Windows), not `cap-std`, whose Windows backend removes by a rebuilt path. Each OS
//! module gives the same small set of handle operations; [`freeze`] and [`delete`] are
//! written once on top of them.

mod delete;
mod freeze;
mod names;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::io;

use kipple_core::{
    Anchor, ApplyError, CancelToken, Component, DeleteOp, EffectSink, EntryKind, EntrySnapshot,
    ErrorCode, Executor, FileSystemClass, Finish, Freeze, Freezer, LeftReason, RelPath, Volume,
};

pub use names::OsNames;
#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
use windows as sys;

/// The platform's [`Freezer`] and [`Executor`]: handle-relative, no-follow, confined to a
/// verified root and its mount.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConfinedFs;

impl Freezer for ConfinedFs {
    fn freeze(&self, anchor: &Anchor, cancel: &CancelToken) -> Result<Freeze, ApplyError> {
        let _quiet = sys::NoMaterialize::new();
        freeze::freeze(anchor, cancel)
    }
}

impl Executor for ConfinedFs {
    fn delete(
        &self,
        op: &DeleteOp,
        effects: &mut dyn EffectSink,
        cancel: &CancelToken,
    ) -> Result<Finish, ApplyError> {
        let _quiet = sys::NoMaterialize::new();
        delete::delete(op, effects, cancel)
    }
}

/// One entry as read from its open handle, or by name relative to its open directory.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Seen {
    /// Its kind, identity, fingerprint and link count.
    pub(crate) snapshot: EntrySnapshot,
    /// The mount it is on, where the OS names one (Linux `statx` mount ID).
    pub(crate) mount: Option<u64>,
    /// What it allocates on disk, where the OS says.
    pub(crate) allocated: Option<u64>,
    /// Whether it is something kipple must not read or remove as a plain entry.
    pub(crate) form: Form,
}

/// What kind of thing an entry is, beyond its [`EntryKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Form {
    /// A plain file, directory or link.
    Plain,
    /// A cloud placeholder or dataless file, or another kind of reparse point that isn't a
    /// link: unknown, never read or removed.
    Placeholder,
    /// Where another volume is mounted (Windows: a mount point reparse point that names a
    /// volume).
    MountPoint,
}

impl Seen {
    /// Whether it is on `volume`: the same device and, where the OS names mounts, the same
    /// mount. A bind mount of the same device has another mount ID.
    fn on(&self, volume: Volume) -> bool {
        self.form != Form::MountPoint
            && self.snapshot.identity.volume == volume.device
            && self.mount == volume.mount
    }

    /// Its volume.
    const fn volume(&self) -> Volume {
        Volume {
            device: self.snapshot.identity.volume,
            mount: self.mount,
        }
    }
}

/// The directory holding a target, reached from its verified root.
#[derive(Debug)]
pub(crate) struct Reached {
    /// The root, opened and verified.
    pub(crate) root: sys::Dir,
    /// What the root is.
    pub(crate) root_seen: Seen,
    /// The directory holding the target, when that isn't the root.
    below: Option<(sys::Dir, Seen)>,
    /// The volume every entry must be on: the root's.
    pub(crate) volume: Volume,
    /// The file system of the directory holding the target.
    pub(crate) file_system: FileSystemClass,
}

impl Reached {
    /// The directory holding the target: the root for a target directly in it, and the
    /// root itself for a target that is the root.
    pub(crate) fn holder(&self) -> &sys::Dir {
        self.below.as_ref().map_or(&self.root, |(dir, _)| dir)
    }

    /// What [`holder`](Self::holder) is.
    pub(crate) fn holder_seen(&self) -> &Seen {
        self.below
            .as_ref()
            .map_or(&self.root_seen, |(_, seen)| seen)
    }
}

/// Opens `anchor`'s root and walks to the directory holding its target.
///
/// The root must open without following a link and have the identity the scan recorded.
/// Each component up to the target's directory must open without following a link, be a
/// directory and be on the root's mount. The target's directory must have the identity
/// the anchor records, when it records one.
pub(crate) fn reach(anchor: &Anchor) -> Result<Reached, ApplyError> {
    let root = sys::Dir::open_root(&anchor.root.path).map_err(|error| {
        let code = if sys::is_link_or_file(&error) {
            ErrorCode::RootChanged
        } else {
            ErrorCode::RootUnreadable
        };
        ApplyError::io(code, error)
    })?;
    let root_seen = root
        .seen()
        .map_err(|error| ApplyError::io(ErrorCode::RootUnreadable, error))?;
    if root_seen.snapshot.kind != EntryKind::Dir
        || root_seen.form != Form::Plain
        || root_seen.snapshot.identity != anchor.root.identity
    {
        return Err(ApplyError::new(ErrorCode::RootChanged));
    }
    let volume = root_seen.volume();
    let way = anchor
        .relative
        .components()
        .split_last()
        .map_or(&[][..], |(_, way)| way);
    let below = walk_down(&root, way, volume)?;
    let (holder, holder_seen) = below
        .as_ref()
        .map_or((&root, &root_seen), |(dir, seen)| (dir, seen));
    if !anchor.relative.is_empty()
        && let Some(expected) = anchor.parent
        && holder_seen.snapshot.identity != expected
    {
        return Err(ApplyError::new(ErrorCode::ParentChanged));
    }
    let file_system = holder
        .file_system()
        .map_err(|error| ApplyError::io(ErrorCode::Io, error))?;
    Ok(Reached {
        root,
        root_seen,
        below,
        volume,
        file_system,
    })
}

/// Opens each of `way` in turn below `root`: each a real directory on `volume`. `None`
/// when `way` is empty.
fn walk_down(
    root: &sys::Dir,
    way: &[Component],
    volume: Volume,
) -> Result<Option<(sys::Dir, Seen)>, ApplyError> {
    let mut dir: Option<(sys::Dir, Seen)> = None;
    for (depth, component) in way.iter().enumerate() {
        let at = || RelPath::new(way.iter().take(depth + 1).cloned().collect());
        let name = sys::name(component.as_os_str())
            .map_err(|error| ApplyError::io(ErrorCode::Io, error).at(at()))?;
        let holder = dir.as_ref().map_or(root, |(dir, _)| dir);
        let next = holder.open_dir(&name).map_err(|error| {
            let code = if sys::is_link_or_file(&error) {
                ErrorCode::ThroughLink
            } else {
                ErrorCode::ParentMissing
            };
            ApplyError::io(code, error).at(at())
        })?;
        let seen = next
            .seen()
            .map_err(|error| ApplyError::io(ErrorCode::Io, error).at(at()))?;
        if !seen.on(volume) {
            return Err(ApplyError::new(ErrorCode::MountBoundary).at(at()));
        }
        if seen.snapshot.kind != EntryKind::Dir || seen.form != Form::Plain {
            return Err(ApplyError::new(ErrorCode::ThroughLink).at(at()));
        }
        dir = Some((next, seen));
    }
    Ok(dir)
}

/// Why the OS refused to remove or open one entry, as a receipt counts it.
pub(crate) fn left_reason(error: &io::Error) -> LeftReason {
    let kind = error.kind();
    if sys::is_busy(error) {
        LeftReason::Busy
    } else if kind == io::ErrorKind::NotFound {
        LeftReason::Gone
    } else if kind == io::ErrorKind::PermissionDenied {
        LeftReason::Denied
    } else if kind == io::ErrorKind::DirectoryNotEmpty {
        LeftReason::NotEmpty
    } else {
        LeftReason::Failed
    }
}
