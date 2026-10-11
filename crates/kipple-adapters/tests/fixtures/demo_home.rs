//! `demo-home`: the one shared tree used by the end-to-end tests, the docs demo and the
//! tapes (`docs/06-adapters-and-rules.md` §7), built from the upstream layout fixtures.

use std::time::{Duration, SystemTime};

use super::tree::FixtureTree;
use super::{
    bun_v1_4_2, cargo_v1_99_0, claude_settings, journald_v262, npm_v11_19_0, pacman_v7_1_0,
    pnpm_v12_9_1, trash_v1_0, uv_v0_12_23, venv_v3_14_8,
};

/// Each symbolic root of the demo, with its path relative to the demo directory.
pub(crate) const ROOTS: &[(&str, &str)] = &[
    ("home", "home"),
    ("xdg.config", "home/.config"),
    ("xdg.cache", "home/.cache"),
    ("xdg.data", "home/.local/share"),
    ("xdg.state", "home/.local/state"),
    ("config.project-roots", "home/projects"),
    ("trash", "home/.local/share/Trash/files"),
    ("claude.config", "home/.claude"),
    ("pacman.cache", "var/cache/pacman/pkg"),
    ("journald.journal", "var/log/journal"),
];

const DAY: Duration = Duration::from_hours(24);

/// When a demo built now is looked at: long enough after it was built that its
/// directories, which keep the time they were created, are older than any recent window.
/// A scan of it runs with its clock fixed here.
pub(crate) fn looked_at() -> SystemTime {
    SystemTime::now() + 60 * DAY
}

/// The demo tree, looked at `now`: a home with projects for each supported layout, plus
/// the system locations kipple reports. Its files were last modified 41 days before
/// `now`, except one project's dependencies, installed 2 days before.
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
    tree.modified("home", now - 41 * DAY)
        .modified("var", now - 41 * DAY)
        .modified(&format!("{projects}/web/node_modules"), now - 2 * DAY);
    tree
}
