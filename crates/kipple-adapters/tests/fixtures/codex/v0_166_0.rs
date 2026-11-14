//! A release as Codex 0.166.0 lays it out, from S5 codex §3: `execServer` in its manifest and
//! three files in `bin/`. On Windows it has no `codex` alias and its programs end in `.exe`.

use super::tree::{FixtureTree, Holds};

/// The version this layout was read from.
pub(crate) const VERSION: &str = "0.166.0";

/// What a release's manifest says, in the order upstream writes it.
pub(crate) fn manifest(version: &str, target: &str, entrypoint: &str) -> String {
    format!(
        r#"{{
  "layoutVersion": 1,
  "version": "{version}",
  "target": "{target}",
  "variant": "codex",
  "entrypoint": "{entrypoint}",
  "execServer": "bin/codex-exec-server",
  "resourcesDir": "codex-resources",
  "pathDir": "codex-path"
}}
"#
    )
}

/// A release of `version` for a Unix `target` at `dir`. An unverified version takes the same
/// shape: what differs between it and a verified one is what the store says, not how it
/// looks.
pub(crate) fn release(tree: &mut FixtureTree, dir: &str, version: &str, target: &str) {
    let manifest = manifest(version, target, "bin/codex");
    tree.file(&format!("{dir}/codex-package.json"), manifest.as_bytes())
        .file(&format!("{dir}/bin/codex"), b"\x7fELF")
        .file(&format!("{dir}/bin/codex-code-mode-host"), b"\x7fELF")
        .file(&format!("{dir}/bin/codex-exec-server"), b"\x7fELF")
        .file(&format!("{dir}/codex-path/rg"), b"\x7fELF")
        .file(&format!("{dir}/codex-resources/bwrap"), b"\x7fELF")
        .file(
            &format!("{dir}/codex-resources/voice/manifest.json"),
            b"{}\n",
        )
        .link(&format!("{dir}/codex"), "bin/codex", Holds::AsWritten);
}

/// A release of `version` for a Windows `target` at `dir`.
pub(crate) fn windows_release(tree: &mut FixtureTree, dir: &str, version: &str, target: &str) {
    let manifest = manifest(version, target, "bin/codex.exe");
    tree.file(&format!("{dir}/codex-package.json"), manifest.as_bytes())
        .file(&format!("{dir}/bin/codex.exe"), b"MZ")
        .file(&format!("{dir}/bin/codex-code-mode-host.exe"), b"MZ")
        .file(&format!("{dir}/bin/codex-exec-server.exe"), b"MZ")
        .file(&format!("{dir}/codex-path/rg.exe"), b"MZ")
        .file(
            &format!("{dir}/codex-resources/codex-command-runner.exe"),
            b"MZ",
        );
}
