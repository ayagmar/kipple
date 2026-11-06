//! What `demo-home` shows of git worktrees. `demo_home.rs` calls this once the demo step enables
//! the hooks, so nothing compiles it before then.

use super::tree::FixtureTree;

/// Adds git worktrees to the demo tree.
pub(crate) fn demo(_: &mut FixtureTree) {}
