//! What `demo-home` shows of npm: npm 11.20.0's cache at `~/.npm` after an install and an
//! `npx` run. `demo_home.rs` calls this once npm has something to show, so nothing
//! compiles it before then; it then declares `npm/v11_20_0.rs` as `npm_v11_20_0` beside it
//! and places `npm.cache` and `npm.default-cache` at `home/.npm`.

use super::npm_v11_20_0;
use super::tree::FixtureTree;

/// Adds npm to the demo tree.
pub(crate) fn demo(tree: &mut FixtureTree) {
    npm_v11_20_0::cache(tree, "home/.npm");
}
