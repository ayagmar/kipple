//! What `demo-home` shows of npm. `demo_home.rs` calls this once npm has something to
//! show, so nothing compiles it before then.

use super::tree::FixtureTree;

/// Adds npm to the demo tree.
pub(crate) fn demo(_: &mut FixtureTree) {}
