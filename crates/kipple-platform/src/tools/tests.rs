//! The resolver against a fake `PATH` the test builds below a temp root.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use kipple_core::{NativeTool, ToolGap, ToolResolver as _};
use tempfile::TempDir;

use super::PathTools;
use crate::environment::Environment;

mod npm;

/// Catches a wrapper or a version manager's shim named `git` becoming the reviewed tool,
/// which would start a shell that picks a git by its own rules (pinned 27): a file that
/// names its interpreter is refused, and nothing runs it, not even for its version.
#[test]
fn a_script_named_git_is_refused_before_it_runs() {
    let temp = TempDir::new().unwrap();
    let ran = temp.path().join("ran");
    let git = temp.path().join("git");
    fs::write(&git, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
    let env =
        Environment::from_vars([(OsString::from("PATH"), temp.path().as_os_str().to_owned())]);

    let resolved = PathTools::new(&env).resolve(NativeTool::Git);

    assert_eq!(resolved, Err(ToolGap::Script));
    assert!(!ran.exists(), "the script never ran");
}
