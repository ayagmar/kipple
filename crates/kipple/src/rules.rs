//! `kipple rules` (`docs/03-functional-spec.md` §3, `docs/06-adapters-and-rules.md` §2):
//! the integrations, the built-in rules, where each runs, and the capability catalog
//! rules may use.

use std::io::{self, Write};

use kipple_core::{
    FactKind, GENERIC_FACTS, GENERIC_SELECTORS, IntegrationDescriptor, OPERATIONS, Pack, Rule,
    Selector, SymbolicRoot,
};
use kipple_platform::PLATFORM;
use schemars::JsonSchema;
use serde::Serialize;

use crate::human::escaped;
use crate::json::{LensOut, RULES, VERSION};
use crate::setup::Builtin;

/// `kipple rules --json`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RulesOut {
    /// `kipple.rules/v1`.
    pub schema: &'static str,
    /// kipple's version.
    pub kipple: &'static str,
    /// The platform it runs on.
    pub platform: &'static str,
    /// Every built-in integration, sorted by ID.
    pub integrations: Vec<IntegrationInfo>,
    /// Every built-in rule, sorted by pack, then document order.
    pub rules: Vec<RuleOut>,
    /// What rules may reference.
    pub capabilities: Capabilities,
}

/// A built-in integration.
#[derive(Debug, Serialize, JsonSchema)]
pub struct IntegrationInfo {
    /// Its ID.
    pub id: &'static str,
    /// Its lens.
    pub lens: LensOut,
    /// The platforms it supports.
    pub platforms: Vec<&'static str>,
    /// Whether it runs on this platform.
    pub supported: bool,
    /// Roots it reads besides its rules' roots.
    pub roots: Vec<&'static str>,
    /// The upstream versions its layouts were verified against.
    pub verified: Vec<&'static str>,
}

/// A rule.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RuleOut {
    /// Its ID.
    pub id: String,
    /// Its pack.
    pub pack: String,
    /// Its pack's version.
    pub pack_version: String,
    /// The integration that runs it.
    pub integration: &'static str,
    /// A short title.
    pub title: String,
    /// A longer description, possibly empty.
    pub description: String,
    /// Why this exists on the machine.
    pub why: String,
    /// What removing it loses.
    pub loss: String,
    /// Upstream documentation.
    pub references: Vec<String>,
    /// What its findings are. Display only.
    pub category: &'static str,
    /// Where it applies.
    pub platforms: Vec<&'static str>,
    /// Whether it runs on this platform.
    pub supported: bool,
    /// The root it selects under.
    pub root: &'static str,
    /// How it selects.
    pub selector: SelectorOut,
    /// What may be done with its findings.
    pub operation: &'static str,
}

/// A rule's selector and its parameters.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SelectorOut {
    /// The selector's name.
    pub selection: &'static str,
    /// `project-artifact`: the marker files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<Vec<String>>,
    /// `project-artifact`: the artifact directory's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// `project-artifact`: how deep a candidate may be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// `dirs-matching` and `files-matching`: the pattern.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// `dirs-matching` and `files-matching`: how many matches are kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_matches: Option<u32>,
}

/// The published capability set.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Capabilities {
    /// Symbolic roots.
    pub roots: Vec<&'static str>,
    /// Selectors: the generic ones, then each integration's.
    pub selectors: Vec<SelectorInfo>,
    /// Facts rules may test.
    pub facts: Vec<FactInfo>,
    /// Operations.
    pub operations: Vec<&'static str>,
}

/// A selector rules may use.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SelectorInfo {
    /// Its name.
    pub name: &'static str,
    /// The integration that publishes it, or `None` for a generic selector any pack
    /// may use.
    pub integration: Option<&'static str>,
    /// Its parameters.
    pub parameters: Vec<&'static str>,
    /// What it selects, for an integration's selector.
    pub meaning: Option<&'static str>,
}

/// A fact rules may test.
#[derive(Debug, Serialize, JsonSchema)]
pub struct FactInfo {
    /// Its name.
    pub name: &'static str,
    /// Its type: `enum`, `bool`, `duration`, `bytes` or `count`.
    pub kind: &'static str,
    /// An enum fact's values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<&'static str>>,
    /// The one selector it applies to, or `None` for every selector.
    pub selector: Option<&'static str>,
    /// What it means.
    pub meaning: &'static str,
}

impl RulesOut {
    /// The catalog of `builtin`.
    #[must_use]
    pub fn new(builtin: &Builtin) -> Self {
        let integrations = builtin.descriptors.iter().map(integration).collect();
        let rules = builtin
            .rules()
            .map(|(pack, rule)| rule_out(pack, rule))
            .collect();
        Self {
            schema: RULES,
            kipple: VERSION,
            platform: PLATFORM.name(),
            integrations,
            rules,
            capabilities: capabilities(&builtin.descriptors),
        }
    }
}

fn names<T: Copy>(items: &[T], name: fn(T) -> &'static str) -> Vec<&'static str> {
    items.iter().map(|&item| name(item)).collect()
}

fn integration(descriptor: &IntegrationDescriptor) -> IntegrationInfo {
    IntegrationInfo {
        id: descriptor.id.0,
        lens: descriptor.lens.into(),
        platforms: names(descriptor.platforms, kipple_core::Platform::name),
        supported: descriptor.platforms.contains(&PLATFORM),
        roots: names(descriptor.roots, SymbolicRoot::name),
        verified: descriptor.verified.to_vec(),
    }
}

fn rule_out(pack: &Pack, rule: &Rule) -> RuleOut {
    RuleOut {
        id: rule.id.0.clone(),
        pack: pack.id.0.clone(),
        pack_version: pack.version.clone(),
        integration: rule.integration.0,
        title: rule.text.title.clone(),
        description: rule.text.description.clone(),
        why: rule.text.why.clone(),
        loss: rule.text.loss.clone(),
        references: rule.text.references.clone(),
        category: rule.category.name(),
        platforms: names(&rule.platforms, kipple_core::Platform::name),
        supported: rule.platforms.contains(&PLATFORM),
        root: rule.root.name(),
        selector: selector(&rule.selector),
        operation: rule.operation.name(),
    }
}

fn selector(selector: &Selector) -> SelectorOut {
    let mut out = SelectorOut {
        selection: selector.name(),
        marker: None,
        artifact: None,
        max_depth: None,
        pattern: None,
        max_matches: None,
    };
    match selector {
        Selector::ProjectArtifact {
            markers,
            artifact,
            max_depth,
        } => {
            out.marker = Some(markers.iter().map(|m| m.0.clone()).collect());
            out.artifact = Some(artifact.0.clone());
            out.max_depth = Some(*max_depth);
        }
        Selector::DirsMatching {
            pattern,
            max_matches,
        }
        | Selector::FilesMatching {
            pattern,
            max_matches,
        } => {
            out.pattern = Some(pattern.0.join("/"));
            out.max_matches = Some(*max_matches);
        }
        Selector::Integration(_) => {}
    }
    out
}

fn capabilities(descriptors: &[IntegrationDescriptor]) -> Capabilities {
    let generic = GENERIC_SELECTORS
        .iter()
        .map(|(name, parameters)| SelectorInfo {
            name,
            integration: None,
            parameters: parameters.to_vec(),
            meaning: None,
        });
    let published = descriptors.iter().flat_map(|d| {
        d.selectors.iter().map(|s| SelectorInfo {
            name: s.name,
            integration: Some(d.id.0),
            parameters: Vec::new(),
            meaning: Some(s.meaning),
        })
    });
    let facts = GENERIC_FACTS
        .iter()
        .map(|fact| {
            let (kind, values) = match fact.kind {
                FactKind::Enum(values) => ("enum", Some(values.to_vec())),
                FactKind::Bool => ("bool", None),
                FactKind::Duration => ("duration", None),
                FactKind::Bytes => ("bytes", None),
                FactKind::Count => ("count", None),
            };
            FactInfo {
                name: fact.name,
                kind,
                values,
                selector: fact.selector,
                meaning: fact.meaning,
            }
        })
        .collect();
    Capabilities {
        roots: names(SymbolicRoot::ALL, SymbolicRoot::name),
        selectors: generic.chain(published).collect(),
        facts,
        operations: OPERATIONS.to_vec(),
    }
}

/// Writes the catalog for people.
///
/// # Errors
/// When writing to `out` fails.
pub fn write_human(out: &mut dyn Write, rules: &RulesOut) -> io::Result<()> {
    writeln!(out, "kipple {} rules on {}", rules.kipple, rules.platform)?;
    writeln!(out, "\nIntegrations")?;
    for i in &rules.integrations {
        let here = if i.supported {
            "runs here"
        } else {
            "not on this OS"
        };
        let line = format!(
            "{:<12} {:<7} {:<22} {here}",
            i.id,
            lens(i.lens),
            i.platforms.join(", ")
        );
        writeln!(out, "  {}", line.trim_end())?;
    }
    writeln!(out, "\nRules (all report only in this version)")?;
    for r in &rules.rules {
        let here = if r.supported {
            ""
        } else {
            "  (not on this OS)"
        };
        writeln!(
            out,
            "  {:<28} {:<19} {}{here}",
            r.id,
            r.category,
            escaped(&r.title)
        )?;
    }
    writeln!(
        out,
        "\n`kipple explain RULE` shows what a rule selects and why."
    )?;
    out.flush()
}

pub(crate) const fn lens(lens: LensOut) -> &'static str {
    match lens {
        LensOut::Agents => "agents",
        LensOut::Dev => "dev",
        LensOut::System => "system",
    }
}
