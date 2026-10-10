//! Native tool capabilities against fake tools on a disposable `PATH`
//! (`docs/05-architecture.md` §1): a tool counts once its version probe succeeds.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use kipple_core::{NativeTool, ToolError, ToolStatus};
use kipple_platform::{Environment, probe_tools};
use tempfile::TempDir;

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn script(dir: &Path, name: &str, body: &str, mode: u32) {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn a_tool_counts_only_as_an_executable_on_an_absolute_path_entry_that_reports_a_version() {
    let temp = TempDir::new().unwrap();
    let not_executable = temp.path().join("plain");
    let tools = temp.path().join("tools");
    script(&not_executable, "git", "echo 'git version 9.9.9'", 0o644);
    script(&tools, "git", "echo 'git version 2.56.0'", 0o755);
    script(&tools, "journalctl", "echo broken >&2; exit 3", 0o755);
    // A relative entry would depend on the directory kipple was started in.
    let path = std::env::join_paths(["tools".as_ref(), not_executable.as_path(), &tools]).unwrap();
    let env = Environment::from_vars([(OsString::from("PATH"), path)]);

    let found = probe_tools(&env);

    let [
        (
            NativeTool::Git,
            ToolStatus::Found {
                path: git,
                version: Ok(version),
            },
        ),
        (NativeTool::Paccache, ToolStatus::NotFound),
        (
            NativeTool::Journalctl,
            ToolStatus::Found {
                version: Err(failure),
                ..
            },
        ),
    ] = found.as_slice()
    else {
        panic!("unexpected {found:?}");
    };
    assert_eq!(git, &tools.join("git"));
    assert_eq!(version.to_string(), "2.56.0");
    assert!(matches!(failure, ToolError::Failed { code: Some(3), stderr } if stderr == "broken"));
}
