//! Taking a tool's own locks for one removal (spec M4 pinned 13). Linux takes the lock
//! Codex's installer takes; elsewhere no lock can be held, which leaves every item that
//! needs one alone.

use std::path::PathBuf;

use kipple_core::{HeldLocks, LockGap, StoreLocks};

use crate::os;

/// The platform's [`StoreLocks`].
#[derive(Debug, Clone, Copy, Default)]
pub struct FileLocks;

impl StoreLocks for FileLocks {
    fn try_hold(&self, paths: &[PathBuf]) -> Result<HeldLocks, LockGap> {
        os::hold_locks(paths).map(HeldLocks::new)
    }
}
