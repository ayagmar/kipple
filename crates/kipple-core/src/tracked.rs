//! Asking git which entries inside an item it tracks (`docs/04-safety-model.md` §3.2,
//! D-066). Implemented only by `kipple-platform`, through its runner.
//!
//! Finding the repository is kipple's own walk ([`Tracked`](crate::Tracked)): no `.git`
//! from the item up to the file system's root means nothing is tracked, and git is never
//! asked. When a walk finds one, git is asked about it and every repository above it, and
//! every answer it can't give in full blocks the item: git missing or too old, a failed or
//! truncated query, a work tree that isn't the one the walk found, or a returned path
//! kipple can't place in the item.
//!
//! The scan asks how many paths git tracks inside the item, which only decides whether it
//! is held. Planning and apply ask again of the frozen tree, and the platform places each
//! returned path on an entry of the tree by its file identity, so the file system folds
//! case and normalization itself. Those entries are left out of the plan with their
//! subtrees, keeping the directories above them. At apply the query runs again on the
//! fresh tree, after the freeze: it only ever subtracts.

use std::fmt;
use std::path::Path;

use crate::cancel::CancelToken;
use crate::frozen::{FrozenTree, NodeId};
use crate::plan::Anchor;

named! {
    /// Why git couldn't say what it tracks inside an item. Each blocks the item.
    GitGap {
        /// No `git` was found on the absolute entries of `PATH`.
        Missing = "git-missing",
        /// The `git` found is older than 2.32, which can't be isolated from the user's
        /// global config.
        TooOld = "git-too-old",
        /// git failed, timed out or was cancelled.
        Failed = "git-failed",
        /// git's answer was cut short or malformed: over its output bound, not ending in
        /// a NUL, or holding an empty record.
        Truncated = "git-truncated",
        /// The work tree git reports isn't the directory kipple's walk found.
        OtherWorkTree = "git-other-work-tree",
        /// A path git returned exists inside the item but is no entry kipple froze, is a
        /// directory (a submodule or a sparse directory), or leads through a link, a
        /// junction or a mount point, so whether it is in the item can't be told.
        Unplaced = "git-unplaced",
        /// The item's path can't be passed to git without loss (ill-formed UTF-16 on
        /// Windows).
        Unpassable = "git-unpassable",
        /// A `.git` lives inside the item: another repository, whose files git wasn't
        /// asked about.
        NestedRepository = "nested-repository",
        /// This build has no git query.
        Unsupported = "git-unsupported",
    }
}

/// Asks git what it tracks inside an item. Read-only: it runs git through the platform's
/// runner, isolated from the user's and the repository's config, hooks and pager.
pub trait TrackedQuery: fmt::Debug + Send + Sync {
    /// How many paths at or inside `item` the repository whose `.git` is in `work_tree`,
    /// the nearest, and every repository whose `.git` is above it track. For the scan,
    /// which holds the item unless git answered.
    ///
    /// # Errors
    /// When git couldn't answer in full.
    fn count(&self, work_tree: &Path, item: &Path, cancel: &CancelToken) -> Result<u64, GitGap>;

    /// The entries of `tree`, frozen at `anchor`, that the repository whose `.git` is in
    /// `work_tree`, or any repository whose `.git` is above it, tracks. Each returned path is placed on an entry by resolving it
    /// handle-relative from the item, without following links, and matching the
    /// entry's file identity. A path absent on disk (a sparse or skip-worktree entry)
    /// places nothing.
    ///
    /// # Errors
    /// When git couldn't answer in full, or a path it returned can't be placed.
    fn entries(
        &self,
        work_tree: &Path,
        anchor: &Anchor,
        tree: &FrozenTree,
        cancel: &CancelToken,
    ) -> Result<Vec<NodeId>, GitGap>;
}

/// Whether `tree` holds a `.git` below its top, compared ignoring ASCII case, which is
/// stricter than any file system: a repository inside the item blocks it.
#[must_use]
pub(crate) fn holds_repository(tree: &FrozenTree) -> bool {
    tree.entries().skip(1).any(|entry| {
        entry.name().is_some_and(|name| {
            name.as_os_str()
                .as_encoded_bytes()
                .eq_ignore_ascii_case(b".git")
        })
    })
}
