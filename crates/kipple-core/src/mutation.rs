//! The ports that change the filesystem, implemented only by `kipple-platform`
//! (`docs/04-safety-model.md` §1, AGENTS.md 5) and faked in tests.
//!
//! [`Freezer`] reads; [`Executor`] deletes. Both walk handle-relative from a verified
//! root, never follow a link or junction, never cross a mount and never let the OS
//! download a cloud placeholder (04 §3.1, §3.4). Neither decides anything: the executor
//! removes only what an [`DeleteOp`] authorizes, checks each entry on its open handle
//! first, and reports what happened to each one through an [`EffectSink`]; the core turns
//! that into the item's outcome.

use std::fmt;

use crate::authorize::DeleteOp;
use crate::cancel::CancelToken;
use crate::frozen::{FrozenTree, NodeId};
use crate::outcome::{ApplyError, Freed, LeftReason};
use crate::plan::Anchor;
use crate::probe::FileIdentity;

named! {
    /// A local file system kipple changes. Each OS has its own: Linux ext4, XFS, btrfs,
    /// tmpfs and F2FS; macOS APFS and HFS+; Windows NTFS and `ReFS`.
    LocalFileSystem {
        /// Linux ext4.
        Ext4 = "ext4",
        /// Linux XFS.
        Xfs = "xfs",
        /// Linux btrfs.
        Btrfs = "btrfs",
        /// Linux tmpfs.
        Tmpfs = "tmpfs",
        /// Linux F2FS.
        F2fs = "f2fs",
        /// macOS APFS.
        Apfs = "apfs",
        /// macOS HFS+.
        HfsPlus = "hfs+",
        /// Windows NTFS.
        Ntfs = "ntfs",
        /// Windows `ReFS`, Dev Drive's file system.
        Refs = "refs",
    }
}

/// Why kipple doesn't change a file system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// A network file system (NFS, SMB, AFP...), where another machine decides.
    Network,
    /// A user-space file system (FUSE, macFUSE, `WinFsp`), where a program decides.
    UserSpace,
    /// One without POSIX delete semantics, such as FAT or exFAT.
    NoPosixDelete,
    /// One kipple doesn't recognise, with what the OS called it.
    Unrecognised(String),
}

/// What the file system under a root or target is, classified from an open handle.
///
/// Anything not on the allowlist is unsupported: items there are report-only before
/// review, and the executor refuses them again at apply (04 §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSystemClass {
    /// On the allowlist.
    Supported(LocalFileSystem),
    /// Not on it.
    Unsupported(Unsupported),
}

/// What freezing a target found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freeze {
    /// The target and everything below it.
    pub tree: FrozenTree,
    /// The file system it is on.
    pub file_system: FileSystemClass,
    /// The identity of the directory holding the target, which a trash move leaves from
    /// and a restore returns to. `None` when the target is the root.
    pub parent: Option<FileIdentity>,
}

/// Freezes a target: reads it and everything below it, entry by entry, from its open
/// handles. Read-only.
///
/// It opens the anchor's root without following a link and refuses it unless it has the
/// identity the scan recorded; walks the anchor's components handle-relative, each a real
/// directory on the root's mount, and refuses one that isn't; checks the parent's
/// identity when the anchor has one; and reads every entry below with its identity,
/// fingerprint and link count. A root target freezes as [`Scope::Contents`], anything else
/// as [`Scope::Whole`].
///
/// Unknown is never frozen as something known: a directory it can't list, an entry on
/// another mount (by mount identity where the OS has one, so a bind mount of the same
/// device is refused), a cloud placeholder or dataless file, or an entry of a kind it
/// can't judge fails the whole freeze. Materialization of dataless files is off while it
/// walks, so freezing never downloads anything.
///
/// [`Scope::Contents`]: crate::Scope::Contents
/// [`Scope::Whole`]: crate::Scope::Whole
pub trait Freezer: fmt::Debug + Send + Sync {
    /// The frozen tree at `anchor`. Stops at the next entry once `cancel` is set.
    ///
    /// # Errors
    /// When the root, the way to the target or anything below it isn't what it must be,
    /// can't be read, or the freeze was cancelled.
    fn freeze(&self, anchor: &Anchor, cancel: &CancelToken) -> Result<Freeze, ApplyError>;
}

/// How a run ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    /// Every authorized entry was reported.
    Completed,
    /// It stopped at the next entry after `cancel` was set.
    Cancelled,
}

/// Where an executor reports what happened to each entry it may touch, once per entry.
/// A report about any other entry is a fault: [`Tally`](crate::Tally) fails the item.
pub trait EffectSink {
    /// `id` was removed, freeing `freed`.
    fn removed(&mut self, id: NodeId, freed: Freed);

    /// `id` was left, for `reason`.
    fn left(&mut self, id: NodeId, reason: LeftReason);
}

/// Deletes what a [`DeleteOp`] authorizes. The only code that removes files.
///
/// It opens the root and the way to the target as [`Freezer`] does, checks the file
/// system is supported, then walks the tree from its open handles:
///
/// - Each entry is opened by name relative to its directory's handle, without following
///   a link, and checked with [`EntrySnapshot::drift`] against its frozen snapshot before
///   anything of this run touches it. One that differs is left as `replaced` or
///   `changed`.
/// - A directory is checked once, when the walk first opens it, before anything below it
///   is touched. One that differs is left with everything inside it, each entry counted
///   under its reason. Its final removal checks its identity, then that it is empty,
///   never its fingerprint: removing its children changed that. One still holding
///   entries is left as `not-empty`.
/// - Only entries the operation authorizes are removed. A link is removed as a name; its
///   target is never touched.
/// - The link count and allocation are read from the open handle just before removing:
///   a file with another name frees nothing ([`Freed::Shared`]).
/// - Removal happens on the handle that was checked where the OS allows (Windows
///   `FileDispositionInfoEx` with POSIX semantics), or by name relative to the checked
///   directory's handle. A read-only attribute is never cleared as a separate step.
/// - An OS refusal for one entry (permission, a lock) leaves that entry and goes on. A
///   failure that makes the rest unknown (the root or the target changed, a mount, an
///   unsupported file system) stops the run with an error.
/// - `cancel` is checked between entries.
///
/// [`EntrySnapshot::drift`]: crate::EntrySnapshot::drift
pub trait Executor: fmt::Debug + Send + Sync {
    /// Runs `op`, reporting each entry to `effects`.
    ///
    /// # Errors
    /// When the run had to stop: what it reported until then stands.
    fn delete(
        &self,
        op: &DeleteOp,
        effects: &mut dyn EffectSink,
        cancel: &CancelToken,
    ) -> Result<Finish, ApplyError>;
}
