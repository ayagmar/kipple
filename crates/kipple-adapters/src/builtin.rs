//! The built-in integrations and the rule packs shipped in the binary.

use kipple_core::{Integration, IntegrationDescriptor, IntegrationId, Pack};

use crate::integrations::{
    Cargo, ClaudeCode, Codex, Cursor, Git, Journald, Node, Npm, Pacman, Pi, Python, Trash,
};
use crate::pack::{self, Catalog, Origin, PackError};

pub use crate::integrations::{Anomaly, Unregistered, registration_advice};

/// Integrations that give advice with no verified layout behind them. They report and find
/// nothing. Every other integration reports only once `verified` names what it was built
/// against.
pub const ADVICE_ONLY: &[IntegrationId] = &[IntegrationId("cursor")];

/// The built-in rule packs, from `rules/` in the repository, by file name.
pub const PACKS: &[(&str, &str)] = &[
    ("cargo.toml", include_str!("../../../rules/cargo.toml")),
    (
        "claude-code.toml",
        include_str!("../../../rules/claude-code.toml"),
    ),
    ("codex.toml", include_str!("../../../rules/codex.toml")),
    ("cursor.toml", include_str!("../../../rules/cursor.toml")),
    ("git.toml", include_str!("../../../rules/git.toml")),
    (
        "journald.toml",
        include_str!("../../../rules/journald.toml"),
    ),
    ("node.toml", include_str!("../../../rules/node.toml")),
    ("npm.toml", include_str!("../../../rules/npm.toml")),
    ("pacman.toml", include_str!("../../../rules/pacman.toml")),
    ("pi.toml", include_str!("../../../rules/pi.toml")),
    ("python.toml", include_str!("../../../rules/python.toml")),
    ("trash.toml", include_str!("../../../rules/trash.toml")),
];

/// Every built-in integration, on every platform. Each descriptor says where it runs.
#[must_use]
pub fn integrations() -> Vec<Box<dyn Integration>> {
    vec![
        Box::new(Cargo),
        Box::new(ClaudeCode),
        Box::new(Codex),
        Box::new(Cursor),
        Box::new(Git),
        Box::new(Journald),
        Box::new(Node),
        Box::new(Npm),
        Box::new(Pacman),
        Box::new(Pi),
        Box::new(Python),
        Box::new(Trash),
    ]
}

/// The descriptors of every built-in integration.
#[must_use]
pub fn descriptors() -> Vec<IntegrationDescriptor> {
    integrations().iter().map(|i| i.descriptor()).collect()
}

/// The built-in packs, validated like any pack against this engine's catalog.
///
/// # Errors
/// Every error of every pack that fails validation, which is a bug in kipple.
pub fn packs(engine: &semver::Version) -> Result<Vec<Pack>, Vec<PackError>> {
    let integrations = descriptors();
    let catalog = Catalog {
        engine,
        integrations: &integrations,
    };
    let mut packs = Vec::new();
    let mut errors = Vec::new();
    for (file, text) in PACKS {
        match pack::validate(file, text.as_bytes(), Origin::Builtin, catalog) {
            Ok(pack) => packs.push(pack),
            Err(found) => errors.extend(found),
        }
    }
    if errors.is_empty() {
        Ok(packs)
    } else {
        Err(errors)
    }
}
