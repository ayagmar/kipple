//! Waits that fail on purpose and a hook run right before a spawn, for the tests that
//! check a command lost after it started is never read as one that didn't start, and
//! that a child starts in its checked directory whatever its path has become by the
//! spawn. Only this thread's runs see them.

use std::cell::{Cell, RefCell};
use std::io;

thread_local! {
    /// Whether this thread's waits on a child fail.
    static WAITS_FAIL: Cell<bool> = const { Cell::new(false) };
    /// What runs right before this thread's next spawn.
    static BEFORE_SPAWN: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

/// Makes every wait on a child on this thread fail, until the returned guard is dropped.
pub(crate) fn fail_waits() -> Failing {
    WAITS_FAIL.set(true);
    Failing
}

/// While it lives, waits on this thread fail as [`fail_waits`] set.
pub(crate) struct Failing;

impl Drop for Failing {
    fn drop(&mut self) {
        WAITS_FAIL.set(false);
    }
}

/// Runs `hook` right before this thread's next spawn, after the command was built. Only
/// Linux starts a child in a checked handle, so only its tests swap one.
#[cfg(target_os = "linux")]
pub(crate) fn before_spawn(hook: impl FnOnce() + 'static) {
    BEFORE_SPAWN.set(Some(Box::new(hook)));
}

/// Fails if this wait is set to fail.
pub(super) fn wait() -> io::Result<()> {
    if WAITS_FAIL.get() {
        Err(io::Error::other("a wait set to fail"))
    } else {
        Ok(())
    }
}

/// Runs the hook set for this spawn, if any.
pub(super) fn spawning() {
    if let Some(hook) = BEFORE_SPAWN.take() {
        hook();
    }
}
