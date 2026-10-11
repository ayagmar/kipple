//! pacman 7.1.0's package cache: each downloaded package with its signature.

use super::tree::FixtureTree;

/// A cache at `at` with two versions of one package.
pub(crate) fn cache(tree: &mut FixtureTree, at: &str) {
    for version in ["1.0-1", "1.1-1"] {
        let package = format!("{at}/demo-{version}-x86_64.pkg.tar.zst");
        tree.sized(&package, 8 * 1024 * 1024)
            .sized(&format!("{package}.sig"), 119);
    }
}
