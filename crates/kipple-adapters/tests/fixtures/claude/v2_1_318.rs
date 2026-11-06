//! Claude Code 2.1.318 as a native Linux install leaves it, from the listing in S5 (Claude §3):
//! the configuration directory and, apart from it, the launcher, versions and locks.

use super::tree::{FixtureTree, Holds};

/// The version this fixture is.
pub(crate) const VERSION: &str = "2.1.318";

/// A session whose marker `config` writes, so its log is open.
pub(crate) const OPEN_SESSION: &str = "1f0c9d7e-6b2a-4c11-9a3e-7d5e2b8c4a10";

/// A session with no marker, so its log is closed.
pub(crate) const CLOSED_SESSION: &str = "a3b1e2d4-58c7-4f9a-b6d0-2c9e7f1a3d55";

/// The marker of a running session. The key names are observed, not documented.
pub(crate) fn marker(session: &str) -> String {
    format!(
        "{{\"pid\":4242,\"procStart\":\"812734\",\"sessionId\":\"{session}\",\
         \"cwd\":\"/home/u/code\",\"version\":\"{VERSION}\",\"startedAt\":1793000000000}}\n"
    )
}

/// The configuration directory at `at`, with one open session and two logs, and everything
/// of §3 that must stay closed.
pub(crate) fn config(tree: &mut FixtureTree, at: &str) {
    let mut file = |name: &str, contents: &[u8]| {
        tree.file(&format!("{at}/{name}"), contents);
    };
    file("settings.json", b"{\n  \"model\": \"opus\"\n}\n");
    file("settings.local.json", b"{}\n");
    file(".credentials.json", b"{\"token\":\"credential-value\"}\n");
    file("CLAUDE.md", b"# Notes\n");
    file("history.jsonl", b"{\"display\":\"history-value\"}\n");
    file(&format!("debug/{OPEN_SESSION}.txt"), b"[DEBUG] open\n");
    file(&format!("debug/{CLOSED_SESSION}.txt"), b"[DEBUG] closed\n");
    file("sessions/4242.json", marker(OPEN_SESSION).as_bytes());
    let key = format!("sessions/4242.{}.key", "ab".repeat(32));
    file(&key, b"key-value");
    file("daemon/control.key", b"key-value");
    file("daemon/roster.json", b"{\"proto\":1}\n");
    file("daemon.log", b"started\n");
    file("projects/-home-u-code/transcript.jsonl", b"{}\n");
    file("projects/-home-u-code/memory/MEMORY.md", b"# Memory\n");
    file("file-history/session/a.txt", b"a\n");
    file("backups/.claude.json.backup.1793000000000", b"{}\n");
    file("plugins/installed_plugins.json", b"{}\n");
    file("skills/mine/SKILL.md", b"# Skill\n");
    file(
        "shell-snapshots/snapshot-zsh-1793000000000-a1b2c3.sh",
        b"# zsh\n",
    );
    file("session-env/a3b1e2d4-58c7-4f9a-b6d0-2c9e7f1a3d55/env", b"");
    file("plans/plan.md", b"# Plan\n");
    file("cache/changelog.md", b"# Changelog\n");
    file("telemetry/1p_failed_events.json", b"{}\n");
    tree.link(
        &format!("{at}/debug/latest"),
        &format!("{at}/debug/{OPEN_SESSION}.txt"),
        Holds::AbsoluteInTree,
    );
}

/// The native install of `version` below the home directory at `home`: the launcher, the
/// version it points at, a rollback, and per-process locks.
pub(crate) fn install(tree: &mut FixtureTree, home: &str, version: &str) {
    let versions = format!("{home}/.local/share/claude/versions");
    tree.file(&format!("{versions}/{version}"), b"\x7fELF")
        .file(&format!("{versions}/2.1.316"), b"\x7fELF")
        .file(
            &format!("{home}/.local/state/claude/locks/{version}#4242.lock"),
            b"{\"pid\":4242}\n",
        )
        .link(
            &format!("{home}/.local/bin/claude"),
            &format!("{versions}/{version}"),
            Holds::AbsoluteInTree,
        );
}
