//! What `demo-home` shows of Cursor. `demo_home.rs` calls this once the demo step enables
//! the hooks, so nothing compiles it before then.

use super::tree::FixtureTree;

/// Adds Cursor to the demo tree: its configuration directory, which is what its advice
/// answers to. Nothing of Cursor's layout is verified, so nothing else is built.
pub(crate) fn demo(tree: &mut FixtureTree) {
    tree.file("home/.cursor/cli-config.json", b"{}\n");
}
