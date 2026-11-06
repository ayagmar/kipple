//! A Codex home for the demo: a store with a release left behind by an update, the old TUI
//! log and a cached plugin catalog. `selection-history` isn't here: it records the `current`
//! link once that exists, so [`selection_history`] writes it after the tree is built.

use std::fs;
use std::io;
use std::path::Path;

use super::codex_v0_165_0;
use super::codex_v0_166_0;
use super::tree::{FixtureTree, Holds};

/// Where the home is in the demo tree.
pub(crate) const HOME: &str = "home/.codex";

const TARGET: &str = "x86_64-unknown-linux-musl";

fn name(version: &str) -> String {
    format!("{version}-{TARGET}")
}

/// Adds the home to `tree`.
pub(crate) fn demo(tree: &mut FixtureTree) {
    let store = format!("{HOME}/packages/standalone");
    let releases = format!("{store}/releases");
    let selected = name(codex_v0_166_0::VERSION);
    let (rollback, stale) = (name(codex_v0_165_0::VERSION), name("0.164.0"));
    codex_v0_166_0::release(
        tree,
        &format!("{releases}/{selected}"),
        codex_v0_166_0::VERSION,
        TARGET,
    );
    let previous = format!("{releases}/{rollback}");
    codex_v0_165_0::release(tree, &previous, codex_v0_165_0::VERSION, TARGET);
    codex_v0_165_0::release(tree, &format!("{releases}/{stale}"), "0.164.0", TARGET);
    tree.link(
        &format!("{store}/current"),
        &format!("{releases}/{selected}"),
        Holds::AbsoluteInTree,
    )
    .file(&format!("{store}/install.lock"), b"")
    .sized(&format!("{HOME}/log/codex-tui.log"), 48 * 1024 * 1024)
    .sized(
        &format!("{HOME}/cache/remote_plugin_catalog/0123456789abcdef.json"),
        2 * 1024 * 1024,
    )
    .file(&format!("{HOME}/config.toml"), b"model = \"gpt\"\n");
}

/// Writes the store's `selection-history` below `root`, where the tree was built, from what
/// `stamp` says of the `current` link, as upstream does after it selects a release.
pub(crate) fn selection_history(
    root: &Path,
    stamp: impl FnOnce(&Path) -> io::Result<String>,
) -> io::Result<()> {
    let store = root.join(HOME).join("packages/standalone");
    let stamp = stamp(&store.join("current"))?;
    let selected = name(codex_v0_166_0::VERSION);
    let previous = name(codex_v0_165_0::VERSION);
    let history = format!("{selected}\n{previous}\n{stamp}\n");
    fs::write(store.join("selection-history"), history)
}
