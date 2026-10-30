//! The trash (spike S4, D-062): moving a reviewed item whole into the user's trash, and
//! back (`docs/04-safety-model.md` §3.5, §5).
//!
//! kipple moves the item itself, with one rename from its verified parent's handle into a
//! verified trash directory's handle that never replaces anything, after checking every
//! entry against its frozen tree with the executor's own checks. Nothing here copies,
//! and nothing deletes the item: a move the OS can't do in place fails, and the item
//! stays.
//!
//! - Linux: the freedesktop trash, kipple's own handle-relative implementation. The home
//!   trash (`SymbolicRoot::Trash`, `$XDG_DATA_HOME/Trash/files`) for items on its mount,
//!   and an existing `$topdir/.Trash/$uid` (`.Trash` sticky) or `$topdir/.Trash-$uid` for
//!   items on another. A `.trashinfo` is written first, so file managers list and restore
//!   the item too.
//! - macOS: a kipple-owned move into `~/.Trash` under the name journalled before the move.
//!   kipple restores it; Finder's Put Back doesn't, since Finder records where an item
//!   came from only for items it trashed itself.
//! - Windows: unsupported. The Recycle Bin is reached only through the shell, by path,
//!   and the shell deletes permanently when a volume has no bin, is set not to use it or
//!   the item exceeds its quota (spec pinned 9).

#[cfg(unix)]
mod unix;
#[cfg(unix)]
mod verify;

use std::path::PathBuf;
use std::sync::Arc;

use kipple_core::{
    Anchor, ApplyError, CancelToken, Clock, EntrySnapshot, RestoreRequest, SlotState, TrashBackend,
    TrashOp, TrashSlot, Trashed,
};

/// The platform's [`TrashBackend`]: whole-item moves by handle-relative, no-replace
/// renames into a trash on the item's own file system.
#[derive(Debug, Clone)]
pub struct ConfinedTrash {
    /// The user's trash, as the resolved `SymbolicRoot::Trash` names it: where trashed
    /// items go (Linux `…/Trash/files`, macOS `~/.Trash`). `None` when it didn't resolve.
    #[cfg_attr(
        windows,
        expect(dead_code, reason = "Windows has no trash kipple uses")
    )]
    home: Option<PathBuf>,
    /// When the freedesktop `.trashinfo` says an item was trashed.
    #[cfg_attr(
        windows,
        expect(dead_code, reason = "Windows has no trash kipple uses")
    )]
    clock: Arc<dyn Clock>,
}

impl ConfinedTrash {
    /// The trash backend for the user whose trash is `home` (the resolved
    /// `SymbolicRoot::Trash`), never a path taken from the process environment.
    #[must_use]
    pub fn new(home: Option<PathBuf>, clock: Arc<dyn Clock>) -> Self {
        Self { home, clock }
    }
}

#[cfg(unix)]
impl TrashBackend for ConfinedTrash {
    fn available(&self, anchor: &Anchor) -> Result<(), ApplyError> {
        unix::available(self, anchor)
    }

    fn destination(&self, op: &TrashOp) -> Result<TrashSlot, ApplyError> {
        unix::destination(self, op)
    }

    fn move_whole(
        &self,
        op: &TrashOp,
        slot: &TrashSlot,
        cancel: &CancelToken,
    ) -> Result<Trashed, ApplyError> {
        let _quiet = crate::executor::sys::NoMaterialize::new();
        unix::move_whole(self, op, slot, cancel)
    }

    fn find(&self, slot: &TrashSlot, target: &EntrySnapshot) -> Result<SlotState, ApplyError> {
        unix::find(self, slot, target)
    }

    fn restore(&self, request: &RestoreRequest, cancel: &CancelToken) -> Result<(), ApplyError> {
        unix::restore(self, request, cancel)
    }
}

/// Every operation is refused: nothing on Windows moves an item to the Recycle Bin
/// without the risk of deleting it permanently instead.
#[cfg(windows)]
impl TrashBackend for ConfinedTrash {
    fn available(&self, _: &Anchor) -> Result<(), ApplyError> {
        Err(unsupported())
    }

    fn destination(&self, _: &TrashOp) -> Result<TrashSlot, ApplyError> {
        Err(unsupported())
    }

    fn move_whole(
        &self,
        _: &TrashOp,
        _: &TrashSlot,
        _: &CancelToken,
    ) -> Result<Trashed, ApplyError> {
        Err(unsupported())
    }

    fn find(&self, _: &TrashSlot, _: &EntrySnapshot) -> Result<SlotState, ApplyError> {
        Err(unsupported())
    }

    fn restore(&self, _: &RestoreRequest, _: &CancelToken) -> Result<(), ApplyError> {
        Err(unsupported())
    }
}

#[cfg(windows)]
const fn unsupported() -> ApplyError {
    ApplyError::new(kipple_core::ErrorCode::Unsupported)
}
