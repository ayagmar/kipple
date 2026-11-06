//! What `demo-home` shows of Claude Code. `demo_home.rs` calls this once the demo step enables
//! the hooks, so nothing compiles it before then.

use super::claude_v2_1_318;
use super::tree::FixtureTree;

/// Adds Claude Code to the demo tree: a native install beside the demo's configuration
/// directory, whose settings `demo_home.rs` already writes.
pub(crate) fn demo(tree: &mut FixtureTree) {
    claude_v2_1_318::install(tree, "home", claude_v2_1_318::VERSION);
    let closed = claude_v2_1_318::CLOSED_SESSION;
    tree.file(&format!("home/.claude/debug/{closed}.txt"), b"[DEBUG] closed\n")
        .file(
            "home/.claude/sessions/4242.json",
            claude_v2_1_318::marker(claude_v2_1_318::OPEN_SESSION).as_bytes(),
        )
        .file("home/.claude/daemon.log", b"started\n");
}
