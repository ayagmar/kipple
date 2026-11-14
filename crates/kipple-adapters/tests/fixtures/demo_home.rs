//! `demo-home`: the one shared tree used by the end-to-end tests, the docs demo and the
//! tapes (`docs/06-adapters-and-rules.md` §7), built from the upstream layout fixtures.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::tree::{self, FixtureTree};
use super::{
    bun_v1_4_2, cargo_v1_99_0, claude_settings, journald_v262, npm_v11_19_0, pacman_v7_1_0,
    pnpm_v12_9_1, trash_v1_0, uv_v0_12_23, venv_v3_14_8,
};

// What the demo shows of each agent, from that agent's own fixtures. They are declared here
// and not beside this file's other fixtures, so a crate that includes the demo names one
// module and gets them all.
#[path = "claude/demo.rs"]
mod claude_demo;
#[path = "claude/v2_1_318.rs"]
#[expect(dead_code, reason = "the demo uses part of this layout's fixture")]
mod claude_v2_1_318;
#[path = "codex/demo.rs"]
mod codex_demo;
#[path = "codex/v0_165_0.rs"]
mod codex_v0_165_0;
#[path = "codex/v0_166_0.rs"]
#[expect(dead_code, reason = "the demo uses part of this layout's fixture")]
mod codex_v0_166_0;
#[path = "cursor/demo.rs"]
mod cursor_demo;
#[path = "git/demo.rs"]
mod git_demo;
#[path = "npm/demo.rs"]
mod npm_demo;
#[path = "npm/v11_20_0.rs"]
pub(crate) mod npm_v11_20_0;
#[path = "pi/demo.rs"]
mod pi_demo;
#[path = "pi/v1_5_0.rs"]
mod pi_v1_5_0;
#[path = "git/v2_56_0.rs"]
#[expect(dead_code, reason = "the demo uses part of this layout's fixture")]
mod v2_56_0;

/// Each symbolic root of the demo, with its path relative to the demo directory.
pub(crate) const ROOTS: &[(&str, &str)] = &[
    ("home", "home"),
    ("xdg.config", "home/.config"),
    ("xdg.cache", "home/.cache"),
    ("xdg.data", "home/.local/share"),
    ("xdg.state", "home/.local/state"),
    // kipple's own config directory on Windows; `xdg.config` holds it elsewhere.
    ("known.roaming-app-data", "home/AppData/Roaming"),
    ("config.project-roots", "home/projects"),
    ("trash", "home/.local/share/Trash/files"),
    ("claude.config", "home/.claude"),
    ("claude.home", "home/.claude"),
    ("claude.config-file", "home/.claude.json"),
    ("claude.versions", "home/.local/share/claude"),
    ("claude.install-state", "home/.local/state/claude"),
    ("codex.home", "home/.codex"),
    ("codex.default-home", "home/.codex"),
    ("codex.release-store", "home/.codex/packages"),
    ("pi.agent", "home/.pi/agent"),
    ("pi.home", "home/.pi"),
    ("cursor.config", "home/.cursor"),
    ("cursor.app-data", "home/.config/Cursor"),
    // Where agents keep worktrees outside a project: one root, several places.
    ("git.agent-locations", "home/.codex/worktrees"),
    ("git.agent-locations", "home/.cursor/worktrees"),
    ("git.agent-locations", "home/conductor/workspaces"),
    ("git.agent-locations", "home/.herdr/worktrees"),
    ("npm.cache", "home/.npm"),
    ("npm.default-cache", "home/.npm"),
    ("pacman.cache", "var/cache/pacman/pkg"),
    ("journald.journal", "var/log/journal"),
];

// Not every crate that builds the demo runs it, or lays out npm's install beside it as a
// host's would be; naming these here keeps `dead_code` off them in those.
const _: () = {
    let _ = path;
    let _ = npm_v11_20_0::install;
    let _ = npm_v11_20_0::NODE;
};

/// The `PATH` a run on the demo built in `dir` gets: one directory that holds the host's
/// git and nothing else, so a scan can ask git about the demo's worktrees and can start no
/// other program of the host's, npm and node included (AGENTS.md 8). Every host then shows
/// the same demo: npm's cache, say, is held on Linux for the missing tool. The directory is
/// made below `dir` on first use. On Windows it is the host git's own directory, which may
/// hold more than git; nothing of npm is looked for there. A host with no git is an error,
/// not an empty `PATH`.
pub(crate) fn path(dir: &Path) -> io::Result<OsString> {
    let git = host_git()?;
    git_dir(dir, &git).map(PathBuf::into_os_string)
}

/// The first `git` on the absolute entries of the process's `PATH`, as kipple finds it.
fn host_git() -> io::Result<PathBuf> {
    let name = format!("git{}", std::env::consts::EXE_SUFFIX);
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .filter(|entry| entry.is_absolute())
        .map(|entry| entry.join(&name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no git on PATH: a demo-home run needs the host's git",
            )
        })
}

/// `<dir>/tools/git`, holding a link named `git` to `git`.
#[cfg(unix)]
fn git_dir(dir: &Path, git: &Path) -> io::Result<PathBuf> {
    let tools = dir.join("tools").join("git");
    std::fs::create_dir_all(&tools)?;
    match std::os::unix::fs::symlink(git, tools.join("git")) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        made => made?,
    }
    Ok(tools)
}

/// The host git's own directory: a lone `git.exe` link can't find its DLLs. No npm is
/// looked for off Linux, so whatever else that directory holds changes nothing.
#[cfg(windows)]
fn git_dir(_: &Path, git: &Path) -> io::Result<PathBuf> {
    git.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "git has no directory"))
}

const DAY: Duration = Duration::from_hours(24);

/// When a demo built now is looked at: long enough after it was built that its
/// directories, which keep the time they were created, are older than any recent window.
/// A scan of it runs with its clock fixed here.
pub(crate) fn looked_at() -> SystemTime {
    SystemTime::now() + 60 * DAY
}

/// The demo tree, looked at `now`: a home with projects for each supported layout, plus
/// the system locations kipple reports. Its files were last modified 41 days before
/// `now`, except one project's dependencies, installed 4 days before: recent at Careful
/// and Balanced, not at Thorough, and clear of every level's window edge.
pub(crate) fn tree(now: SystemTime) -> FixtureTree {
    let mut tree = FixtureTree::new();
    let projects = "home/projects";
    cargo_v1_99_0::project(&mut tree, &format!("{projects}/rust-app"));
    cargo_v1_99_0::project(&mut tree, &format!("{projects}/clients/acme/api"));
    npm_v11_19_0::project(&mut tree, &format!("{projects}/web"));
    pnpm_v12_9_1::project(&mut tree, &format!("{projects}/dash"));
    bun_v1_4_2::project(&mut tree, &format!("{projects}/bunapp"));
    venv_v3_14_8::project(&mut tree, &format!("{projects}/tool"));
    uv_v0_12_23::project(&mut tree, &format!("{projects}/uvproj"));
    // A directory named like an artifact, but no project beside it.
    tree.file(&format!("{projects}/notes/target/plan.md"), b"# Plan\n");
    tree.dir("home/.config")
        .dir("home/.cache")
        .dir("home/.local/state");
    trash_v1_0::trash(&mut tree, "home/.local/share/Trash");
    claude_settings::config(&mut tree, "home/.claude", 14);
    pacman_v7_1_0::cache(&mut tree, "var/cache/pacman/pkg");
    journald_v262::journal(&mut tree, "var/log/journal");
    claude_demo::demo(&mut tree);
    codex_demo::demo(&mut tree);
    tree.after_build(codex_selection_history);
    cursor_demo::demo(&mut tree);
    git_demo::demo(&mut tree);
    npm_demo::demo(&mut tree);
    pi_demo::demo(&mut tree);
    tree.modified("home", now - 41 * DAY)
        .modified("var", now - 41 * DAY)
        .modified(&format!("{projects}/web/node_modules"), now - 4 * DAY);
    tree
}

/// Writes the Codex store's `selection-history` once its `current` link exists, since the
/// file records that link's stamp. A system with no such stamp gets no file, and its store
/// reads as one whose history is missing.
fn codex_selection_history(root: &Path) -> io::Result<()> {
    match codex_demo::selection_history(root, FixtureTree::link_stamp) {
        Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(()),
        written => written,
    }
}
