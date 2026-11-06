//! Taking a tool's own locks for one removal. Until an OS has its arm here, no lock can be
//! held, which leaves every item that needs one alone.

use std::path::PathBuf;

use kipple_core::{HeldLocks, LockGap, StoreLocks};

/// The platform's [`StoreLocks`].
#[derive(Debug, Clone, Copy, Default)]
pub struct FileLocks;

impl StoreLocks for FileLocks {
    fn try_hold(&self, _: &[PathBuf]) -> Result<HeldLocks, LockGap> {
        Err(LockGap::Unsupported)
    }
}
