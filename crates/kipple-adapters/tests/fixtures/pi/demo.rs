//! What `demo-home` shows of Pi. `demo_home.rs` calls this.

use super::pi_v1_5_0;
use super::tree::FixtureTree;

/// Adds Pi to the demo tree.
pub(crate) fn demo(tree: &mut FixtureTree) {
    pi_v1_5_0::store(tree, "home/.pi/agent");
}
