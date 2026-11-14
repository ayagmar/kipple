//! Taking a tool's own locks for one removal (spec M4 pinned 13). Implemented only by the
//! platform. A scan takes none (`docs/04-safety-model.md` §3.1): only apply does, and only
//! for as long as one item runs.

use std::fmt;
use std::path::PathBuf;

named! {
    /// Why a tool's locks can't be held now. Each leaves the item alone.
    LockGap {
        /// A lock file isn't there: it is never created, so this isn't the store it was.
        Missing = "lock-missing",
        /// Something else holds it.
        Held = "lock-held",
        /// It couldn't be opened or asked about.
        Unreadable = "lock-unreadable",
        /// This OS has no lock kipple knows to match the tool's.
        Unsupported = "lock-unsupported",
    }
}

/// Locks kept for as long as this lives. Dropping it releases them.
#[derive(Debug)]
pub struct HeldLocks {
    _held: Box<dyn fmt::Debug + Send>,
}

impl HeldLocks {
    /// `held`, which releases the locks when it is dropped.
    #[must_use]
    pub fn new(held: impl fmt::Debug + Send + 'static) -> Self {
        Self {
            _held: Box::new(held),
        }
    }
}

/// Takes a tool's own locks, the way the tool does.
pub trait StoreLocks: fmt::Debug + Send + Sync {
    /// Holds every lock in `paths`, or none. Never waits and never creates a file.
    ///
    /// # Errors
    /// When one is missing, held or can't be read.
    fn try_hold(&self, paths: &[PathBuf]) -> Result<HeldLocks, LockGap>;
}
