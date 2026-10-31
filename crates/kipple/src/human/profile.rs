//! The words of profiles (03 J6, D-068): what `profile create` and `profile review` show
//! before the question, each change that suspends an entry, `profile list`, a profile
//! run's summary, and why a profile can't be made, read or stored.

use std::path::Path;

use kipple_core::format::FormatError;
use kipple_core::profile::{
    EntryScope, FreezeError, FrozenRoot, HeldKind, ProfileChange, ProfileEntry, ProfileError,
    ProfileStoreError, RuleState,
};

use super::escaped;
use super::outcome::{error_text, unsupported};
use super::receipt::method_word;

/// A value that changed since the profile was made, which suspends its entry.
pub(crate) const fn change(change: ProfileChange) -> &'static str {
    match change {
        ProfileChange::Rule => "the rule changed",
        ProfileChange::Integration => "what finds it changed",
        ProfileChange::Level => "its level changed",
        ProfileChange::RecentWindow => "its recent window changed",
        ProfileChange::Enabled => "it was turned on or off",
        ProfileChange::Method => "its method changed",
        ProfileChange::Roots => "the folders it looks in changed",
        ProfileChange::Held => "a kept or excluded path it was made with is gone",
        ProfileChange::PolicyVersion => "kipple's classification changed with an upgrade",
        ProfileChange::Scope => "it was left out and is to be added",
    }
}

/// Whether a profile entry is authorized.
pub(crate) const fn scope(scope: EntryScope) -> &'static str {
    match scope {
        EntryScope::Included => "included",
        EntryScope::LeftOut => "left out",
    }
}

/// Which list a held path is in.
pub(crate) const fn held(kind: HeldKind) -> &'static str {
    match kind {
        HeldKind::Keep => "kept",
        HeldKind::Exclude => "excluded",
    }
}

/// Whether a rule was turned on.
pub(crate) const fn rule_state(state: RuleState) -> &'static str {
    match state {
        RuleState::Enabled => "on",
        RuleState::Disabled => "off",
    }
}

/// An entry suspended, with what changed.
pub(crate) fn suspended(rule: &str, changes: &[ProfileChange]) -> String {
    let why: Vec<&str> = changes.iter().map(|one| change(*one)).collect();
    format!("{rule}: suspended, {}", why.join("; "))
}

/// A rule the profile has no entry for, which a run leaves out.
pub(crate) fn pending(rule: &str) -> String {
    format!("{rule}: new since the profile was made, not added")
}

/// A rule the profile has an entry for that this kipple doesn't have.
pub(crate) fn gone(rule: &str) -> String {
    format!("{rule}: no longer a rule, ignored")
}

/// A rule `profile review --add` includes.
pub(crate) fn added(rule: &str) -> String {
    format!("{rule}: added")
}

/// The heading over what `profile create` or `profile review` would save for `name`.
pub(crate) fn entries_heading(name: &str) -> String {
    format!("Profile {name} holds each rule as it is now:")
}

/// An entry of a profile about to be saved: its rule, whether it is included and on, its
/// level, recent window and method.
pub(crate) fn entry_line(entry: &ProfileEntry) -> String {
    format!(
        "  {}  {}, {}, {}, recent {} days, {}",
        entry.rule().0,
        scope(entry.scope()),
        rule_state(entry.enabled()),
        super::level(entry.level()),
        entry.recent_days(),
        method_word(entry.method())
    )
}

/// A folder an entry looks in, under its line; one the scan didn't open authorizes
/// nothing.
pub(crate) fn root_line(root: &FrozenRoot) -> String {
    let path = escaped(&root.path().display().to_string());
    match root.identity() {
        Some(_) => format!("      {path}"),
        None => format!("      {path}: not opened, so nothing under it is cleaned"),
    }
}

/// A kept or excluded path the profile holds: removing it from the config suspends it.
pub(crate) fn held_line(kind: HeldKind, path: &Path) -> String {
    format!("  {}: {}", held(kind), escaped(&path.display().to_string()))
}

/// The heading over what a profile about to be saved would clean today.
pub(crate) const TODAY: &str = "Today it would clean:";

/// What a profile about to be saved would clean today, when nothing.
pub(crate) const NOTHING_TODAY: &str = "Today it would clean nothing.";

/// Why `profile create` or `profile review` without a terminal saves nothing.
pub(crate) const NEEDS_TERMINAL: &str =
    "nothing was saved: saving a profile needs a terminal to confirm it";

/// Why `clean --profile` without a terminal and without `--yes` runs nothing.
pub(crate) const RUN_NEEDS_YES: &str = "nothing was changed: confirming needs a terminal; \
     without one, pass --yes to clean what the profile authorizes";

/// What `profile create` or `profile review` says when the person didn't say yes.
pub(crate) const NOT_SAVED: &str = "Nothing was saved.";

/// A profile removed.
pub(crate) fn removed(name: &str) -> String {
    format!("Removed profile {name}.")
}

/// Why the profile `name` can't be used, saved or removed.
pub(crate) fn cant_use(name: &str, why: &str) -> String {
    format!("profile {name}: {why}")
}

/// The heading over the changes `profile review NAME` would freeze.
pub(crate) fn review_heading(name: &str) -> String {
    format!("Profile {name} changes:")
}

/// What `profile review` says when nothing changed.
pub(crate) const NOTHING_TO_REVIEW: &str = "Nothing to review: every entry is as it was made.";

/// Why a profile run cleaned nothing: no entry is active.
pub(crate) fn none_active(name: &str) -> String {
    format!(
        "profile {name} has no active entry, so nothing was changed; run kipple profile review {name}"
    )
}

/// The question `profile create` or `profile review` asks before writing `name`.
pub(crate) fn save_question(name: &str) -> String {
    format!("Save profile {name}? [y/N] ")
}

/// A profile saved.
pub(crate) fn saved(name: &str) -> String {
    format!("Saved profile {name}.")
}

/// What `profile list` says when there are none.
pub(crate) const NO_PROFILES: &str =
    "No profiles yet. `kipple profile create NAME --from-safe` makes one.";

/// A profile's line in `profile list`: its name and how many entries are active,
/// suspended and pending.
pub(crate) fn list_line(name: &str, active: usize, suspended: usize, pending: usize) -> String {
    format!("{name}  {active} active, {suspended} suspended, {pending} pending")
}

/// What `profile list` can't check without a scan.
pub(crate) const ROOTS_UNCHECKED: &str =
    "The folders each entry looks in are checked when a profile runs, not here.";

/// Why a name isn't a profile name.
pub(crate) fn bad_name(text: &str) -> String {
    format!(
        "{} isn't a profile name: use up to 64 lowercase letters, digits and -",
        escaped(text)
    )
}

/// Why a profile couldn't be made.
pub(crate) fn freeze_error(error: &FreezeError) -> String {
    match error {
        FreezeError::ScanIncomplete => "the scan didn't finish, so no profile was made".to_owned(),
        FreezeError::NotRemovable(rule) => {
            format!(
                "{} isn't a rule that removes anything here",
                escaped(&rule.0)
            )
        }
        FreezeError::UnknownIntegration(rule) => {
            format!(
                "{}'s integration isn't known to this kipple",
                escaped(&rule.0)
            )
        }
        FreezeError::Empty => "the profile would include no rule".to_owned(),
    }
}

/// Why a profile couldn't be read or stored.
pub(crate) fn error(error: &ProfileError) -> String {
    match error {
        ProfileError::Store(why) => store_error(why),
        ProfileError::Format(FormatError::Newer { .. }) => {
            "it was made by a newer kipple; upgrade to use it".to_owned()
        }
        ProfileError::Format(why) => format!("it isn't a profile this kipple reads: {why}"),
        ProfileError::Misnamed => "its file holds another profile".to_owned(),
        ProfileError::ReadBack => "what was read back isn't what was written".to_owned(),
    }
}

/// Why the profile store refused.
pub(crate) fn store_error(error: &ProfileStoreError) -> String {
    match error {
        ProfileStoreError::NoDataDir => "kipple's data folder isn't known".to_owned(),
        ProfileStoreError::FileSystem(why) => {
            format!("kipple's data folder is on {}", unsupported(why))
        }
        ProfileStoreError::Exists => "a profile of that name exists".to_owned(),
        ProfileStoreError::Missing => {
            "there is no profile of that name; `kipple profile list` lists them".to_owned()
        }
        ProfileStoreError::Read(why) => format!("it couldn't be read: {}", error_text(why)),
        ProfileStoreError::Write(why) => format!("it couldn't be written: {}", error_text(why)),
        ProfileStoreError::Unsupported => "this build can't store profiles yet".to_owned(),
    }
}
