//! Pi 1.5.0's agent directory as a managed install leaves it, from the listing in S5 (Pi §3).

use super::tree::{FixtureTree, Holds};

/// The version this fixture is.
pub(crate) const VERSION: &str = "1.5.0";

/// The marker of a managed install, as observed.
pub(crate) const MARKER: &str = "{\n  \"kind\": \"pi-managed-install\",\n  \"schemaVersion\": 1,\n  \
     \"layout\": \"releases-v1\",\n  \"entrypoint\": { \"type\": \"symlink\", \"path\": \
     \"/home/u/.local/bin/pi\" }\n}\n";

/// A managed install in the agent directory at `at`, with the release `version` active.
pub(crate) fn install(tree: &mut FixtureTree, at: &str, version: &str) {
    let install = format!("{at}/install");
    tree.file(
        &format!("{install}/managed-install.json"),
        MARKER.as_bytes(),
    )
    .file(
        &format!("{install}/current-version"),
        format!("{version}\n").as_bytes(),
    )
    .file(
        &format!("{install}/releases/{version}/package.json"),
        b"{\"name\":\"@earendil-works/pi-coding-agent-install\"}\n",
    )
    .file(
        &format!("{install}/releases/{version}/node_modules/.bin/pi"),
        b"#!/usr/bin/env node\n",
    );
}

/// The four logs the removing rule names, and the active one it doesn't.
pub(crate) fn logs(tree: &mut FixtureTree, at: &str) {
    for name in [
        "pi-debug.log",
        "pi-tui-crash.log",
        "pi-tui-debug.log",
        "mcp.log.1",
    ] {
        tree.file(&format!("{at}/{name}"), b"diagnostics\n");
    }
    tree.file(&format!("{at}/mcp.log"), b"active\n");
}

/// The agent directory at `at`: a managed install of this version, its logs, and everything
/// of §3 that must stay closed.
pub(crate) fn store(tree: &mut FixtureTree, at: &str) {
    install(tree, at, VERSION);
    logs(tree, at);
    let mut file = |name: &str, contents: &[u8]| {
        tree.file(&format!("{at}/{name}"), contents);
    };
    file("auth.json", b"{\"token\":\"credential-value\"}\n");
    file("mcp-auth.json", b"{\"token\":\"credential-value\"}\n");
    file("settings.json", b"{}\n");
    file("models-store.json", b"{}\n");
    file("trust.json", b"{}\n");
    file("crashes.json", b"[]\n");
    file("bin/pi", b"#!/bin/sh\n");
    file("bin/fd", b"\x7fELF");
    file("bin/rg", b"\x7fELF");
    file(
        "sessions/--home-u-code--/2026-11-02T10-00-00_a.jsonl",
        b"{}\n",
    );
    file("extensions/mine/index.ts", b"\n");
    file("git/.gitignore", b"*\n!.gitignore\n");
    file("npm/package.json", b"{}\n");
    file("tmp/extensions/npm/0a1b2c3d/index.js", b"\n");
    file("install/releases/1.4.0/package.json", b"{}\n");
    file("install/staging/update-a1B2c3/package.json", b"{}\n");
    tree.link(
        &format!("{at}/AGENTS.md"),
        "../../.agents/AGENTS.md",
        Holds::AsWritten,
    );
}
