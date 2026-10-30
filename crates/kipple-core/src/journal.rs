//! The journal port: intent before effect, outcome after (`docs/04-safety-model.md` §7).
//! Implemented by `kipple-platform`'s store.
//!
//! The plan, with its frozen trees, is stored durably first ([`Journal::plan`]). Then,
//! per item, one [`Intent`] is made durable before the first entry is touched. It names
//! every entry the run may change, because it names the item's tree in the stored plan
//! and the digest of what revalidation left of it, so an intent per entry would add
//! nothing but a sync per file (D-063). After a crash, reconciliation compares a fresh
//! freeze with the stored tree: entries now gone are recorded as gone with their cause
//! indeterminate, never as removed by kipple, and nothing is ever run again.
//!
//! Every item that ends gets an outcome record, a skip included, so a receipt computed
//! from the stored plan and its records says what the live one said. Restores append to
//! the run they restore from (spec M3b pinned 3, 6, 12).

use std::fmt;

use crate::digest::Digest;
use crate::outcome::{ApplyError, Found, ItemOutcome};
use crate::plan::{ItemId, Plan};
use crate::rule::Method;
use crate::trash::TrashSlot;

/// What an item's run is about to do, recorded before it does anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// The stored plan.
    pub plan: Digest,
    /// The item.
    pub item: ItemId,
    /// How it is removed.
    pub method: Method,
    /// Its tree as stored with the plan ([`FrozenTree::digest`]).
    ///
    /// [`FrozenTree::digest`]: crate::FrozenTree::digest
    pub reviewed: Digest,
    /// That tree with what revalidation left of it ([`FrozenTree::digest_with`]).
    ///
    /// [`FrozenTree::digest_with`]: crate::FrozenTree::digest_with
    pub authorized: Digest,
    /// How many entries the run may remove.
    pub entries: u64,
    /// For a trash item, where it will go, chosen before the move.
    pub trash: Option<TrashSlot>,
}

/// One durable record of a run, in the order it was written (`kipple.journal/v1`).
///
/// The receipt of a run is computed from its stored plan and these records, never stored
/// itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// The run's plan was stored, with this digest as it was written. Always first.
    Plan {
        /// [`Plan::digest`] when it was stored. Compared as stored, never recomputed.
        digest: Digest,
    },
    /// An item's run is about to change something.
    Intent(Intent),
    /// How an item ended. Every item that ends gets one, skips included.
    Outcome {
        /// The item.
        item: ItemId,
        /// How it ended.
        outcome: ItemOutcome,
    },
    /// A restore is about to move a trashed item back.
    RestoreIntent {
        /// The item.
        item: ItemId,
        /// Where it is in the trash.
        slot: TrashSlot,
    },
    /// How a restore ended.
    RestoreOutcome {
        /// The item.
        item: ItemId,
        /// How it ended.
        result: RestoreResult,
    },
    /// Reconciliation removed the record a trash move wrote before a crash kept the item
    /// from moving (the freedesktop `.trashinfo`), so no file manager lists a slot that
    /// is empty.
    TrashRecordRemoved {
        /// The item.
        item: ItemId,
        /// The slot the record named.
        slot: TrashSlot,
    },
}

/// How a restore of a trashed item ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreResult {
    /// The item is back at its place.
    Restored,
    /// The restore was refused or failed; with [`ErrorCode::Stranded`], something swapped
    /// in for the trashed entry was moved to the original name and couldn't be moved
    /// back.
    ///
    /// [`ErrorCode::Stranded`]: crate::ErrorCode::Stranded
    Failed(ApplyError),
    /// A crash interrupted it, and reconciliation found this.
    Indeterminate(Found),
}

/// Records an apply durably.
///
/// Each call returns only once what it records is durable (written and synced). The
/// apply loop holds to this order and stops at the first error: a plan that can't be
/// stored runs nothing; an item whose intent can't be recorded isn't run, and nothing
/// after it is; an outcome that can't be recorded stops the apply, and an item whose
/// intent was recorded is reconciled as indeterminate on the next start.
pub trait Journal: fmt::Debug {
    /// Stores `plan` with its trees, then [`Record::Plan`], before any intent.
    ///
    /// # Errors
    /// When it isn't durable.
    fn plan(&mut self, plan: &Plan) -> Result<(), ApplyError>;

    /// Appends `record`: an intent before the item's first mutation, an outcome after its
    /// run or its skip.
    ///
    /// # Errors
    /// When it isn't durable.
    fn record(&mut self, record: &Record) -> Result<(), ApplyError>;
}
