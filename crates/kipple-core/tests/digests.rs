//! What a profile entry's digests cover (D-068, spec M4 pinned 5). A field left out of a
//! digest keeps an entry live after the thing it froze changed.

use kipple_core::format::{integration_digest, rule_digest};
use kipple_core::{
    ActivityEvidence, Category, FactKind, FactSpec, GroupKey, IntegrationDescriptor, IntegrationId,
    Lens, Method, Operation, Platform, Retention, Rule, RuleId, RuleText, Selector,
    StateDeclaration, SymbolicRoot, Unresolved,
};

fn release_rule(group_by: Vec<GroupKey>) -> Rule {
    Rule {
        id: RuleId("builtin.test.releases".to_owned()),
        integration: IntegrationId("test"),
        text: RuleText {
            title: String::new(),
            description: String::new(),
            why: String::new(),
            loss: String::new(),
            references: Vec::new(),
        },
        category: Category::ReleaseInstall,
        platforms: vec![Platform::Linux],
        root: SymbolicRoot::CodexReleaseStore,
        selector: Selector::Integration("verified-release-directories"),
        requirements: Vec::new(),
        eligibility: Vec::new(),
        operation: Operation::RemoveVerifiedRelease {
            methods: vec![Method::Delete],
            retention: Retention { group_by },
        },
        recent_days: None,
    }
}

const DESCRIPTOR: IntegrationDescriptor = IntegrationDescriptor {
    id: IntegrationId("test"),
    lens: Lens::Agents,
    platforms: Platform::ALL,
    roots: &[],
    selectors: &[],
    artifacts: &[],
    owner_files: &[],
    verified: &[],
    consequences: &[],
    facts: &[],
    state: &[],
    evidences: &[ActivityEvidence::BuiltExecutables],
    revision: 1,
};

// Catches a changed keep set leaving a profile entry live: the rule would remove more than
// the person who saved the profile agreed to.
#[test]
fn rule_retention_is_in_its_digest() {
    assert_ne!(
        rule_digest(&release_rule(vec![GroupKey::Component])),
        rule_digest(&release_rule(vec![GroupKey::Component, GroupKey::Target])),
    );
}

// Catches an integration loosening what counts as activity, by any declared evidence and not
// just the first, without suspending the profiles that froze it.
#[test]
fn evidences_are_in_the_integration_digest() {
    let more = IntegrationDescriptor {
        evidences: &[
            ActivityEvidence::BuiltExecutables,
            ActivityEvidence::Everything,
        ],
        ..DESCRIPTOR
    };
    assert_ne!(integration_digest(&DESCRIPTOR), integration_digest(&more));
}

// Catches a change to what a tool's state protects, or to the facts its rules test, leaving
// a profile live.
#[test]
fn state_and_facts_are_in_the_integration_digest() {
    let state = IntegrationDescriptor {
        state: &[StateDeclaration {
            root: SymbolicRoot::CodexHome,
            protected: &[],
            open: &[],
            delegates: &[],
            unresolved: Unresolved::HoldOwn,
        }],
        ..DESCRIPTOR
    };
    let facts = IntegrationDescriptor {
        facts: &[FactSpec {
            name: "layout",
            kind: FactKind::Enum(&["supported"]),
            selector: None,
            meaning: "whether its layout is one kipple verified",
        }],
        ..DESCRIPTOR
    };
    assert_ne!(integration_digest(&DESCRIPTOR), integration_digest(&state));
    assert_ne!(integration_digest(&DESCRIPTOR), integration_digest(&facts));
}
