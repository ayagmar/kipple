//! What `demo-home` shows of git worktrees. `demo_home.rs` calls this once the demo step enables
//! the hooks, beside a `v2_56_0` module. The demo tree's place isn't known here, so its links
//! are relative.

use super::tree::FixtureTree;
use super::v2_56_0::{self, Links};

/// Adds S5 git §8's eight worktrees to the demo tree.
pub(crate) fn demo(tree: &mut FixtureTree) {
    v2_56_0::eight(tree, Links::Relative);
}
