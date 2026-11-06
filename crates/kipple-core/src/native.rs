//! The closed set of native operations (`docs/04-safety-model.md` §6).

use std::fmt;

named! {
    /// A first-party native operation: a tool's own command, reviewed as a semantic plan.
    NativeKind {
        /// `git worktree remove`.
        GitWorktreeRemove = "git-worktree-remove",
        /// `npm cache clean`.
        NpmCacheClean = "native.npm-cache-clean",
    }
}
