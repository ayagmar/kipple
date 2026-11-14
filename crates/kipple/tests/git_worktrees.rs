//! The git integration over repositories real git made, through the platform's `GitQuery`:
//! the one crate that sees both. Each repository is the test's own, under docs/08 §5's
//! isolation, so nothing of the user's git config or hooks is read or run.
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]

#[path = "git_worktrees/gate.rs"]
mod gate;
