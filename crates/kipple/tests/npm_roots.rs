//! Where npm's cache is placed on each OS, and what that closes (spec M4 pinned 33): an
//! npm install laid out as this OS's Node installer lays it out, found on `PATH`, whose
//! npmrc files place the cache, read by the real reader. On Windows that is `npm.cmd` and
//! `node.exe` side by side in the prefix, the package in `node_modules\npm`, whose own
//! npmrc sets `prefix=${APPDATA}\npm` (so the global npmrc is `%APPDATA%\npm\etc\npmrc`),
//! and a default cache in `%LOCALAPPDATA%\npm-cache`, as npm's `@npmcli/config` places
//! them; elsewhere `bin/npm` and `bin/node` with the package in `lib/node_modules/npm`, and
//! `~/.npm`. Every root comes from a test-roots file in a temp dir and nothing runs, so no
//! real npm, home or npmrc is read. The Windows arms run only on Windows CI.
#![expect(
    clippy::unwrap_used,
    reason = "a temp dir or path that can't be made fails the test"
)]

#[path = "../../kipple-adapters/tests/fixtures/tree.rs"]
mod tree;

#[path = "../../kipple-adapters/tests/fixtures/npm/v11_20_0.rs"]
#[expect(
    dead_code,
    reason = "this OS's layout is written here; the fixture's install is Unix's"
)]
mod npm_v11_20_0;

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};

use kipple_adapters::builtin::descriptors;
use kipple_core::{
    FsProbe as _, HeldPath, IntegrationId, Lineage, Owners, Protections, Roots, SymbolicRoot,
};
use kipple_platform::{Environment, NoFollowFs, resolve_roots};
use tree::FixtureTree;

// `tree.rs` keeps `dead_code` off the builder methods most binaries leave unused, but not
// these, which every other binary that includes it uses.
const _: () = {
    let _ = FixtureTree::modified;
    let _ = FixtureTree::link;
};

/// The `node_modules` an `npx` run leaves below a cache root.
const NPX: &str = "_npx/0123456789abcdef/node_modules";

/// What a path's ancestors are, as the host reads them.
fn lineage(path: &Path) -> Lineage {
    let identity = |at: &Path| NoFollowFs.metadata(at).ok().and_then(|m| m.identity);
    Lineage(path.ancestors().map(identity).collect())
}

/// Whether two paths name the same directory, however each is spelled.
fn same_dir(a: &Path, b: &Path) -> bool {
    let identity = |at: &Path| NoFollowFs.metadata(at).ok().and_then(|m| m.identity);
    identity(a).is_some() && identity(a) == identity(b)
}

#[cfg(unix)]
fn make_runnable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Windows runs a file by its extension.
#[cfg(not(unix))]
const fn make_runnable(_: &Path) {}

/// A machine in a temp dir with npm 11.20.0 installed as this OS lays it out, the only
/// directory on `PATH`, its home, `%APPDATA%` and `%LOCALAPPDATA%` below it, and a cache
/// as an install and an `npx` run leave it wherever npm's default puts it.
struct Machine {
    _temp: tempfile::TempDir,
    at: PathBuf,
    /// The directory on `PATH`.
    bin: PathBuf,
    /// The roots file: the OS's roots in the temp dir, npm's cache placed by its npmrc.
    roots: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let at = temp.path().to_path_buf();
        let windows = cfg!(windows);
        let (package, bin, script) = if windows {
            ("nodejs/node_modules/npm", "nodejs", "nodejs/npm.cmd")
        } else {
            (
                "nodejs/lib/node_modules/npm",
                "nodejs/bin",
                "nodejs/bin/npm",
            )
        };
        let node = if windows {
            "nodejs/node.exe"
        } else {
            "nodejs/bin/node"
        };
        let default = if windows {
            "local/npm-cache"
        } else {
            "home/.npm"
        };
        let manifest = format!(
            "{{\"name\":\"npm\",\"version\":\"{}\",\"engines\":{{\"node\":\"{}\"}}}}",
            npm_v11_20_0::VERSION,
            npm_v11_20_0::ENGINES
        );
        // The installer's own npmrc, as Node's Windows installer ships it.
        let builtin = format!("prefix=${{APPDATA}}{MAIN_SEPARATOR}npm\n");
        let mut tree = FixtureTree::new();
        tree.file(&format!("{package}/package.json"), manifest.as_bytes())
            .file(
                &format!("{package}/bin/npm-cli.js"),
                b"#!/usr/bin/env node\n",
            )
            .file(&format!("{package}/npmrc"), builtin.as_bytes())
            .file(script, b"@ECHO off\r\n")
            .file(node, b"")
            .dir("home")
            .dir("roaming")
            .dir("local")
            .dir("code/app/node_modules");
        npm_v11_20_0::cache(&mut tree, default);
        tree.build_in(&at).unwrap();
        make_runnable(&at.join(script));
        let mut file = String::new();
        for (name, relative) in [
            ("home", "home"),
            ("known.roaming-app-data", "roaming"),
            ("known.local-app-data", "local"),
            ("npm.default-cache", default),
        ] {
            writeln!(file, "{name} = {}", at.join(relative).display()).unwrap();
        }
        writeln!(file, "npm.cache = npmrc").unwrap();
        let roots = at.join("roots.txt");
        fs::write(&roots, file).unwrap();
        Self {
            bin: at.join(bin),
            _temp: temp,
            at,
            roots,
        }
    }

    fn at(&self, relative: &str) -> PathBuf {
        self.at.join(relative)
    }

    /// The roots a run on this machine resolves.
    fn roots(&self) -> Roots {
        let vars = [
            ("KIPPLE_TEST_ROOTS", self.roots.clone().into_os_string()),
            ("PATH", self.bin.clone().into_os_string()),
            ("APPDATA", self.at("roaming").into_os_string()),
            ("LOCALAPPDATA", self.at("local").into_os_string()),
        ];
        let env = Environment::from_vars(vars.map(|(name, value)| (OsString::from(name), value)));
        resolve_roots(&env, &[])
    }

    /// Whether `roots` keep the Node integration out of `relative`.
    fn closed_to_node(&self, roots: &Roots, relative: &str) -> bool {
        let path = self.at(relative);
        let held = |at: &Path| HeldPath {
            forms: vec![at.to_owned()],
            lineage: lineage(at),
            note: None,
        };
        let data = self.at("data");
        let protections = Protections::of_host(roots, Some(&data), &descriptors(), &held);
        let found = protections.check(
            &path,
            &lineage(&path),
            IntegrationId("node"),
            &Owners::default(),
        );
        found.is_some()
    }
}

/// The directory `roots` place npm's cache in.
fn npm_cache(roots: &Roots) -> PathBuf {
    let cache = roots.get(SymbolicRoot::NpmCache).unwrap();
    cache.as_ref().unwrap().path.clone()
}

/// Catches npm's default cache placed somewhere npm doesn't write, on Windows
/// `%LOCALAPPDATA%\npm-cache` beside the install's `npm.cmd` and `node.exe`: the `npx`
/// packages below it would be offered to the Node integration as a project's.
#[test]
fn the_default_cache_is_placed_where_this_os_npm_puts_it() {
    let machine = Machine::new();
    let default = if cfg!(windows) {
        "local/npm-cache"
    } else {
        "home/.npm"
    };

    let roots = machine.roots();

    assert!(
        same_dir(&npm_cache(&roots), &machine.at(default)),
        "{roots:?}"
    );
    assert!(
        machine.closed_to_node(&roots, &format!("{default}/{NPX}")),
        "npx's node_modules is closed"
    );
    assert!(
        !machine.closed_to_node(&roots, "code/app/node_modules"),
        "a project's stays open"
    );
}

/// Catches a cache moved by the global npmrc that the installer's own npmrc places (on
/// Windows `%APPDATA%\npm\etc\npmrc`) read as not moved: the moved cache's `npx` packages
/// would be open to the Node integration. The default cache stays closed beside it.
#[test]
fn a_cache_moved_by_the_global_npmrc_is_placed_and_closed() {
    let machine = Machine::new();
    let default = if cfg!(windows) {
        "local/npm-cache"
    } else {
        "home/.npm"
    };
    let mut tree = FixtureTree::new();
    npm_v11_20_0::cache(&mut tree, "moved");
    let moved = machine.at("moved");
    let global = format!("cache={}\n", moved.display());
    tree.file("roaming/npm/etc/npmrc", global.as_bytes());
    tree.build_in(&machine.at).unwrap();

    let roots = machine.roots();

    assert!(same_dir(&npm_cache(&roots), &moved), "{roots:?}");
    for npx in [format!("moved/{NPX}"), format!("{default}/{NPX}")] {
        assert!(machine.closed_to_node(&roots, &npx), "{npx} is closed");
    }
    assert!(
        !machine.closed_to_node(&roots, "code/app/node_modules"),
        "a project's stays open"
    );
}
