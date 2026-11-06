//! The support matrix the docs site renders (`docs/09-site-and-docs.md` §6): each
//! integration, where it runs, and what it does there.

use kipple_adapters::builtin::ADVICE_ONLY;
use kipple_core::{IntegrationDescriptor, Operation, Platform};
use schemars::JsonSchema;
use serde::Serialize;

use crate::json::{LensOut, SUPPORT_MATRIX, VERSION};
use crate::rules::{self, FactInfo, IntegrationInfo, OpenInfo, ProtectedRoot};
use crate::setup::{Builtin, BuiltinRule};

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
    /// The facts its findings can supply.
    pub facts: Vec<FactInfo>,
    /// Its tool's state roots: closed, whether or not it runs here, except for `open`.
    pub protected: Vec<ProtectedRoot>,
    /// What its state roots open, and to whom.
    pub open: Vec<OpenInfo>,
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
    /// Whether one of its rules may change anything there.
    pub mutate: bool,
}

impl SupportMatrixOut {
    /// The matrix of `builtin`.
    #[must_use]
    pub fn new(builtin: &Builtin) -> Self {
        let integrations = builtin
            .descriptors
            .iter()
            .map(|descriptor| support(builtin, descriptor))
            .collect();
        Self {
            schema: SUPPORT_MATRIX,
            kipple: VERSION,
            integrations,
        }
    }
}

fn support(builtin: &Builtin, descriptor: &IntegrationDescriptor) -> SupportOut {
    let IntegrationInfo {
        id,
        lens,
        verified,
        facts,
        protected,
        open,
        ..
    } = rules::integration(descriptor);
    let own = || {
        builtin
            .rules()
            .filter(|BuiltinRule { rule, .. }| rule.integration == descriptor.id)
    };
    SupportOut {
        id,
        lens,
        verified,
        facts,
        protected,
        open,
        rules: own()
            .map(|BuiltinRule { rule, .. }| rule.id.0.clone())
            .collect(),
        platforms: Platform::ALL
            .iter()
            .map(|&platform| {
                let supported = descriptor.platforms.contains(&platform);
                let here: Vec<BuiltinRule<'_>> = own()
                    .filter(|found| found.rule.platforms.contains(&platform))
                    .collect();
                let removes = |found: &BuiltinRule<'_>| found.rule.operation != Operation::Report;
                // A rule proves nothing by itself: a stub has its rules and finds nothing
                // until `verified` names the layout it was built against.
                let built = !descriptor.verified.is_empty();
                PlatformSupport {
                    platform: platform.name(),
                    discover: supported && built && !here.is_empty(),
                    report: supported && (built || ADVICE_ONLY.contains(&descriptor.id)),
                    mutate: supported && here.iter().any(removes),
                }
            })
            .collect(),
    }
}
