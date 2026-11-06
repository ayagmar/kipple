//! git 2.56.0's on-disk worktree contract, from S5 git §3: a main repository, each linked
//! worktree's admin directory and `.git` file, and §8's eight mixed-state worktrees. Only
//! the files are here, because a test scripts what git answers about them.

use std::path::Path;

use super::tree::FixtureTree;

/// How a registration's two links are written.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Links<'base> {
    /// Absolute, below the directory the tree is built in, as git writes them by default.
    Absolute(&'base Path),
    /// Relative to the file that holds them, as `worktree.useRelativePaths` writes them.
    Relative,
}

impl Links<'_> {
    /// What a link in the tree at `from`, a file, holds to reach `to`.
    fn target(self, from: &str, to: &str) -> String {
        match self {
            Self::Absolute(base) => base.join(to).display().to_string(),
            Self::Relative => {
                let up = from.matches('/').count();
                format!("{}{to}", "../".repeat(up))
            }
        }
    }
}

/// A repository's main worktree at `main`, with a `.git` directory git can open.
pub(crate) fn repository(tree: &mut FixtureTree, main: &str) {
    tree.file(&format!("{main}/.git/HEAD"), b"ref: refs/heads/main\n")
        .file(
            &format!("{main}/.git/config"),
            b"[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
        )
        .dir(&format!("{main}/.git/objects"))
        .dir(&format!("{main}/.git/refs/heads"));
}

/// A worktree of the repository at `main`, checked out at `path`, with its admin entry `id`.
pub(crate) fn linked(tree: &mut FixtureTree, main: &str, id: &str, path: &str, links: Links<'_>) {
    admin(tree, main, id, path, links);
    gitfile(tree, main, id, path, links);
}

/// The admin entry alone, as a checkout that was deleted leaves it.
pub(crate) fn admin(tree: &mut FixtureTree, main: &str, id: &str, path: &str, links: Links<'_>) {
    let admin = format!("{main}/.git/worktrees/{id}");
    let gitdir = format!("{admin}/gitdir");
    let back = links.target(&gitdir, &format!("{path}/.git"));
    tree.file(&format!("{admin}/HEAD"), b"ref: refs/heads/wt\n")
        .file(&format!("{admin}/commondir"), b"../..\n")
        .file(&gitdir, format!("{back}\n").as_bytes())
        .dir(&format!("{admin}/refs"));
}

/// The `.git` file alone, as a checkout whose admin entry was pruned keeps it.
pub(crate) fn gitfile(tree: &mut FixtureTree, main: &str, id: &str, path: &str, links: Links<'_>) {
    let file = format!("{path}/.git");
    let admin = format!("{main}/.git/worktrees/{id}");
    let target = links.target(&file, &admin);
    tree.file(&file, format!("gitdir: {target}\n").as_bytes());
}

/// The repository of S5 git §8.
pub(crate) const APP: &str = "home/projects/app";

/// S5 git §8's checkouts, each with its admin entry's name: every one but `swift-fox`, whose
/// directory is gone, and `warsaw`, whose admin entry is.
pub(crate) const PRESENT: [(&str, &str); 6] = [
    ("feature-a", "home/projects/app/.claude/worktrees/feature-a"),
    (
        "agent-a1b2c3d4e5f607182",
        "home/projects/app/.claude/worktrees/agent-a1b2c3d4e5f607182",
    ),
    ("fix-b", "home/projects/app/.claude/worktrees/fix-b"),
    ("ab12", "home/.codex/worktrees/ab12/app"),
    (
        "worktree-brave-river-1a2b",
        "home/.herdr/worktrees/app/worktree-brave-river-1a2b",
    ),
    ("fix-1", "home/projects/app-wt/fix-1"),
];

/// Case 5: registered, and its directory deleted.
pub(crate) const SWIFT_FOX: &str = "home/.cursor/worktrees/app/swift-fox";

/// Case 6: a `.git` file whose admin entry is gone.
pub(crate) const WARSAW: &str = "home/conductor/workspaces/app/warsaw";

/// The eight worktrees of S5 git §8, as their files stand.
pub(crate) fn eight(tree: &mut FixtureTree, links: Links<'_>) {
    repository(tree, APP);
    tree.file(
        &format!("{APP}/.gitignore"),
        b"node_modules/\n.env\n.claude/worktrees/\n",
    );
    for (id, path) in PRESENT {
        linked(tree, APP, id, path, links);
    }
    tree.file(
        &format!("{APP}/.git/worktrees/feature-a/CLAUDE_BASE"),
        b"0dc80dc80dc80dc80dc80dc80dc80dc80dc80dc8",
    )
    .file(
        &format!("{APP}/.git/worktrees/agent-a1b2c3d4e5f607182/locked"),
        b"any text at all",
    )
    .file(
        "home/projects/app/.claude/worktrees/fix-b/node_modules/.package-lock.json",
        b"{\"lockfileVersion\":3}",
    )
    .file(
        "home/projects/app/.claude/worktrees/fix-b/package.json",
        b"{\"name\":\"web\"}",
    )
    .file(
        "home/projects/app/.claude/worktrees/fix-b/package-lock.json",
        b"{\"lockfileVersion\":3}",
    )
    .file(
        &format!("{APP}/.git/worktrees/ab12/codex-thread.json"),
        b"{}",
    )
    .file(
        "home/.herdr/worktrees/app/worktree-brave-river-1a2b/.env",
        b"NOT_A_SECRET=1\n",
    );
    admin(tree, APP, "swift-fox", SWIFT_FOX, links);
    tree.dir("home/.cursor/worktrees/app");
    gitfile(tree, APP, "warsaw", WARSAW, links);
}
