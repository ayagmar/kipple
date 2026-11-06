//! What `kipple rules --json` and the support matrix publish about operations and
//! integrations (`docs/06-adapters-and-rules.md` §2): one catalog, read from the same tables
//! the validator and the host use.
#![expect(
    clippy::unwrap_used,
    reason = "a catalog that can't be read fails the test"
)]

use kipple::rules::RulesOut;
use kipple::setup::Builtin;
use kipple::support::SupportMatrixOut;
use kipple_core::OPERATIONS;
use serde_json::{Value, json};

fn rules(builtin: &Builtin) -> Value {
    serde_json::to_value(RulesOut::new(builtin)).unwrap()
}

fn matrix(builtin: &Builtin) -> Value {
    serde_json::to_value(SupportMatrixOut::new(builtin)).unwrap()
}

fn integration<'v>(list: &'v Value, id: &str) -> &'v Value {
    let integrations = list["integrations"].as_array().unwrap();
    integrations.iter().find(|i| i["id"] == id).unwrap()
}

/// Catches the catalog hiding first-party operations again, or a pack author reading one as
/// open to them: only `first_party` says which they may name.
#[test]
fn rules_json_lists_every_operation_and_who_may_name_it() {
    let builtin = Builtin::load().unwrap();

    let catalog = rules(&builtin);

    let listed = catalog["capabilities"]["operations"].as_array().unwrap();
    assert_eq!(listed.len(), OPERATIONS.len());
    let named = |name: &str| listed.iter().find(|o| o["name"] == name).unwrap();
    assert_eq!(named("remove-files")["first_party"], json!(false));
    assert_eq!(named("remove-files")["methods"], json!(["delete", "trash"]));
    let release = named("remove-verified-release");
    assert_eq!(release["first_party"], json!(true));
    assert_eq!(release["retention"], json!(true));
    assert_eq!(named("git-worktree-remove")["methods"], json!([]));
}

/// Catches the two outputs wording an integration's state from two sources, so that one of
/// them could show a root open that the other shows closed.
#[test]
fn rules_and_the_support_matrix_show_the_same_state() {
    let builtin = Builtin::load().unwrap();
    let (catalog, support) = (rules(&builtin), matrix(&builtin));

    for descriptor in &builtin.descriptors {
        let id = descriptor.id.0;
        let (in_rules, in_matrix) = (integration(&catalog, id), integration(&support, id));
        for key in ["facts", "protected", "open"] {
            assert_eq!(in_rules[key], in_matrix[key], "{id}: {key}");
        }
    }
    let claude = integration(&catalog, "claude-code");
    assert_eq!(claude["protected"][0]["root"], json!("claude.config"));
}

/// Catches `mutate` saying "never" while `clean` removes Cargo's targets, or saying "yes" for
/// a tool kipple only reports.
#[test]
fn the_support_matrix_says_mutate_only_where_a_rule_removes() {
    let builtin = Builtin::load().unwrap();

    let support = matrix(&builtin);

    let on_linux = |id: &str| {
        let platforms = integration(&support, id)["platforms"].as_array().unwrap();
        let linux = platforms.iter().find(|p| p["platform"] == "linux").unwrap();
        linux["mutate"].clone()
    };
    assert_eq!(on_linux("cargo"), json!(true));
    assert_eq!(on_linux("pacman"), json!(false));
}

/// Each platform's `discover` and `report` for `id`.
fn claims(support: &Value, id: &str) -> Vec<(bool, bool)> {
    let platforms = integration(support, id)["platforms"].as_array().unwrap();
    let claim = |p: &Value| (p["discover"] == true, p["report"] == true);
    platforms.iter().map(claim).collect()
}

/// Catches the site saying kipple finds what it doesn't: a stub's pack gives it rules, and a
/// rule proves nothing about whether its integration can find anything yet.
#[test]
fn the_support_matrix_claims_nothing_for_an_integration_that_finds_nothing() {
    let builtin = Builtin::load().unwrap();

    let support = matrix(&builtin);

    for stub in ["codex", "cursor", "git", "npm", "pi"] {
        assert_eq!(claims(&support, stub), [(false, false); 3], "{stub}");
    }
    assert_eq!(claims(&support, "claude-code"), [(false, true); 3]);
    assert_eq!(claims(&support, "cargo"), [(true, true); 3]);
}
