//! A pnpm 12.9.1 install: pnpm records it in `node_modules/.modules.yaml` and keeps the
//! packages under `node_modules/.pnpm`.

use super::tree::FixtureTree;

/// A project with one installed dependency at `at`.
pub(crate) fn project(tree: &mut FixtureTree, at: &str) {
    tree.file(
        &format!("{at}/package.json"),
        b"{\"name\":\"dash\",\"dependencies\":{\"dep\":\"1.0.0\"}}",
    )
    .file(&format!("{at}/pnpm-lock.yaml"), b"lockfileVersion: '9.0'\n")
    .file(
        &format!("{at}/node_modules/.modules.yaml"),
        b"layoutVersion: 5\n",
    )
    .file(
        &format!("{at}/node_modules/.pnpm-workspace-state-v1.json"),
        b"{}",
    )
    .file(
        &format!("{at}/node_modules/.pnpm/dep@1.0.0/node_modules/dep/package.json"),
        b"{\"name\":\"dep\"}",
    )
    .sized(
        &format!("{at}/node_modules/.pnpm/dep@1.0.0/node_modules/dep/index.js"),
        200 * 1024,
    );
}
