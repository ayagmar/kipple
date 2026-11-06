//! What `demo-home` shows of Cursor. `demo_home.rs` calls this once the demo step enables
//! the hooks, so nothing compiles it before then.

use super::tree::FixtureTree;

/// Adds Cursor to the demo tree.
pub(crate) fn demo(_: &mut FixtureTree) {}
