//! Native-backend tests of a whole removal: a link swapped into the path of a worktree as
//! git starts. They run only in the `native` nextest profile, on a disposable host
//! (`docs/08-engineering-standards.md` §5).
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]

#[path = "git_worktrees/apply.rs"]
#[expect(dead_code, reason = "these tests use part of the removal harness")]
mod apply;
#[path = "git_worktrees/repo.rs"]
#[expect(dead_code, reason = "these tests use part of the repository harness")]
mod repo;
#[path = "git_worktrees/swap.rs"]
mod swap;
