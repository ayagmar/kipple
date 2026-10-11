//! Claude Code's user settings, `settings.json` in its configuration directory.

use super::tree::FixtureTree;

/// A configuration directory at `at` whose settings keep transcripts for `days`.
pub(crate) fn config(tree: &mut FixtureTree, at: &str, days: u32) {
    let settings = format!("{{\n  \"cleanupPeriodDays\": {days}\n}}\n");
    tree.file(&format!("{at}/settings.json"), settings.as_bytes());
}
