//! The demo's `PATH` (AGENTS.md 8): a demo run asks the host's git and starts nothing
//! else of the host's, so a verified npm on the host's own `PATH` is never run, and every
//! host shows the same demo.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser as _;
use kipple::{Cli, Io};
use kipple_core::Platform;
use kipple_platform::{Environment, PLATFORM};
use serde_json::Value;

use super::demo_home::npm_v11_20_0;
use super::tree::FixtureTree;
use super::{Demo, fake_node, json, present};

/// npm is looked for only on Linux, where its cache has a rule that cleans it.
const LOOKS_FOR_NPM: bool = matches!(PLATFORM, Platform::Linux);

/// What `kipple <args>`, which prints JSON, prints for the demo when its `PATH` is
/// `path`, run in this process, whose own environment keeps the host's `PATH`.
fn json_with(demo: &Demo, path: OsString, args: &[&str]) -> Value {
    let env = Environment::from_vars([
        (
            OsString::from("KIPPLE_TEST_ROOTS"),
            demo.roots.clone().into_os_string(),
        ),
        (OsString::from("PATH"), path),
    ]);
    let cli = Cli::parse_from(std::iter::once("kipple").chain(args.iter().copied()));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = kipple::run(&cli, &env, Io::redirected(&mut out, &mut err));
    assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
    serde_json::from_slice(&out).unwrap()
}

/// The finding of `scan` at `relative` below the demo.
fn finding<'scan>(scan: &'scan Value, relative: &str) -> &'scan Value {
    scan.get("findings")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|finding| {
            finding
                .pointer("/path/display")
                .and_then(Value::as_str)
                .is_some_and(|display| Path::new(display).ends_with(relative))
        })
        .unwrap()
}

/// What the decoy's node logged: one line for each time it was started.
fn started(log: &Path) -> String {
    match fs::read_to_string(log) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        read => read.unwrap(),
    }
}

/// npm 11.20.0, a version kipple runs, with a node that logs each start, laid out below
/// `host/npm` in the demo's temp dir, outside every root: a host's npm, on a `PATH` the
/// demo doesn't get. Returns the `bin` directory and the node's log.
fn decoy_npm(demo: &Demo) -> (OsString, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;

    let mut tree = FixtureTree::new();
    npm_v11_20_0::install(&mut tree, "host/npm");
    tree.build_in(&demo.at("")).unwrap();
    let script = demo.at("host/npm/lib/node_modules/npm/bin/npm-cli.js");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let bin = demo.at("host/npm/bin");
    fake_node::build(&bin, npm_v11_20_0::NODE).unwrap();
    let log = bin.join("node.log");
    (bin.into_os_string(), log)
}

/// Asserts that every directory on the `PATH` a demo run gets holds git alone.
fn demo_path_holds_git_alone(demo: &Demo) {
    let env = demo.env();
    for entry in std::env::split_paths(env.get("PATH").unwrap()) {
        let names: Vec<OsString> = fs::read_dir(&entry)
            .unwrap()
            .map(|child| child.unwrap().file_name())
            .collect();
        assert_eq!(names, ["git"], "{} holds git alone", entry.display());
    }
}

/// The value at `pointer` in `value`, null when there is none.
fn at<'value>(value: &'value Value, pointer: &str) -> &'value Value {
    value.pointer(pointer).unwrap_or(&Value::Null)
}

/// Asserts what the demo's scan says of npm's cache: held on Linux for the missing npm,
/// which `doctor` finds nowhere on the demo's `PATH`; reported elsewhere.
fn cache_is_held_for_the_missing_npm(demo: &Demo, scan: &Value) {
    let cache = finding(scan, "home/.npm/_cacache");
    assert_eq!(at(cache, "/verdict/bucket"), "protected", "{cache}");
    if !LOOKS_FOR_NPM {
        assert_eq!(at(cache, "/rule"), "builtin.npm.inventory", "{cache}");
        return;
    }
    assert_eq!(at(cache, "/rule"), "builtin.npm.cache", "{cache}");
    let reasons = at(cache, "/verdict/reasons");
    assert_eq!(
        *reasons,
        json(r#"["fact-unmet","fact-unknown"]"#),
        "{cache}"
    );
    let doctor = json_with(demo, demo.path.clone(), &["doctor", "--json"]);
    let tools = at(&doctor, "/tools").as_array().unwrap();
    let npm = tools
        .iter()
        .find(|tool| at(tool, "/name") == "npm")
        .unwrap();
    assert_eq!(
        *at(npm, "/path"),
        Value::Null,
        "no npm on the demo's PATH: {npm}"
    );
}

/// Catches a demo run that starts a program of the host's other than git, or a demo that
/// reads differently from host to host: with a verified npm on a host-style `PATH`, a scan
/// and a clean of the Safe bucket with the demo's `PATH` never start its node, npm's cache
/// is held on Linux for the missing tool, every directory on the `PATH` a demo run gets
/// holds git alone, and the built binary, run with no other `PATH` in its environment,
/// prints the same scan. The decoy is shown to start when it is on `PATH`, so its silence
/// means something.
#[test]
fn a_demo_run_starts_no_program_of_the_hosts_but_git() {
    let demo = Demo::new();
    let (decoy, log) = decoy_npm(&demo);
    if LOOKS_FOR_NPM {
        let decoyed = json_with(&demo, decoy, &["scan", "--json"]);
        assert!(
            started(&log).starts_with("--version"),
            "the decoy's node ran"
        );
        let cache = finding(&decoyed, "home/.npm/_cacache");
        assert_eq!(cache["verdict"]["bucket"], "safe", "{cache}");
    }
    let before = started(&log);

    let scan = json_with(&demo, demo.path.clone(), &["scan", "--json"]);
    let path = demo.path.to_str().unwrap();
    let spawned = demo.spawn(&["scan", "--json"], &[("PATH", path)], "");
    let cleaned = demo.answer(&["clean"], "y\n");

    assert_eq!(started(&log), before, "nothing started the decoy's node");
    assert_eq!(cleaned.code, ExitCode::SUCCESS, "{}", cleaned.err);
    assert!(present(&demo.at("home/.npm/_cacache")), "the cache stays");
    demo_path_holds_git_alone(&demo);
    cache_is_held_for_the_missing_npm(&demo, &scan);
    assert_eq!(spawned.code, Some(0), "{}", spawned.err);
    assert_eq!(json(&spawned.out), scan, "the same scan with no host PATH");
}
