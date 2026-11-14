//! The native runner on the real file system: an ancestor of the command's target swapped
//! while the command runs (04 §3.4; spec M4 pinned 27). Linux alone starts a child in a
//! checked handle; elsewhere nothing starts, which `kipple-platform`'s own tests check.
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

use crate::executor::support::{Site, freeze, identity, present, write};

/// Writes the fake program at `path`, running `body`.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn fake(path: &Path, body: &str) {
    write(path, format!("#!/bin/sh\n{body}\n").as_bytes());
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Catches a command that follows a path after an ancestor swap: npm's cache at
/// `a/cache` is moved aside and a decoy put at its old path while the command runs, and
/// the command, working through its own working directory, removes the reviewed cache
/// and leaves the decoy, which the runner's postcondition, read through the cache root's
/// handle, agrees with.
#[test]
fn swapped_ancestor_leaves_the_child_in_the_checked_directory() {
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
    let tool = ResolvedTool {
        tool: NativeTool::Npm,
        path: cli.clone(),
        identity: identity(&cli),
        version: "11.6.2".to_owned(),
        runtime: Some(ToolFile {
            path: node.clone(),
            identity: identity(&node),
        }),
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
