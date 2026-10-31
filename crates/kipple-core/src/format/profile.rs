//! `kipple.profile/v1`: a profile, as `<data>/profiles/<name>.json` keeps it (D-068).
//!
//! Strict JSON: an unknown field anywhere is refused, so any addition to the format is
//! `kipple.profile/v2`, and a document of a newer version reads as made by a newer
//! kipple. It holds rule IDs, digests, levels, windows, methods, root paths and
//! identities, and kept and excluded paths without their notes (04 §8a). It carries no
//! digest of itself: without a key that stops no deliberate edit, and the store replaces
//! it atomically, so it is never torn.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::stored_plan::rule_id_ok;
use super::{DigestOut, FormatError, IdentityOut, PROFILE, TextOut, path_out, shape};
use crate::frozen::NameCodec;
use crate::policy::Level;
use crate::profile::{
    EntryScope, FrozenRoot, HeldEntry, HeldKind, ProfileContents, ProfileEntry, ProfileName,
    RuleState,
};
use crate::root::SymbolicRoot;
use crate::rule::{Method, RuleId};

/// The most bytes a profile file may take.
pub const PROFILE_LIMIT: usize = 16 * 1024 * 1024;

/// A profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileOut {
    /// `kipple.profile/v1`.
    pub schema: String,
    /// Its name, which is also its file's.
    #[schemars(regex(pattern = r"^[a-z0-9][a-z0-9-]{0,63}$"))]
    pub name: String,
    /// When it was made, in seconds since 1970 (UTC).
    pub created: u64,
    /// The classification version it was made under.
    pub policy_version: u32,
    /// The kept and excluded paths it was made with, without their notes.
    pub held: Vec<HeldOut>,
    /// One entry per rule proposing removal when it was made, sorted by rule.
    pub entries: Vec<EntryOut>,
}

/// A kept or excluded path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct HeldOut {
    /// Which list.
    pub kind: HeldKind,
    /// The path as configured, `~` expanded.
    pub path: TextOut,
}

/// What a profile froze about one rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntryOut {
    /// The rule.
    #[schemars(regex(pattern = r"^[a-z0-9-]+(\.[a-z0-9-]+)+$"))]
    pub rule: String,
    /// Whether the profile authorizes it.
    pub scope: EntryScope,
    /// The digest of the rule's canonical encoding.
    pub rule_digest: DigestOut,
    /// The digest of its integration's canonical encoding.
    pub integration_digest: DigestOut,
    /// Whether the config turned it on.
    pub enabled: RuleState,
    /// Its effective level.
    pub level: Level,
    /// Its configured recent window, in days.
    #[schemars(range(min = 1))]
    pub recent_days: u16,
    /// Its method.
    pub method: Method,
    /// The roots it resolved to, sorted.
    pub roots: Vec<RootOut>,
}

/// A root a rule resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RootOut {
    /// Which root.
    pub root: SymbolicRoot,
    /// Its directory.
    pub path: TextOut,
    /// Its identity when the scan opened it; `null` when it didn't.
    pub identity: Option<IdentityOut>,
}

/// `contents` as a profile file.
#[must_use]
pub fn encode_profile(contents: &ProfileContents, codec: &dyn NameCodec) -> Vec<u8> {
    let file = ProfileOut {
        schema: PROFILE.to_owned(),
        name: contents.name.as_str().to_owned(),
        created: contents.created,
        policy_version: contents.policy_version,
        held: contents
            .held
            .iter()
            .map(|held| HeldOut {
                kind: held.kind,
                path: path_out(&held.path, codec),
            })
            .collect(),
        entries: contents
            .entries
            .iter()
            .map(|entry| entry_out(entry, codec))
            .collect(),
    };
    // Serializing records of strings, numbers and enums can't fail.
    let mut bytes = serde_json::to_vec_pretty(&file).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

fn entry_out(entry: &ProfileEntry, codec: &dyn NameCodec) -> EntryOut {
    EntryOut {
        rule: entry.rule.0.clone(),
        scope: entry.scope,
        rule_digest: DigestOut(entry.rule_digest),
        integration_digest: DigestOut(entry.integration_digest),
        enabled: entry.enabled,
        level: entry.level,
        recent_days: entry.recent_days,
        method: entry.method,
        roots: entry
            .roots
            .iter()
            .map(|root| RootOut {
                root: root.root,
                path: path_out(&root.path, codec),
                identity: root.identity.map(IdentityOut),
            })
            .collect(),
    }
}

/// The profile a profile file holds.
///
/// # Errors
/// When it is too large, isn't JSON of this shape, is another format or a newer version
/// of this one ([`FormatError::Newer`]), or holds an invalid value: a name, rule ID or
/// path that isn't one, a rule twice, entries or roots out of order, a window of 0 days,
/// or no included entry.
pub(crate) fn decode_profile(
    bytes: &[u8],
    codec: &dyn NameCodec,
) -> Result<ProfileContents, FormatError> {
    // The version is read first, so a newer file isn't refused for its new fields.
    #[derive(Deserialize)]
    struct Versioned {
        schema: String,
    }
    if bytes.len() > PROFILE_LIMIT {
        return Err(FormatError::TooLarge(PROFILE));
    }
    let versioned: Versioned =
        serde_json::from_slice(bytes).map_err(|error| shape(PROFILE, &error))?;
    if versioned.schema != PROFILE {
        return Err(newer_or_other(versioned.schema));
    }
    let file: ProfileOut = serde_json::from_slice(bytes).map_err(|error| shape(PROFILE, &error))?;
    let invalid = |what| FormatError::invalid(PROFILE, what);
    let name = ProfileName::parse(&file.name).ok_or_else(|| invalid("name"))?;
    let held = file
        .held
        .iter()
        .map(|held| {
            Some(HeldEntry {
                kind: held.kind,
                path: held.path.path(codec)?,
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| invalid("held path"))?;
    if !strictly_sorted(&held) {
        return Err(invalid("held list"));
    }
    let entries = file
        .entries
        .iter()
        .map(|entry| read_entry(entry, codec))
        .collect::<Result<Vec<_>, _>>()?;
    let rules: Vec<&RuleId> = entries.iter().map(|entry| &entry.rule).collect();
    if !strictly_sorted(&rules) {
        return Err(invalid("entry list"));
    }
    if !entries
        .iter()
        .any(|entry| entry.scope == EntryScope::Included)
    {
        return Err(invalid("profile without an included entry"));
    }
    Ok(ProfileContents {
        name,
        created: file.created,
        policy_version: file.policy_version,
        held,
        entries,
    })
}

/// Why a document that says it is `schema` isn't read: a newer version of this format, or
/// another format.
fn newer_or_other(schema: String) -> FormatError {
    let newer = schema
        .strip_prefix("kipple.profile/v")
        .and_then(|version| version.parse::<u32>().ok())
        .is_some_and(|version| version > 1);
    if newer {
        FormatError::Newer {
            format: PROFILE,
            found: schema,
        }
    } else {
        FormatError::Schema {
            format: PROFILE,
            found: schema,
        }
    }
}

fn read_entry(entry: &EntryOut, codec: &dyn NameCodec) -> Result<ProfileEntry, FormatError> {
    let invalid = |what| FormatError::invalid(PROFILE, what);
    if !rule_id_ok(&entry.rule) {
        return Err(invalid("rule ID"));
    }
    if entry.recent_days == 0 {
        return Err(invalid("recent window"));
    }
    let roots = entry
        .roots
        .iter()
        .map(|root| {
            Some(FrozenRoot {
                root: root.root,
                path: root.path.path(codec)?,
                identity: root.identity.map(|identity| identity.0),
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| invalid("root path"))?;
    if !strictly_sorted(&roots) {
        return Err(invalid("root list"));
    }
    Ok(ProfileEntry {
        rule: RuleId(entry.rule.clone()),
        scope: entry.scope,
        rule_digest: entry.rule_digest.0,
        integration_digest: entry.integration_digest.0,
        enabled: entry.enabled,
        level: entry.level,
        recent_days: entry.recent_days,
        method: entry.method,
        roots,
    })
}

/// Whether `items` are in order with none twice, as the encoder writes them, so one
/// profile has one encoding.
fn strictly_sorted<T: Ord>(items: &[T]) -> bool {
    items.windows(2).all(|pair| match pair {
        [a, b] => a < b,
        _ => true,
    }) && items.iter().collect::<BTreeSet<_>>().len() == items.len()
}
