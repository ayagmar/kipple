//! What `demo-home` shows of Pi. `demo_home.rs` calls this once the demo step enables
//! the hooks, so nothing compiles it before then.

use super::pi_v1_5_0;
use super::tree::FixtureTree;

/// Adds Pi to the demo tree.
pub(crate) fn demo(tree: &mut FixtureTree) {
    pi_v1_5_0::store(tree, "home/.pi/agent");
}
