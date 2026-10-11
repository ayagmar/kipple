//! An npm 11.19.0 install: npm records it in `node_modules/.package-lock.json`.

use super::tree::FixtureTree;

/// A project with one installed dependency at `at`.
pub(crate) fn project(tree: &mut FixtureTree, at: &str) {
    tree.file(
        &format!("{at}/package.json"),
        b"{\"name\":\"web\",\"dependencies\":{\"dep\":\"1.0.0\"}}",
    )
    .file(
        &format!("{at}/package-lock.json"),
        b"{\"lockfileVersion\":3}",
    )
    .file(
        &format!("{at}/node_modules/.package-lock.json"),
        b"{\"lockfileVersion\":3}",
    )
    .file(
        &format!("{at}/node_modules/dep/package.json"),
        b"{\"name\":\"dep\"}",
    )
    .sized(&format!("{at}/node_modules/dep/index.js"), 300 * 1024);
}
