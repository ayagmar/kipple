//! npm's contract (spec M4 pinned 33): the real `npm cache clean` of each verified npm, run
//! by the native runner the way apply runs it, removes `_cacache` and nothing else, past
//! every npmrc that tries to move its logs, its prefix or its global config, and against a
//! process that swaps what it runs on. It also measures a 20,000-file cache against
//! `NPM_DEADLINE`. Linux alone starts a native command.
//!
//! CI installs each npm into a prefix of its own and names the prefix in
//! [`VERSIONS`]' variable: `<prefix>/lib/node_modules/npm` is the package and
//! `<prefix>/bin/node` the node it runs on, inside both versions' `engines`. Each test
//! copies the package into its own site, so it can write npm's own npmrc there, and never
//! writes below the prefix. A missing variable fails the test: the contract is never
//! skipped by accident.
#![cfg(target_os = "linux")]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that can't be set up fails the test"
)]

#[path = "../../../kipple-adapters/tests/fixtures/npm/v11_20_0.rs"]
mod npm_v11_20_0;
#[path = "../../../kipple-adapters/tests/fixtures/npm/v12_2_1.rs"]
mod npm_v12_2_1;
#[path = "../../../kipple-adapters/tests/fixtures/tree.rs"]
mod tree;

use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kipple_core::{
    ApplyError, DeleteOp, Marks, NativeCommand, NativeExit, NativeOp, NativeRunner as _,
    NativeSpec, NativeTool, Postcondition, ResolvedTool, ToolResolver as _,
};
use kipple_platform::{Environment, NativeCommands, PathTools};
use tree::FixtureTree;

use crate::executor::support::{Site, freeze, link, listing, move_to, present, write};

// The cache layouts are what these tests clean; the install layouts are the resolver's.
const _: () = {
    let _ = (npm_v11_20_0::install, npm_v12_2_1::install);
    let _ = (npm_v11_20_0::ENGINES, npm_v12_2_1::ENGINES);
    let _ = (npm_v11_20_0::NODE, npm_v12_2_1::NODE);
    let _ = FixtureTree::modified;
};

/// Each verified npm: the variable naming its prefix, its version and the cache it writes.
const VERSIONS: [Verified; 2] = [
    Verified {
        var: "KIPPLE_TEST_NPM_11_20_0",
        number: npm_v11_20_0::VERSION,
        cache: npm_v11_20_0::cache,
    },
    Verified {
        var: "KIPPLE_TEST_NPM_12_2_1",
        number: npm_v12_2_1::VERSION,
        cache: npm_v12_2_1::cache,
    },
];

/// `NPM_DEADLINE` in `kipple-platform/src/native.rs`, which a test can't read. A
/// 20,000-file cache must clean in a twentieth of it.
const NPM_DEADLINE: Duration = Duration::from_mins(5);

struct Verified {
    var: &'static str,
    number: &'static str,
    cache: fn(&mut FixtureTree, &str),
}

/// A site with a verified npm copied into it, a home and a cache at `root/cache`.
struct Npm {
    site: Site,
    /// The copy of npm's package, which holds its own npmrc.
    package: PathBuf,
    tool: ResolvedTool,
    home: PathBuf,
}

impl Npm {
    /// `version` from its CI prefix, copied below the site's outside area, resolved as
    /// apply resolves it, with its cache built in the site's root.
    fn new(version: &Verified) -> Self {
        let source = std::env::var_os(version.var).map_or_else(
            || {
                panic!(
                    "{} must name the prefix npm {} is installed in, with its node at \
                     bin/node",
                    version.var, version.number
                )
            },
            PathBuf::from,
        );
        let site = Site::new();
        let prefix = site.outside.join(format!("npm-{}", version.number));
        let package = prefix.join("lib/node_modules/npm");
        copy_dir(&source.join("lib/node_modules/npm"), &package);
        fs::create_dir_all(prefix.join("bin")).unwrap();
        let node = fs::canonicalize(source.join("bin/node")).unwrap();
        symlink(node, prefix.join("bin/node")).unwrap();
        symlink(
            "../lib/node_modules/npm/bin/npm-cli.js",
            prefix.join("bin/npm"),
        )
        .unwrap();
        let path =
            Environment::from_vars([(OsString::from("PATH"), prefix.join("bin").into_os_string())]);
        let tool = PathTools::new(&path).resolve(NativeTool::Npm).unwrap();
        assert_eq!(
            tool.version, version.number,
            "the copy is the version named"
        );
        let mut tree = FixtureTree::new();
        (version.cache)(&mut tree, "cache");
        tree.dir("home").build_in(&site.root).unwrap();
        let home = site.at("home");
        Self {
            site,
            package,
            tool,
            home,
        }
    }

    /// kipple's data directory, where the runner keeps npm's configuration boundary.
    fn data(&self) -> PathBuf {
        self.site.outside.join("data")
    }

    /// The clean of `_cacache` as a reviewed plan holds it, frozen now.
    fn reviewed(&self) -> NativeOp {
        let target = self.site.anchor("cache/_cacache");
        let tree = freeze(&target).tree;
        let marks = Marks::of(&tree);
        let spec = NativeSpec {
            command: NativeCommand::NpmCacheClean {
                cache: self.site.anchor("cache"),
            },
            tool: self.tool.clone(),
            scope: target.clone(),
        };
        let delete = DeleteOp::for_platform_tests(target, tree, marks);
        NativeOp::for_platform_tests(delete, spec, None, None)
    }

    /// Runs `op` as apply does, with the home as `HOME`.
    fn run(&self, op: &NativeOp) -> Result<NativeExit, ApplyError> {
        let env =
            Environment::from_vars([(OsString::from("HOME"), self.home.clone().into_os_string())]);
        NativeCommands::new(&env, Some(self.data())).run(op)
    }
}

/// Copies the directory `from` to `to`, links as links.
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &target);
        } else if kind.is_symlink() {
            symlink(fs::read_link(entry.path()).unwrap(), &target).unwrap();
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// An npmrc that would move everything npm writes into `sentinels/<name>-*`, and point
/// its global config at `global`, if npm read it.
fn moving(sentinels: &Path, name: &str, global: &Path) -> String {
    let at = |what: &str| {
        sentinels
            .join(format!("{name}-{what}"))
            .display()
            .to_string()
    };
    format!(
        "logs-dir={}\nlogs-max=1\nglobalconfig={}\nprefix={}\ncache={}\nupdate-notifier=true\n",
        at("logs"),
        global.display(),
        at("prefix"),
        at("cache"),
    )
}

/// Whether the run removed the reviewed cache, durably, and said so.
fn done(exit: &NativeExit) -> bool {
    exit.postcondition == Postcondition::Met && exit.synced.is_ok()
}

/// Catches npm writing or rotating anything outside `_cacache`: its logs past `logs-max`,
/// or wherever a user, global, builtin or project npmrc points its logs, prefix, global
/// config or cache. Every one of those stays as it was, nothing is written anywhere, and
/// the boundary's logs directory stays empty.
#[test]
fn npm_contract_touches_only_cacache() {
    for version in &VERSIONS {
        let npm = Npm::new(version);
        let site = &npm.site;
        for log in 0..12 {
            let name = format!("cache/_logs/2026-11-06T10_00_{log:02}_000Z-debug-0.log");
            write(&site.at(&name), b"0 verbose cli\n");
        }
        let sentinels = site.outside.join("sentinels");
        let global = site.outside.join("global-npmrc");
        let builtin_global = site.outside.join("builtin-global-npmrc");
        write(
            &npm.home.join(".npmrc"),
            moving(&sentinels, "user", &global).as_bytes(),
        );
        write(&global, moving(&sentinels, "global", &global).as_bytes());
        let builtin = moving(&sentinels, "builtin", &builtin_global);
        write(&npm.package.join("npmrc"), builtin.as_bytes());
        write(
            &builtin_global,
            moving(&sentinels, "named", &global).as_bytes(),
        );
        write(
            &site.at("cache/.npmrc"),
            moving(&sentinels, "project", &global).as_bytes(),
        );
        let root_before = listing(&site.root);
        let outside_before = listing(&site.outside);

        let exit = npm.run(&npm.reviewed()).unwrap();

        let version = version.number;
        assert!(done(&exit), "npm {version} cleaned the cache: {exit:?}");
        assert!(
            !present(&site.at("cache/_cacache")),
            "npm {version}: `_cacache` went"
        );
        let kept: Vec<_> = root_before
            .into_iter()
            .filter(|listed| !format!("{listed:?}").contains("/cache/_cacache"))
            .collect();
        assert_eq!(
            listing(&site.root),
            kept,
            "npm {version}: nothing else changed"
        );
        let data = npm.data();
        let outside_now: Vec<_> = listing(&site.outside)
            .into_iter()
            .filter(|listed| !format!("{listed:?}").contains(&data.display().to_string()))
            .collect();
        assert_eq!(
            outside_now, outside_before,
            "npm {version}: nothing written outside"
        );
        assert!(
            !present(&sentinels),
            "npm {version}: no npmrc moved anything"
        );
        let logs = data.join("native/npm/logs");
        assert_eq!(
            listing(&logs).len(),
            0,
            "npm {version}: the boundary has no log"
        );
        site.assert_outside_intact();
    }
}

/// Catches npm cleaning a directory swapped in above the reviewed cache after the
/// review: a process moves the cache's parent aside and puts a decoy cache at its path.
/// npm runs in the reviewed directory, or not at all, and the decoy survives.
#[test]
fn swapped_ancestor_cannot_rebind_npm() {
    for version in &VERSIONS {
        let npm = Npm::new(version);
        let site = &npm.site;
        let op = npm.reviewed();
        move_to(&site.at("cache"), &site.at("moved"));
        write(&site.at("cache/_cacache/index-v5/decoy"), b"not reviewed");

        let ran = npm.run(&op);

        let version = version.number;
        let decoy = fs::read(site.at("cache/_cacache/index-v5/decoy")).ok();
        assert_eq!(
            decoy.as_deref(),
            Some(&b"not reviewed"[..]),
            "npm {version}"
        );
        if let Ok(exit) = ran {
            assert!(
                !present(&site.at("moved/_cacache")) || !done(&exit),
                "npm {version}: what it did is what it reports"
            );
        }
        site.assert_outside_intact();
    }
}

/// Catches npm following a link swapped in for the reviewed `_cacache`: it removes the
/// link, never what it leads to, and the run doesn't read as done.
#[test]
fn swapped_cacache_link_keeps_its_target() {
    for version in &VERSIONS {
        let npm = Npm::new(version);
        let site = &npm.site;
        let victim = site.outside.join("victim");
        write(&victim.join("index-v5/keep"), b"someone else's");
        write(&victim.join("content-v2/keep"), b"someone else's");
        let op = npm.reviewed();
        let before = listing(&victim);
        site.replace(&site.at("cache/_cacache"), |path| link(&victim, path));

        let ran = npm.run(&op);

        let version = version.number;
        assert_eq!(
            listing(&victim),
            before,
            "npm {version}: the link's target stayed"
        );
        if let Ok(exit) = ran {
            assert!(
                !done(&exit),
                "npm {version}: the reviewed cache wasn't cleaned"
            );
        }
        site.assert_outside_intact();
    }
}

/// Measures the clean of a 20,000-file cache, cacache's index and content half each, and
/// catches `NPM_DEADLINE` set too close to it. The time is printed for the record.
#[test]
fn a_20000_file_cache_cleans_well_inside_npm_deadline() {
    for version in &VERSIONS {
        let npm = Npm::new(version);
        let site = &npm.site;
        for n in 0..10_000_u32 {
            let hash = format!("{n:0>8x}{:0>56}", n.wrapping_mul(2_654_435_761));
            let (a, rest) = hash.split_at(2);
            let (b, rest) = rest.split_at(2);
            let content = format!("cache/_cacache/content-v2/sha512/{a}/{b}/{rest}");
            let index = format!("cache/_cacache/index-v5/{a}/{b}/{rest}");
            write(&site.at(&content), &[b'x'; 512]);
            write(&site.at(&index), b"entry\n");
        }
        let op = npm.reviewed();

        let started = Instant::now();
        let exit = npm.run(&op).unwrap();
        let took = started.elapsed();

        let version = version.number;
        let measured = format!("npm {version}: cache clean of 20,000 files took {took:?}\n");
        std::io::stdout().write_all(measured.as_bytes()).unwrap();
        assert!(done(&exit), "npm {version} cleaned the cache: {exit:?}");
        assert!(
            took < NPM_DEADLINE / 20,
            "npm {version} took {took:?}, too close to NPM_DEADLINE"
        );
    }
}
