//! The trash port: moving a reviewed item whole into the trash, and back
//! (`docs/04-safety-model.md` §3.5, §5). Implemented only by `kipple-platform`.
//!
//! A trash move is the one whole-directory operation (04 §3.5): the item and whatever is
//! in it at apply time move together, recoverably. It is allowed only for an item whose
//! every entry is still authorized ([`TrashOp`]), and it never falls back to deleting:
//! a [`TrashOp`] can't become a [`DeleteOp`](crate::DeleteOp).

use std::fmt;
use std::path::PathBuf;

use crate::authorize::TrashOp;
use crate::cancel::CancelToken;
use crate::frozen::{Component, EntrySnapshot};
use crate::outcome::{ApplyError, Trashed};
use crate::plan::Anchor;
use crate::probe::FileIdentity;

/// A trash directory, from the resolved roots, never from the process environment.
///
/// It is [`SymbolicRoot::Trash`], or a volume's `$topdir/.Trash/$uid` or
/// `$topdir/.Trash-$uid`, checked as the freedesktop specification says: sticky, not a
/// link, owned by the user.
///
/// [`SymbolicRoot::Trash`]: crate::SymbolicRoot::Trash
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrashDir {
    /// Where its trashed items go (freedesktop `files/`, macOS `~/.Trash`).
    pub path: PathBuf,
    /// Its identity, checked before each move into or out of it.
    pub identity: FileIdentity,
}

/// Where one item goes in the trash: chosen before the move and journalled with its
/// intent, so a crash between the move and its outcome can still find the item.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrashSlot {
    /// The trash directory, on the item's file system.
    pub trash: TrashDir,
    /// The item's name there.
    pub name: Component,
}

named! {
    /// What is at a trash slot now.
    SlotState {
        /// Nothing: the trash was emptied, the entry removed, or the move never happened.
        Empty = "empty",
        /// The item that was moved there, by identity.
        Holds = "holds",
        /// Something else.
        Other = "other",
    }
}

named! {
    /// What [`TrashBackend::forget_unmoved`] did with the record of an empty slot.
    Forgotten {
        /// It removed the record.
        Removed = "removed",
        /// There was no record for the item there: nothing to remove.
        Absent = "absent",
    }
}

/// What restoring a trashed item needs: where it is, where it came from, and the
/// identity of the directory it came from, which restore checks (pinned 10). It can't be
/// made without that identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRequest {
    slot: TrashSlot,
    anchor: Anchor,
    parent: FileIdentity,
    target: EntrySnapshot,
}

impl RestoreRequest {
    /// A request to move the item at `slot`, whose top was `target` when frozen, back to
    /// `anchor`. `None` when the anchor doesn't record its directory's identity: only a
    /// root has none, and a root is never moved to the trash.
    #[must_use]
    pub fn new(slot: TrashSlot, anchor: Anchor, target: EntrySnapshot) -> Option<Self> {
        let parent = anchor.parent?;
        Some(Self {
            slot,
            anchor,
            parent,
            target,
        })
    }

    /// Where it is in the trash.
    #[must_use]
    pub const fn slot(&self) -> &TrashSlot {
        &self.slot
    }

    /// Where it came from.
    #[must_use]
    pub const fn anchor(&self) -> &Anchor {
        &self.anchor
    }

    /// The identity the directory it came from must still have.
    #[must_use]
    pub const fn parent(&self) -> FileIdentity {
        self.parent
    }

    /// The item's top entry as frozen, which a move keeps.
    #[must_use]
    pub const fn target(&self) -> &EntrySnapshot {
        &self.target
    }
}

/// Moves items whole into the trash and back.
///
/// Every move is a rename on one file system, relative to verified directory handles,
/// that never replaces anything: the item leaves its verified parent and arrives in a
/// verified trash directory, or nothing happens. A move the OS can't do in place, or a
/// backend that could turn it into a permanent delete (the Windows Recycle Bin can, when a
/// volume has no bin, is set not to use it, or the item exceeds its quota), is refused:
/// never copied and deleted. Such a backend reports [`ErrorCode::Unsupported`], and the
/// item is report-only for `trash`.
///
/// [`ErrorCode::Unsupported`]: crate::ErrorCode::Unsupported
///
/// **Durable before returning.** On every path that moved something, `Ok`, `Err` or
/// cancelled, the move is synced before the call returns: the trash record (Linux's
/// `.trashinfo`) and its directory before the rename, and the item's old directory and
/// the trash directory after it, each with `fsync` (`F_FULLFSYNC` on macOS). A record
/// whose move didn't happen is removed and its directory synced; the record of an item
/// that reached the trash stays, even when a sync after the move fails. The journal
/// records the outcome only after that. A sync that fails makes the call fail with
/// [`ErrorCode::Unsynced`], never succeed, and the item's outcome is `indeterminate`:
/// never done for a move a power loss could undo. The Windows backend moves nothing
/// (D-062).
///
/// [`ErrorCode::Unsynced`]: crate::ErrorCode::Unsynced
pub trait TrashBackend: fmt::Debug + Send + Sync {
    /// Whether an item at `anchor` could be moved to a trash. Read-only: a home trash
    /// that doesn't exist yet, under the resolved [`SymbolicRoot::Trash`] whose parent
    /// exists, is available, since apply creates it ([`prepare_home`]).
    ///
    /// [`SymbolicRoot::Trash`]: crate::SymbolicRoot::Trash
    /// [`prepare_home`]: Self::prepare_home
    ///
    /// # Errors
    /// When there is no usable trash on its file system, or this backend can't move it
    /// without risking a permanent delete.
    fn available(&self, anchor: &Anchor) -> Result<(), ApplyError>;

    /// Creates the home trash's directories (freedesktop `files/` and `info/`, mode
    /// 0700) where they are missing, handle-relative from the directory holding the
    /// resolved [`SymbolicRoot::Trash`], and syncs them. Apply calls it once, under the
    /// lock and after the user confirmed, before the first trash intent; a dry run or a
    /// plan written to a file never does. It never creates that parent (such as
    /// `~/.local/share`), and never a top directory's trash or macOS `~/.Trash`.
    ///
    /// [`SymbolicRoot::Trash`]: crate::SymbolicRoot::Trash
    ///
    /// # Errors
    /// When the parent is missing or isn't the user's own directory, or the directories
    /// can't be created: trash items then skip as unavailable.
    fn prepare_home(&self) -> Result<(), ApplyError>;

    /// The slot `op` will move to. Read-only: it touches neither the item nor the trash,
    /// so the intent naming it is journalled before anything changes.
    ///
    /// # Errors
    /// As [`available`](Self::available).
    fn destination(&self, op: &TrashOp) -> Result<TrashSlot, ApplyError>;

    /// Checks every entry of `op`'s tree on its open handle, with the executor's own
    /// checks, then moves the item whole to `slot` and writes what the trash records to
    /// restore it (the freedesktop `.trashinfo`). Any entry that differs refuses the
    /// whole move. Entries that appear after the check move with it.
    ///
    /// # Errors
    /// When the move was refused or failed. Nothing moved then, except with
    /// [`ErrorCode::Stranded`](crate::ErrorCode::Stranded): something swapped in for the
    /// item moved and couldn't be moved back, and the trash records it at `slot`.
    fn move_whole(
        &self,
        op: &TrashOp,
        slot: &TrashSlot,
        cancel: &CancelToken,
    ) -> Result<Trashed, ApplyError>;

    /// What is at `slot` now, compared with `target`, the item's top as frozen. For
    /// reconciliation after a crash: it never moves anything.
    ///
    /// # Errors
    /// When the slot can't be checked.
    fn find(&self, slot: &TrashSlot, target: &EntrySnapshot) -> Result<SlotState, ApplyError>;

    /// Removes the record a move wrote for `slot` before a crash kept the item from
    /// moving, so no file manager lists an empty slot. Reconciliation calls it only when
    /// [`find`](Self::find) says the slot is [`SlotState::Empty`] and a fresh freeze finds
    /// the item unchanged at `original`, and journals what it did.
    ///
    /// The record is removed only if it is a regular file, not followed through a link,
    /// owned by the user, in the trash's verified record directory, naming exactly the
    /// path of `original`. Anything else is left alone.
    ///
    /// # Errors
    /// When the record couldn't be checked or removed.
    fn forget_unmoved(&self, slot: &TrashSlot, original: &Anchor) -> Result<Forgotten, ApplyError>;

    /// Moves a trashed item back. Opens the original parent from its verified root
    /// without following links, checks its recorded identity, checks the trashed entry is
    /// the item, and renames it back without replacing anything (Linux
    /// `renameat2(RENAME_NOREPLACE)`, macOS `renameatx_np(RENAME_EXCL)`, Windows
    /// `FileRenameInfo` with `ReplaceIfExists` off, relative to the parent's handle).
    /// A missing or replaced parent, or anything at the original name, refuses: the
    /// parent is never created and nothing is overwritten.
    ///
    /// # Errors
    /// When the restore was refused or failed. Nothing moved then, except with
    /// [`ErrorCode::Stranded`](crate::ErrorCode::Stranded): something swapped in for the
    /// trashed entry moved and couldn't be moved back, and is at the original name.
    fn restore(&self, request: &RestoreRequest, cancel: &CancelToken) -> Result<(), ApplyError>;
}
