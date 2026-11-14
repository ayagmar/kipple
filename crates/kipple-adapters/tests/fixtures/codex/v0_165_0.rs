//! A release as Codex 0.165.0 lays it out, from S5 codex §3: no `execServer` in its manifest
//! and two files in `bin/`.

use super::tree::{FixtureTree, Holds};

/// The version this layout was read from.
pub(crate) const VERSION: &str = "0.165.0";

/// A release of `version` for `target` at `dir`. An unverified version takes the same shape:
/// what differs between it and a verified one is what the store says, not how it looks.
pub(crate) fn release(tree: &mut FixtureTree, dir: &str, version: &str, target: &str) {
    let manifest = format!(
        r#"{{
  "layoutVersion": 1,
  "version": "{version}",
  "target": "{target}",
  "variant": "codex",
  "entrypoint": "bin/codex",
  "resourcesDir": "codex-resources",
  "pathDir": "codex-path"
}}
"#
    );
    tree.file(&format!("{dir}/codex-package.json"), manifest.as_bytes())
        .file(&format!("{dir}/bin/codex"), b"\x7fELF")
        .file(&format!("{dir}/bin/codex-code-mode-host"), b"\x7fELF")
        .file(&format!("{dir}/codex-path/rg"), b"\x7fELF")
        .file(&format!("{dir}/codex-resources/bwrap"), b"\x7fELF")
        .file(
            &format!("{dir}/codex-resources/voice/manifest.json"),
            b"{}\n",
        )
        .link(&format!("{dir}/codex"), "bin/codex", Holds::AsWritten);
}
