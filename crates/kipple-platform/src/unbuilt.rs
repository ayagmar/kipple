//! The port M3b's git card builds, until it does. It answers unknown, so nothing is
//! judged safe on its account.

use std::path::Path;

use kipple_core::{Anchor, CancelToken, FrozenTree, GitGap, NodeId, TrackedQuery};

/// The git query, unbuilt: every item inside a repository is held as unknown.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoGit;

impl TrackedQuery for NoGit {
    fn count(&self, _: &Path, _: &Path, _: &CancelToken) -> Result<u64, GitGap> {
        Err(GitGap::Unsupported)
    }

    fn entries(
        &self,
        _: &Path,
        _: &Anchor,
        _: &FrozenTree,
        _: &CancelToken,
    ) -> Result<Vec<NodeId>, GitGap> {
        Err(GitGap::Unsupported)
    }
}
