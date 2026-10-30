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

use std::fmt;

use crate::digest::Digest;
use crate::outcome::{ApplyError, ItemOutcome};
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

/// Records an apply durably.
///
/// Each call returns only once what it records is durable (written and synced). The
/// apply loop holds to this order and stops at the first error: a plan that can't be
/// stored runs nothing; an item whose intent can't be recorded isn't run, and nothing
/// after it is; an outcome that can't be recorded stops the apply, and the item is
/// reconciled as indeterminate on the next start.
pub trait Journal: fmt::Debug + Send {
    /// Stores `plan` with its trees, before any intent.
    ///
    /// # Errors
    /// When it isn't durable.
    fn plan(&mut self, plan: &Plan) -> Result<(), ApplyError>;

    /// Records `intent`, before the item's first mutation.
    ///
    /// # Errors
    /// When it isn't durable.
    fn intent(&mut self, intent: &Intent) -> Result<(), ApplyError>;

    /// Records how `item` ended, after its run.
    ///
    /// # Errors
    /// When it isn't durable.
    fn outcome(&mut self, item: ItemId, outcome: &ItemOutcome) -> Result<(), ApplyError>;
}
