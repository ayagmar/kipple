//! A fake `node` for the npm tests: a real executable, since the resolver refuses a
//! runtime that names its interpreter (`#!`), built with the `rustc` beside the `cargo`
//! that builds the tests. It answers `--version` with the text of `node.version` beside it,
//! and for anything else logs its arguments and working directory to `node.log` beside it,
//! then for `cache clean` removes `_cacache` in its working directory, as npm does, unless
//! `node.exit` beside it holds an exit code other than 0, which it exits with instead after
//! removing only `_cacache/index-v5` (a partial clean). It never runs a real npm.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The fake's source.
const SOURCE: &str = r#"
use std::io::Write as _;
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap();
    let mut log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("node.log")).unwrap();
    let cwd = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default();
    writeln!(log, "{}\t{}", args.join(" "), cwd).unwrap();
    if args.first().map(String::as_str) == Some("--version") {
        let version = std::fs::read_to_string(dir.join("node.version")).unwrap_or_else(|_| "26.12.1".into());
        println!("v{}", version.trim());
        return;
    }
    if args.iter().any(|a| a == "cache") && args.iter().any(|a| a == "clean") {
        let code: i32 = std::fs::read_to_string(dir.join("node.exit")).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0);
        if code == 0 {
            let _ = std::fs::remove_dir_all("_cacache");
        } else {
            let _ = std::fs::remove_dir_all("_cacache/index-v5");
            eprintln!("npm error code EACCES");
        }
        std::process::exit(code);
    }
}
"#;

/// Builds the fake as `<bin>/node`, answering `--version` with `v<version>`, and returns
/// its path.
///
/// # Errors
/// When the fake can't be written or `rustc` fails.
#[expect(
    clippy::disallowed_methods,
    reason = "the test builds its fake node with the toolchain's own rustc"
)]
pub(crate) fn build(bin: &Path, version: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(bin)?;
    let source = bin.join("fake_node.rs");
    fs::write(&source, SOURCE)?;
    let node = bin.join(format!("node{}", std::env::consts::EXE_SUFFIX));
    let rustc =
        Path::new(env!("CARGO")).with_file_name(format!("rustc{}", std::env::consts::EXE_SUFFIX));
    let status = Command::new(rustc)
        .args(["--edition", "2021", "-C", "debuginfo=0", "-o"])
        .arg(&node)
        .arg(&source)
        .status()?;
    if !status.success() {
        return Err(io::Error::other("rustc couldn't build the fake node"));
    }
    fs::remove_file(&source)?;
    fs::write(bin.join("node.version"), version)?;
    Ok(node)
}
