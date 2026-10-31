//! Claude Code: advice on its own retention setting, `cleanupPeriodDays`
//! (`docs/06-adapters-and-rules.md` §6). Checked against the settings and `.claude`
//! directory documentation on 2026-10-11. kipple finds nothing here yet: Claude Code's
//! files arrive with spike S5.

use kipple_core::{
    Advice, FindingSink, Integration, IntegrationDescriptor, IntegrationError, IntegrationId, Lens,
    Platform, ScanContext, SymbolicRoot,
};

/// Claude Code's default retention, in days.
const DEFAULT_DAYS: u64 = 30;

/// More than any settings file should hold.
const SETTINGS_LIMIT: u64 = 1024 * 1024;

/// The Claude Code integration.
#[derive(Debug)]
pub(crate) struct ClaudeCode;

impl Integration for ClaudeCode {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("claude-code"),
            lens: Lens::Agents,
            platforms: Platform::ALL,
            roots: &[SymbolicRoot::ClaudeConfig],
            selectors: &[],
            artifacts: &[],
            owner_files: &[],
            verified: &[],
            consequences: &[],
            revision: 1,
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let Some(root) = cx.roots_of(SymbolicRoot::ClaudeConfig).next() else {
            return Ok(());
        };
        let file = root.path.join("settings.json");
        let default = || format!("{DEFAULT_DAYS} days, the default");
        let setting = match cx.read_file(&file, SETTINGS_LIMIT) {
            Ok(bytes) => match cleanup_period(&bytes) {
                Setting::Unset => default(),
                Setting::Days(days) => format!("{days} days, from {}", file.display()),
                Setting::Unusable => format!("unknown: {} can't be used", file.display()),
            },
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => default(),
            Err(error) => format!("unknown: {error}"),
        };
        sink.advise(advice(&setting));
        Ok(())
    }
}

/// `cleanupPeriodDays` as a settings file sets it.
enum Setting {
    /// Not set, so the default applies.
    Unset,
    /// Set to a valid number of days.
    Days(u64),
    /// The file isn't a JSON object, or the value isn't a number of days.
    Unusable,
}

fn cleanup_period(bytes: &[u8]) -> Setting {
    let Ok(serde_json::Value::Object(settings)) = serde_json::from_slice(bytes) else {
        return Setting::Unusable;
    };
    let Some(days) = settings.get("cleanupPeriodDays") else {
        return Setting::Unset;
    };
    days.as_u64()
        .filter(|&days| days >= 1)
        .map_or(Setting::Unusable, Setting::Days)
}

fn advice(setting: &str) -> Advice {
    Advice {
        title: "Claude Code's own transcript retention".to_owned(),
        text: format!(
            "cleanupPeriodDays in your user settings: {setting}. Claude Code deletes \
             transcripts, file history, plans, debug logs and other session data older than \
             this. The default is 30 and the minimum 1; 0 fails validation. Managed \
             settings, and project or command-line settings, override the user setting, and \
             kipple doesn't read them. It never deletes auto memory files (versions before \
             v2.1.228 could delete old files below a memory directory), keeps Claude \
             Desktop and Cowork transcripts unless desktopSessionCleanupPeriodDays is set, \
             and doesn't sweep in --bare sessions."
        ),
        snippets: vec!["{ \"cleanupPeriodDays\": 14 }".to_owned()],
        references: vec![
            "https://code.claude.com/docs/en/claude-directory#cleaned-up-automatically".to_owned(),
            "https://code.claude.com/docs/en/settings".to_owned(),
        ],
    }
}
