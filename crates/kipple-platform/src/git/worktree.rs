//! Asking git about a repository's worktrees (`docs/06-adapters-and-rules.md` §4). Until
//! this is filled in, git answers nothing, which leaves every fact unknown.

use std::path::Path;

use kipple_core::{CancelToken, GitGap, Registration, WorktreeQuery, WorktreeState};

use super::GitQuery;

impl WorktreeQuery for GitQuery {
    fn list(&self, _: &Path, _: &CancelToken) -> Result<Vec<Registration>, GitGap> {
        Err(GitGap::Failed)
    }

    fn inspect(&self, _: &Registration, _: &CancelToken) -> WorktreeState {
        WorktreeState::unknown(GitGap::Failed)
    }
}
