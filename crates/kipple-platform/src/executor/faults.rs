//! Directory syncs that fail on purpose, for the tests that check a failed sync is never
//! reported as done: no disk here fails one on demand. Only this thread's syncs fail.

use std::cell::Cell;
use std::io;
use std::path::Path;

use kipple_core::{Anchor, FsProbe as _, RelPath, SymbolicRoot, VerifiedRoot};

use crate::NoFollowFs;

thread_local! {
    /// How many more directory syncs on this thread succeed before they fail; `None`
    /// while none is set to fail.
    static PASSING: Cell<Option<u32>> = const { Cell::new(None) };
}

/// Makes every directory sync on this thread fail after the next `passing`, until the
/// returned guard is dropped.
pub(crate) fn fail_syncs_after(passing: u32) -> Failing {
    PASSING.set(Some(passing));
    Failing
}

/// While it lives, directory syncs on this thread fail as [`fail_syncs_after`] set.
pub(crate) struct Failing;

impl Drop for Failing {
    fn drop(&mut self) {
        PASSING.set(None);
    }
}

/// Fails if this sync is set to fail.
pub(super) fn sync() -> io::Result<()> {
    match PASSING.get() {
        None => Ok(()),
        Some(0) => Err(io::Error::other("a directory sync set to fail")),
        Some(left) => {
            PASSING.set(Some(left - 1));
            Ok(())
        }
    }
}

/// The anchor of `relative` below the root `root`, with the identities a scan and a
/// planner record, for the tests that run a delete or a move into a failing sync.
pub(crate) fn anchor(root: &Path, relative: &str) -> Anchor {
    let identity = |path: &Path| NoFollowFs.metadata(path).unwrap().identity;
    let relative = RelPath::of(Path::new(relative)).unwrap();
    let parent = relative
        .components()
        .split_last()
        .and_then(|(_, way)| identity(&RelPath::new(way.to_vec()).below(root)));
    Anchor {
        root: VerifiedRoot {
            root: SymbolicRoot::Home,
            path: root.to_owned(),
            identity: identity(root).unwrap(),
        },
        relative,
        parent,
    }
}
