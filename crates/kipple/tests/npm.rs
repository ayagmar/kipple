//! npm's cache as the composed host sees it: the root the platform resolves from npm's own
//! configuration, closed by what the npm integration declares (spec M4 pinned 33). Unix
//! only: on Windows npm is found as `npm.cmd`, which these installs don't have.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "a temp dir or path that can't be made fails the test"
)]

#[path = "../../kipple-adapters/tests/fixtures/tree.rs"]
mod tree;

#[path = "../../kipple-adapters/tests/fixtures/npm/v11_20_0.rs"]
#[expect(
    dead_code,
    reason = "these tests use the install and the cache, not the node"
)]
mod npm_v11_20_0;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use kipple_adapters::builtin::descriptors;
use kipple_core::{
    FsProbe as _, HeldPath, IntegrationId, Lineage, Owners, Protections, Roots, SymbolicRoot,
};
use kipple_platform::{Environment, NoFollowFs, resolve_roots};
use tree::FixtureTree;

// `tree.rs` keeps `dead_code` off the builder methods most binaries leave unused, but not
// this one, which every other binary that includes it uses.
const _: () = {
    let _ = FixtureTree::modified;
};

/// What a path's ancestors are, as the host reads them.
fn lineage(path: &Path) -> Lineage {
    let identity = |at: &Path| NoFollowFs.metadata(at).ok().and_then(|m| m.identity);
    Lineage(path.ancestors().map(identity).collect())
}

/// Whether the host keeps `integration` out of `path`.
fn closed(roots: &Roots, data: &Path, path: &Path, integration: &'static str) -> bool {
    let held = |at: &Path| HeldPath {
        forms: vec![at.to_owned()],
        lineage: lineage(at),
        note: None,
    };
    let protections = Protections::of_host(roots, Some(data), &descriptors(), &held);
    let found = protections.check(
        path,
        &lineage(path),
        IntegrationId(integration),
        &Owners::default(),
    );
    found.is_some()
}

/// A home whose `.npmrc` moves npm's cache into `~/code`, a project root, after an `npx`
/// run, with npm 11.20.0 on `PATH`.
fn moved_cache() -> (tempfile::TempDir, PathBuf, Roots) {
    let temp = tempfile::tempdir().unwrap();
    let at = temp.path().canonicalize().unwrap();
    let mut tree = FixtureTree::new();
    npm_v11_20_0::install(&mut tree, "npm");
    tree.file("npm/bin/node", b"")
        .file("home/.npmrc", b"cache=~/code/npm-cache\n")
        .file("home/code/app/package.json", b"{}");
    npm_v11_20_0::cache(&mut tree, "home/code/npm-cache");
    tree.build_in(&at).unwrap();
    make_runnable(&at.join("npm/lib/node_modules/npm/bin/npm-cli.js"));
    let home = at.join("home");
    let vars = [
        ("HOME", home.clone().into_os_string()),
        ("PATH", at.join("npm/bin").into_os_string()),
    ];
    let env = Environment::from_vars(vars.map(|(k, v)| (OsString::from(k), v)))
        .with_system_root(at.join("no-system"));
    let roots = resolve_roots(&env, &[]);
    (temp, home, roots)
}

fn make_runnable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Catches the interim the Packs review found: a cache moved by `.npmrc` read as absent, so
/// the `node_modules` an `npx` run leaves below it is open to the Node integration, which
/// would offer a cache npm reuses as a project's dependencies.
#[test]
fn npx_node_modules_is_no_node_candidate() {
    let (_temp, home, roots) = moved_cache();
    let cache = home.join("code/npm-cache");
    let resolved = roots.get(SymbolicRoot::NpmCache).unwrap().as_ref().unwrap();
    assert_eq!(resolved.path, cache);
    let data = home.join(".local/share/kipple");

    let npx = cache.join("_npx/0123456789abcdef/node_modules");
    let project = home.join("code/app");

    assert!(
        closed(&roots, &data, &npx, "node"),
        "npx's node_modules is closed"
    );
    assert!(
        !closed(&roots, &data, &project, "node"),
        "a project beside the cache stays open"
    );
}
