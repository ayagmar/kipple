//! The digests a profile freezes about each rule it authorizes (D-068): SHA-256 over a
//! canonical encoding of what decides the rule's findings, never over how kipple prints
//! a value.
//!
//! The encoding is a sequence of named fields, each a length-prefixed name then its
//! value: text as its length then its UTF-8 bytes, numbers as eight big-endian bytes,
//! lists as their length then each item, and enum values by the name documents use. Each
//! function takes its value apart field by field, so a field added to [`Rule`] or
//! [`IntegrationDescriptor`] doesn't compile until it is encoded or said to be left out.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::digest::{Sha256, Sha256Hasher};
use crate::integration::{ActivityEvidence, Integration, IntegrationDescriptor, IntegrationId};
use crate::policy::Consequence;
use crate::rule::{Bounds, Name, Operation, Predicate, Rule, Selector, Test};

/// What decides `rule`'s findings.
///
/// Its integration, selector, root, requirements, eligibility, operation, platforms and
/// recent window. Its ID is the profile entry's key, and its text and category are
/// shown, never decide anything, so they are left out.
#[must_use]
pub fn rule_digest(rule: &Rule) -> Sha256 {
    #[expect(
        clippy::unneeded_field_pattern,
        reason = "naming every field left out makes a new field fail to compile until it is encoded or left out here"
    )]
    let Rule {
        id: _,
        integration,
        text: _,
        category: _,
        platforms,
        root,
        selector,
        requirements,
        eligibility,
        operation,
        recent_days,
    } = rule;
    let mut canon = Canon::new("kipple.rule/1");
    canon.field("integration").text(integration.0);
    canon.field("selector");
    selector_into(&mut canon, selector);
    canon.field("root").text(root.name());
    canon.field("requirements");
    predicates_into(&mut canon, requirements);
    canon.field("eligibility");
    predicates_into(&mut canon, eligibility);
    canon.field("operation").text(operation.name());
    let methods = match operation {
        Operation::Report => &[][..],
        Operation::RemoveFiles(methods) => methods.as_slice(),
    };
    canon.count(methods.len());
    for method in methods {
        canon.text(method.name());
    }
    canon.field("platforms").count(platforms.len());
    for platform in platforms {
        canon.text(platform.name());
    }
    canon.field("recent_days");
    canon.optional(recent_days.map(u64::from));
    canon.finish()
}

/// What decides how an integration's findings are found and judged: every field of its
/// descriptor, its revision among them, and what can show its findings in use.
#[must_use]
pub fn integration_digest(
    descriptor: &IntegrationDescriptor,
    evidence: ActivityEvidence,
) -> Sha256 {
    let IntegrationDescriptor {
        id,
        lens,
        platforms,
        roots,
        selectors,
        artifacts,
        owner_files,
        verified,
        consequences,
        revision,
    } = descriptor;
    let mut canon = Canon::new("kipple.integration/1");
    canon.field("id").text(id.0);
    canon.field("revision").number(u64::from(*revision));
    canon.field("lens").text(lens.name());
    canon.field("platforms").count(platforms.len());
    for platform in *platforms {
        canon.text(platform.name());
    }
    canon.field("roots").count(roots.len());
    for root in *roots {
        canon.text(root.name());
    }
    canon.field("selectors").count(selectors.len());
    for selector in *selectors {
        canon.text(selector.name).text(selector.meaning);
    }
    canon.field("artifacts").texts(artifacts);
    canon.field("owner_files").texts(owner_files);
    canon.field("verified").texts(verified);
    canon.field("consequences").count(consequences.len());
    for consequence in *consequences {
        consequence_into(&mut canon, consequence);
    }
    canon.field("activity_evidence");
    match evidence {
        ActivityEvidence::Everything => canon.text("everything"),
        ActivityEvidence::BuiltExecutables => canon.text("built-executables"),
        ActivityEvidence::Interpreted { runtimes } => canon.text("interpreted").texts(runtimes),
    };
    canon.finish()
}

/// Each integration's [`integration_digest`], as a profile is frozen and checked with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntegrationDigests(pub(crate) BTreeMap<IntegrationId, Sha256>);

impl IntegrationDigests {
    /// The digests of `integrations`, from what each declares about itself.
    #[must_use]
    pub fn of(integrations: &[Box<dyn Integration>]) -> Self {
        Self(
            integrations
                .iter()
                .map(|integration| {
                    let descriptor = integration.descriptor();
                    let digest = integration_digest(&descriptor, integration.activity_evidence());
                    (descriptor.id, digest)
                })
                .collect(),
        )
    }

    /// The digest of the integration `id`, if it is one of them.
    #[must_use]
    pub fn get(&self, id: IntegrationId) -> Option<Sha256> {
        self.0.get(&id).copied()
    }
}

fn selector_into(canon: &mut Canon, selector: &Selector) {
    canon.text(selector.name());
    match selector {
        Selector::DirsMatching {
            pattern,
            max_matches,
        }
        | Selector::FilesMatching {
            pattern,
            max_matches,
        } => {
            canon.count(pattern.0.len());
            for part in &pattern.0 {
                canon.text(part);
            }
            canon.number(u64::from(*max_matches));
        }
        Selector::ProjectArtifact {
            markers,
            artifact,
            max_depth,
        } => {
            canon.count(markers.len());
            for Name(marker) in markers {
                canon.text(marker);
            }
            canon.text(&artifact.0).number(u64::from(*max_depth));
        }
        // The name is all the rule says: what it selects is the integration's code, whose
        // revision the integration digest holds.
        Selector::Integration(_) => {}
    }
}

fn predicates_into(canon: &mut Canon, predicates: &[Predicate]) {
    canon.count(predicates.len());
    for Predicate { fact, test } in predicates {
        canon.text(fact);
        match test {
            Test::OneOf(values) => {
                canon.text("one-of").texts(values);
            }
            Test::Is(value) => {
                canon.text("is").text(if *value { "true" } else { "false" });
            }
            Test::Duration(bounds) => {
                canon.text("duration");
                bounds_into(canon, bounds, nanos);
            }
            Test::Bytes(bounds) => {
                canon.text("bytes");
                bounds_into(canon, bounds, |bytes| u128::from(*bytes));
            }
            Test::Count(bounds) => {
                canon.text("count");
                bounds_into(canon, bounds, |count| u128::from(*count));
            }
        }
    }
}

const fn nanos(duration: &Duration) -> u128 {
    duration.as_nanos()
}

fn bounds_into<T>(canon: &mut Canon, bounds: &Bounds<T>, value: impl Fn(&T) -> u128) {
    for bound in [&bounds.min, &bounds.max] {
        match bound {
            None => canon.text("none"),
            Some(bound) => canon.text("some").wide(value(bound)),
        };
    }
}

fn consequence_into(canon: &mut Canon, consequence: &Consequence) {
    let Consequence {
        recovery,
        loss,
        cost,
    } = consequence;
    canon.text(recovery.name()).count(loss.len());
    for one in *loss {
        canon.text(one.name());
    }
    canon.text(cost.name());
}

/// Writes a canonical encoding into a SHA-256.
struct Canon(Sha256Hasher);

impl Canon {
    /// An encoding of the kind `kind`, so two kinds of value never share bytes.
    fn new(kind: &str) -> Self {
        let mut canon = Self(Sha256Hasher::new());
        canon.text(kind);
        canon
    }

    fn field(&mut self, name: &str) -> &mut Self {
        self.text(name)
    }

    fn text(&mut self, text: &str) -> &mut Self {
        self.count(text.len());
        self.0.bytes(text.as_bytes());
        self
    }

    fn texts(&mut self, texts: &[&str]) -> &mut Self {
        self.count(texts.len());
        for text in texts {
            self.text(text);
        }
        self
    }

    fn number(&mut self, number: u64) -> &mut Self {
        self.0.bytes(&number.to_be_bytes());
        self
    }

    fn wide(&mut self, number: u128) -> &mut Self {
        self.0.bytes(&number.to_be_bytes());
        self
    }

    fn count(&mut self, count: usize) -> &mut Self {
        self.number(count as u64)
    }

    fn optional(&mut self, number: Option<u64>) -> &mut Self {
        match number {
            None => self.text("none"),
            Some(number) => self.text("some").number(number),
        }
    }

    fn finish(self) -> Sha256 {
        self.0.finish()
    }
}
