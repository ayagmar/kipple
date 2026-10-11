//! The support matrix the docs site renders (`docs/09-site-and-docs.md` §6): each
//! integration, where it runs, and what it does there.

use kipple_core::Platform;
use schemars::JsonSchema;
use serde::Serialize;

use crate::json::{LensOut, SUPPORT_MATRIX, VERSION};
use crate::setup::Builtin;

/// The generated support matrix.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SupportMatrixOut {
    /// `kipple.support-matrix/v1`.
    pub schema: &'static str,
    /// kipple's version.
    pub kipple: &'static str,
    /// Every built-in integration, sorted by ID.
    pub integrations: Vec<SupportOut>,
}

/// One integration's support.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SupportOut {
    /// Its ID.
    pub id: &'static str,
    /// Its lens.
    pub lens: LensOut,
    /// The upstream versions its layouts were verified against.
    pub verified: Vec<&'static str>,
    /// Its rules.
    pub rules: Vec<String>,
    /// What it does on each platform.
    pub platforms: Vec<PlatformSupport>,
}

/// What an integration does on one platform.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PlatformSupport {
    /// The platform.
    pub platform: &'static str,
    /// Whether it finds things there.
    pub discover: bool,
    /// Whether it reports sizes or advice there.
    pub report: bool,
    /// Whether it may change anything there. Never, in this version.
    pub mutate: bool,
}

impl SupportMatrixOut {
    /// The matrix of `builtin`.
    #[must_use]
    pub fn new(builtin: &Builtin) -> Self {
        let integrations = builtin
            .descriptors
            .iter()
            .map(|d| SupportOut {
                id: d.id.0,
                lens: d.lens.into(),
                verified: d.verified.to_vec(),
                rules: builtin
                    .rules()
                    .filter(|(_, rule)| rule.integration == d.id)
                    .map(|(_, rule)| rule.id.0.clone())
                    .collect(),
                platforms: Platform::ALL
                    .iter()
                    .map(|&platform| {
                        let supported = d.platforms.contains(&platform);
                        // Advice alone, as for Claude Code, finds nothing.
                        let finds = builtin.rules().any(|(_, rule)| {
                            rule.integration == d.id && rule.platforms.contains(&platform)
                        });
                        PlatformSupport {
                            platform: platform.name(),
                            discover: supported && finds,
                            report: supported,
                            mutate: false,
                        }
                    })
                    .collect(),
            })
            .collect();
        Self {
            schema: SUPPORT_MATRIX,
            kipple: VERSION,
            integrations,
        }
    }
}
