//! A Bun 1.4.2 install: Bun writes no file into `node_modules` that records it, so the
//! layout can't be recognised.

use super::tree::FixtureTree;

/// A project with one installed dependency at `at`.
pub(crate) fn project(tree: &mut FixtureTree, at: &str) {
    tree.file(
        &format!("{at}/package.json"),
        b"{\"name\":\"bunapp\",\"dependencies\":{\"dep\":\"1.0.0\"}}",
    )
    .file(&format!("{at}/bun.lock"), b"{}")
    .file(
        &format!("{at}/node_modules/dep/package.json"),
        b"{\"name\":\"dep\"}",
    )
    .sized(&format!("{at}/node_modules/dep/index.js"), 100 * 1024);
}
