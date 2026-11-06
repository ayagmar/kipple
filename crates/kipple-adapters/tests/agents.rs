//! Claude Code and Pi against upstream layouts built with `FixtureTree`, through the
//! real engine with injected roots, time and processes (`docs/06-adapters-and-rules.md` §5,
//! §7). Each row of S5's negative cases is a test here, named for what it keeps from going
//! wrong; the file an upstream layout comes from says which version it is.

#[path = "fixtures/claude/v2_1_318.rs"]
mod claude_v2_1_318;
#[path = "agents/host.rs"]
mod host;
#[path = "fixtures/pi/v1_5_0.rs"]
mod pi_v1_5_0;
#[path = "fixtures/tree.rs"]
mod tree;

// Sparse files are for the size-driven fixtures, and this suite has none yet.
const _: () = {
    let _ = tree::FixtureTree::sized;
};

#[path = "agents/claude.rs"]
mod claude;
#[path = "agents/pi.rs"]
mod pi;
