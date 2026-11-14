//! A worktree's removal with an ancestor of the checkout, or the root itself, swapped for a
//! link to a decoy as git starts (04 §3.4; spec M4 pinned 21, 27). git starts in the
//! checkout's held directory, so its `.` is the checkout the runner checked, wherever that
//! now is, and with `-C` the same swap deleted the decoy. Linux alone ships the removal.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use kipple_core::ItemOutcome;

use crate::apply::removes_here;
use crate::repo::{Refs, Repo, listed, write};

/// Which of the two worktrees git registers first: under `-C` only the decoy-first order
/// deleted the decoy, so both are tried.
#[derive(Debug, Clone, Copy)]
enum Order {
    DecoyFirst,
    ReviewedFirst,
}

/// A `git` that swaps `from` for a link to `to`, then runs the real git.
fn swapping_git(repo: &Repo, from: &Path, to: &Path) -> PathBuf {
    let program = repo.temp.path().join("tool/git");
    let moved = from.with_extension("moved");
    let text = format!(
        "#!/bin/sh\n/bin/mv '{from}' '{moved}'\n/bin/ln -s '{to}' '{from}'\nexec '{git}' \"$@\"\n",
        from = from.display(),
        moved = moved.display(),
        to = to.display(),
        git = repo.real_git().path.display(),
    );
    write(&program, text.as_bytes());
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    program
}

fn add(repo: &Repo, branch: &str, at: &str) {
    repo.git(&["worktree", "add", "-q", "-b", branch, at]);
}

/// That the decoy at `projects/<name>` is whole: its files, its registration and its admin
/// directory, since git can drop a registration and leave the files.
fn decoy_stays(repo: &Repo, name: &str, order: Order) {
    let checkout = repo.projects().join(name);
    assert!(
        checkout.join("file.txt").exists(),
        "{order:?}: the decoy's files stay"
    );
    assert!(listed(repo, name), "{order:?}: the decoy is still listed");
    let gitfile = fs::read_to_string(checkout.join(".git")).unwrap();
    let admin = gitfile.trim().strip_prefix("gitdir: ").unwrap_or_default();
    let admin = checkout.join(admin);
    assert!(
        admin.join("gitdir").exists(),
        "{order:?}: the decoy's admin directory stays"
    );
}

/// Catches a removal rebound by a swapped ancestor: `projects/a`, above the reviewed
/// worktree, becomes a link to `projects/b`, above a clean registered worktree of the same
/// repository. Nothing is removed, in either registration order.
#[test]
fn swapped_ancestor_cannot_rebind_the_remove() {
    if !removes_here() {
        return;
    }
    for order in [Order::DecoyFirst, Order::ReviewedFirst] {
        let repo = Repo::bare_bones(Refs::Files);
        match order {
            Order::DecoyFirst => {
                add(&repo, "d", "../b/wt");
                add(&repo, "r", "../a/wt");
            }
            Order::ReviewedFirst => {
                add(&repo, "r", "../a/wt");
                add(&repo, "d", "../b/wt");
            }
        }
        let projects = repo.projects();
        let program = swapping_git(&repo, &projects.join("a"), &projects.join("b"));
        let removal = repo.removal_by(&program);
        let scan = removal.scan();

        let ran = removal.remove(&scan, &[scan.finding("a/wt")]);

        assert!(
            !matches!(ran.outcome(), ItemOutcome::Done(_)),
            "{order:?}: {:?}",
            ran.outcome()
        );
        decoy_stays(&repo, "b/wt", order);
        assert!(
            projects.join("a.moved/wt/file.txt").exists(),
            "{order:?}: the reviewed checkout stays"
        );
    }
}

/// Catches a removal rebound by a swapped root: the project root opened at review becomes a
/// link to a directory holding a clean registered worktree of the same repository at the
/// same name. Nothing is removed.
#[test]
fn swapped_root_cannot_rebind_the_remove() {
    if !removes_here() {
        return;
    }
    for order in [Order::DecoyFirst, Order::ReviewedFirst] {
        let repo = Repo::bare_bones(Refs::Files);
        match order {
            Order::DecoyFirst => {
                add(&repo, "d", "../decoy/wt");
                add(&repo, "r", "../root/wt");
            }
            Order::ReviewedFirst => {
                add(&repo, "r", "../root/wt");
                add(&repo, "d", "../decoy/wt");
            }
        }
        let projects = repo.projects();
        let root = projects.join("root");
        let program = swapping_git(&repo, &root, &projects.join("decoy"));
        let removal = repo.removal_by(&program).projects_at(root);
        let scan = removal.scan();

        let ran = removal.remove(&scan, &[scan.finding("root/wt")]);

        assert!(
            !matches!(ran.outcome(), ItemOutcome::Done(_)),
            "{order:?}: {:?}",
            ran.outcome()
        );
        decoy_stays(&repo, "decoy/wt", order);
        assert!(
            projects.join("root.moved/wt/file.txt").exists(),
            "{order:?}: the reviewed checkout stays"
        );
    }
}
