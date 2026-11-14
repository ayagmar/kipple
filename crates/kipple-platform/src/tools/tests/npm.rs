//! npm resolved from installs the test lays out below a temp root, each with a fake node
//! that logs every start, so a test sees what ran. No real npm or node runs.
#![expect(
    clippy::unwrap_used,
    reason = "an install that can't be laid out fails the test"
)]

#[path = "../../../../kipple-adapters/tests/fixtures/tree.rs"]
mod tree;

#[path = "../../../../kipple-adapters/tests/fixtures/npm/fake_node.rs"]
mod fake_node;
#[path = "../../../../kipple-adapters/tests/fixtures/npm/v11_20_0.rs"]
mod npm_v11_20_0;
#[path = "../../../../kipple-adapters/tests/fixtures/npm/v12_2_1.rs"]
mod npm_v12_2_1;

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use kipple_core::{NativeTool, ResolvedTool, ToolGap, ToolResolver as _};
use tempfile::TempDir;

use super::super::PathTools;
use crate::environment::Environment;
use crate::roots::resolve_roots;
use tree::FixtureTree;

// `tree.rs` keeps `dead_code` off the builder methods most binaries leave unused, but
// not these, and both versions lay out the same install.
const _: () = {
    let _ = FixtureTree::modified;
    let _ = FixtureTree::sized;
    let _ = npm_v11_20_0::cache;
    let _ = npm_v12_2_1::cache;
    let _ = npm_v12_2_1::install;
};

/// One npm install: its version, the node versions it says it runs on, and the version
/// its node answers.
struct Layout<'text> {
    version: &'text str,
    engines: &'text str,
    node: &'text str,
}

const V11: Layout<'static> = Layout {
    version: npm_v11_20_0::VERSION,
    engines: npm_v11_20_0::ENGINES,
    node: npm_v11_20_0::NODE,
};

const V12: Layout<'static> = Layout {
    version: npm_v12_2_1::VERSION,
    engines: npm_v12_2_1::ENGINES,
    node: npm_v12_2_1::NODE,
};

/// Lays out an install of `layout` at `<root>/<name>`, as upstream's tarball does, with
/// its fake node, and returns its prefix.
fn install(root: &Path, name: &str, layout: &Layout<'_>) -> PathBuf {
    let mut tree = FixtureTree::new();
    npm_v11_20_0::install(&mut tree, name);
    tree.build_in(root).unwrap();
    let prefix = root.join(name);
    let package = prefix.join("lib/node_modules/npm");
    let manifest = fs::read_to_string(package.join("package.json")).unwrap();
    let manifest = manifest
        .replace(npm_v11_20_0::VERSION, layout.version)
        .replace(npm_v11_20_0::ENGINES, layout.engines);
    fs::write(package.join("package.json"), manifest).unwrap();
    let script = package.join("bin/npm-cli.js");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fake_node::build(&prefix.join("bin"), layout.node).unwrap();
    prefix.canonicalize().unwrap()
}

/// An environment whose `PATH` is `bins`, in order.
fn path(bins: &[PathBuf]) -> Environment {
    let joined = std::env::join_paths(bins).unwrap();
    Environment::from_vars([(OsString::from("PATH"), joined)])
}

fn resolve(bins: &[PathBuf]) -> Result<ResolvedTool, ToolGap> {
    PathTools::new(&path(bins)).resolve(NativeTool::Npm)
}

/// Every start of the node of `prefix`, as its arguments.
fn started(prefix: &Path) -> Vec<String> {
    let log = fs::read_to_string(prefix.join("bin/node.log")).unwrap_or_default();
    log.lines()
        .map(|line| line.split('\t').next().unwrap_or_default().to_owned())
        .collect()
}

/// Catches the resolver returning the wrapper on `PATH`, or a node other than the one
/// beside the package: the run would start a program kipple didn't check.
#[test]
fn a_verified_install_resolves_to_its_script_and_its_own_node() {
    let temp = TempDir::new().unwrap();
    let prefix = install(temp.path(), "npm", &V11);

    let resolved = resolve(&[prefix.join("bin")]).unwrap();

    let script = prefix.join("lib/node_modules/npm/bin/npm-cli.js");
    assert_eq!(resolved.path, script);
    assert_eq!(resolved.version, V11.version);
    let runtime = resolved.runtime.unwrap();
    assert_eq!(runtime.path, prefix.join("bin/node"));
    assert_ne!(runtime.identity, resolved.identity, "two files");
}

/// Catches a missing npm read as one that needs nothing: nothing to run is a hold.
#[test]
fn npm_missing_blocks() {
    let temp = TempDir::new().unwrap();
    let empty = temp.path().join("empty");
    fs::create_dir(&empty).unwrap();

    assert_eq!(resolve(&[empty]), Err(ToolGap::Missing));
    let unset = Environment::from_vars([]);
    let resolved = PathTools::new(&unset).resolve(NativeTool::Npm);
    assert_eq!(resolved, Err(ToolGap::Missing));
}

/// Catches a version kipple never ran the contract against being run: its `cache
/// clean` may touch more than `_cacache`. Nothing of it starts, not even its node.
#[test]
fn version_outside_the_list_blocks() {
    let temp = TempDir::new().unwrap();
    for version in ["12.2.2", "11.19.0"] {
        let layout = Layout { version, ..V12 };
        let prefix = install(temp.path(), version, &layout);

        let resolved = resolve(&[prefix.join("bin")]);

        assert_eq!(resolved, Err(ToolGap::UnverifiedVersion), "{version}");
        assert_eq!(
            started(&prefix),
            Vec::<String>::new(),
            "{version}: nothing ran"
        );
    }
}

/// Catches a shim, a wrapper or a script named `npm` or `node` taken as npm: each
/// starts a program kipple didn't check, chosen by rules it doesn't read.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test replaces the install's link with a wrapper script"
)]
fn wrapper_and_shim_rejected() {
    let temp = TempDir::new().unwrap();
    let ran = temp.path().join("ran");
    let shims = temp.path().join("shims");
    fs::create_dir(&shims).unwrap();
    let wrapper = shims.join("npm");
    fs::write(&wrapper, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(resolve(&[shims]), Err(ToolGap::Script));

    let manager = temp.path().join("mise");
    fs::copy(&wrapper, &manager).unwrap();
    let linked = temp.path().join("linked");
    fs::create_dir(&linked).unwrap();
    symlink(&manager, linked.join("npm")).unwrap();
    assert_eq!(resolve(&[linked]), Err(ToolGap::Script));
    assert!(!ran.exists(), "no shim ran");

    let scripted = install(temp.path(), "scripted", &V11);
    let node = scripted.join("bin/node");
    fs::write(&node, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
    assert_eq!(resolve(&[scripted.join("bin")]), Err(ToolGap::Script));
    assert!(!ran.exists(), "the scripted node never ran");

    let own_wrapper = install(temp.path(), "wrapped", &V11);
    let bin_npm = own_wrapper.join("bin/npm");
    fs::remove_file(&bin_npm).unwrap();
    fs::write(
        &bin_npm,
        "#!/bin/sh\nexec node \"$0/../npm-cli.js\" \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&bin_npm, fs::Permissions::from_mode(0o755)).unwrap();
    let resolved = resolve(&[own_wrapper.join("bin")]).unwrap();
    assert_eq!(
        resolved.path,
        own_wrapper.join("lib/node_modules/npm/bin/npm-cli.js"),
        "a prefix's own wrapper leads to its script, which is what runs"
    );
}

/// Catches npm run on a node it doesn't support, where its behaviour isn't the one the
/// contract test saw.
#[test]
fn node_outside_engines_blocks() {
    let temp = TempDir::new().unwrap();
    let old = Layout {
        node: "22.9.0",
        ..V12
    };
    let prefix = install(temp.path(), "old", &old);
    assert_eq!(
        resolve(&[prefix.join("bin")]),
        Err(ToolGap::UnsupportedRuntime),
        "the npm is verified; its node isn't one it accepts"
    );
    assert_eq!(
        started(&prefix),
        ["--version"],
        "only its version was asked"
    );

    let odd = Layout {
        node: "26.0.0-pre",
        ..V12
    };
    let odd = install(temp.path(), "odd", &odd);
    assert_eq!(resolve(&[odd.join("bin")]), Err(ToolGap::UnknownVersion));

    let unread = Layout {
        engines: ">=22 <27 || 26.x",
        ..V12
    };
    let unread = install(temp.path(), "unread", &unread);
    assert_eq!(resolve(&[unread.join("bin")]), Err(ToolGap::UnknownVersion));
}

/// Catches a version manager's later install shadowing the verified one, or the pair
/// mixed: the script of one install run on the node of another.
#[test]
fn first_verified_pair_runs_when_two_majors_share_a_cache() {
    let temp = TempDir::new().unwrap();
    let newer = Layout {
        version: "12.2.2",
        ..V12
    };
    let unverified = install(temp.path(), "newer", &newer);
    let v11 = install(temp.path(), "v11", &V11);
    let v12 = install(temp.path(), "v12", &V12);
    let bins = [unverified.join("bin"), v11.join("bin"), v12.join("bin")];

    let resolved = resolve(&bins).unwrap();

    assert_eq!(resolved.version, V11.version);
    assert_eq!(
        resolved.path,
        v11.join("lib/node_modules/npm/bin/npm-cli.js")
    );
    assert_eq!(resolved.runtime.unwrap().path, v11.join("bin/node"));
    assert_eq!(
        started(&v12),
        Vec::<String>::new(),
        "the later one never ran"
    );
}

/// Catches an install without a node of its own read as no npm at all, and npm packaged
/// outside `lib/node_modules/npm` (Debian's `/usr/share/nodejs/npm`) read as a script
/// that starts another program: either sends a person looking for the wrong fix.
#[test]
fn an_install_without_its_node_or_in_another_layout_says_which() {
    let temp = TempDir::new().unwrap();
    let mut tree = FixtureTree::new();
    npm_v11_20_0::install(&mut tree, "bare");
    let debian = "debian/share/nodejs/npm";
    tree.file(
        &format!("{debian}/package.json"),
        b"{\"name\":\"npm\",\"version\":\"11.20.0\"}",
    )
    .file(
        &format!("{debian}/bin/npm-cli.js"),
        b"#!/usr/bin/env node\n",
    )
    .link(
        "debian/bin/npm",
        "../share/nodejs/npm/bin/npm-cli.js",
        tree::Holds::AsWritten,
    );
    tree.build_in(temp.path()).unwrap();
    let root = temp.path().canonicalize().unwrap();
    for script in [
        "bare/lib/node_modules/npm/bin/npm-cli.js",
        "debian/share/nodejs/npm/bin/npm-cli.js",
    ] {
        fs::set_permissions(root.join(script), fs::Permissions::from_mode(0o755)).unwrap();
    }

    assert_eq!(resolve(&[root.join("bare/bin")]), Err(ToolGap::NoRuntime));
    let packaged = resolve(&[root.join("debian/bin")]);
    assert_eq!(packaged, Err(ToolGap::UnknownLayout));
}

/// Catches reading npm's settings by running npm, which writes and rotates its logs:
/// placing the cache reads files alone, and the resolver starts only its node, for
/// `--version`. `crates/kipple/tests/npm.rs` holds the same for a scan and `doctor`.
#[test]
fn placing_the_cache_runs_nothing_and_resolving_starts_only_node() {
    let temp = TempDir::new().unwrap();
    let prefix = install(temp.path(), "npm", &V12);
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let mut env = vec![
        (OsString::from("PATH"), prefix.join("bin").into_os_string()),
        (OsString::from("HOME"), home.into_os_string()),
    ];
    let roots = resolve_roots(&Environment::from_vars(env.clone()), &[]);
    assert!(
        matches!(roots.get(kipple_core::SymbolicRoot::NpmCache), Some(Ok(_))),
        "the cache is placed"
    );
    assert_eq!(
        started(&prefix),
        Vec::<String>::new(),
        "placing it ran nothing"
    );

    env.truncate(1);
    PathTools::new(&Environment::from_vars(env))
        .resolve(NativeTool::Npm)
        .unwrap();

    assert_eq!(started(&prefix), ["--version"]);
}
