//! The native runner on the real file system: an ancestor of the command's target moved
//! by the command itself while it runs (04 §3.4; spec M4 pinned 27). Linux alone starts a
//! child in a checked handle; elsewhere nothing starts, which `kipple-platform`'s own tests
//! check. The swap between the runner's check and the spawn, which needs a seam inside
//! the runner, is `kipple-platform`'s unit test
//! `swapped_ancestor_leaves_the_child_in_the_checked_directory`.
#![cfg(target_os = "linux")]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use kipple_core::{
    DeleteOp, Marks, NativeCommand, NativeOp, NativeRunner as _, NativeSpec, NativeTool,
    Postcondition, ResolvedTool, ToolFile,
};
use kipple_platform::{Environment, NativeCommands};

use crate::executor::support::{Site, anchor_at, freeze, present, write};

/// Writes the fake program at `path`, running `body`.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn fake(path: &Path, body: &str) {
    write(path, format!("#!/bin/sh\n{body}\n").as_bytes());
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The file at `path` as a resolver records it: frozen as itself, below its directory.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn file(path: &Path) -> ToolFile {
    let name = path.file_name().unwrap().to_str().unwrap();
    let anchor = anchor_at(path.parent().unwrap(), name);
    let top = freeze(&anchor).tree.top().snapshot().unwrap();
    ToolFile {
        path: path.to_owned(),
        identity: top.identity,
        fingerprint: top.fingerprint,
    }
}

/// Catches a postcondition read by path after an ancestor moved: npm's cache at
/// `a/cache` is moved aside and a decoy put at its old path by the command itself, which,
/// working through its own working directory, removes the reviewed cache and leaves the
/// decoy. The runner's postcondition, read through the handles it opened before the
/// spawn, agrees, and nothing outside the root is touched.
#[test]
fn a_command_that_moves_its_ancestor_removes_only_the_reviewed_cache() {
    let site = Site::new();
    write(&site.at("a/cache/_cacache/index-v5/entry"), b"reviewed");
    let bin = site.outside.join("bin");
    let node = bin.join("node");
    let cli = bin.join("npm-cli.js");
    let body = format!(
        "/bin/mv '{a}' '{moved}'\n\
         /bin/mkdir -p '{decoy}'\n\
         printf decoy > '{decoy}/entry'\n\
         /bin/rm -rf /proc/self/cwd/_cacache",
        a = site.at("a").display(),
        moved = site.at("moved").display(),
        decoy = site.at("a/cache/_cacache").display(),
    );
    fake(&node, &body);
    write(&cli, b"// npm\n");
    let script = file(&cli);
    let tool = ResolvedTool {
        tool: NativeTool::Npm,
        path: script.path,
        identity: script.identity,
        fingerprint: script.fingerprint,
        version: "11.6.2".to_owned(),
        runtime: Some(file(&node)),
    };
    let target = site.anchor("a/cache/_cacache");
    let tree = freeze(&target).tree;
    let marks = Marks::of(&tree);
    let spec = NativeSpec {
        command: NativeCommand::NpmCacheClean {
            cache: site.anchor("a/cache"),
        },
        tool,
        scope: target.clone(),
    };
    let op = NativeOp::for_platform_tests(
        DeleteOp::for_platform_tests(target, tree, marks),
        spec,
        None,
        None,
    );
    let env = Environment::from_vars([(
        OsString::from("HOME"),
        site.outside.clone().into_os_string(),
    )]);
    let runner = NativeCommands::new(&env, Some(site.outside.join("data")));

    let exit = runner.run(&op).unwrap();

    assert!(
        !present(&site.at("moved/cache/_cacache")),
        "the reviewed cache went"
    );
    assert_eq!(
        fs::read(site.at("a/cache/_cacache/entry")).unwrap(),
        b"decoy",
        "the decoy at the old path stayed"
    );
    assert_eq!(exit.postcondition, Postcondition::Met);
    site.assert_outside_intact();
}
