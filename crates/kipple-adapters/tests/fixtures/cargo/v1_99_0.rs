//! A Cargo 1.99.0 project after `cargo build`: the target directory carries the tag
//! Cargo writes into every target directory it creates.

use super::tree::FixtureTree;

const MIB: u64 = 1024 * 1024;

/// A built binary crate at `at`.
pub(crate) fn project(tree: &mut FixtureTree, at: &str) {
    tree.file(
        &format!("{at}/Cargo.toml"),
        b"[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .file(&format!("{at}/Cargo.lock"), b"version = 4\n")
    .file(&format!("{at}/src/main.rs"), b"fn main() {}\n")
    .file(
        &format!("{at}/target/CACHEDIR.TAG"),
        b"Signature: 8a477f597d28d172789f06886806bc55\n\
              # This file is a cache directory tag created by cargo.\n\
              # For information about cache directory tags see https://bford.info/cachedir/\n",
    )
    .file(&format!("{at}/target/.rustc_info.json"), b"{}")
    .sized(&format!("{at}/target/debug/demo"), 4 * MIB)
    .sized(
        &format!("{at}/target/debug/deps/demo-0123456789abcdef"),
        4 * MIB,
    )
    .dir(&format!("{at}/target/debug/incremental"))
    .dir(&format!("{at}/target/debug/.fingerprint"));
}
