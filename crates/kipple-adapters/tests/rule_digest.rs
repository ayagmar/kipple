//! The golden of every built-in rule's digests (spec M3c pinned 14): a profile entry is
//! suspended when its rule's or its integration's digest changes, so a change here
//! suspends users' profiles and must be made on purpose. `tests/golden/rule_digest` holds
//! a file per integration with rules, a row per rule, sorted: its ID, `rule_digest` and
//! its integration's digest.

#[path = "golden/rows.rs"]
mod golden;

use std::collections::BTreeMap;
use std::fs;
use std::io;

use kipple_adapters::builtin;
use kipple_core::format::{IntegrationDigests, rule_digest};

/// Catches an edit to a built-in rule or integration descriptor that changes what a
/// profile froze without anyone deciding to suspend profiles, a rule with no row, and a
/// row in the wrong file or with no rule.
#[test]
fn every_builtin_rule_keeps_its_digests() {
    let integrations = IntegrationDigests::of(&builtin::integrations());
    let packs = builtin::packs(&semver::Version::new(0, 0, 0)).unwrap();
    let mut rules: Vec<_> = packs.into_iter().flat_map(|pack| pack.rules).collect();
    rules.sort_by(|a, b| a.id.cmp(&b.id));
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rule in &rules {
        let integration = integrations
            .get(rule.integration)
            .map_or_else(|| "none".to_owned(), |digest| digest.to_string());
        found
            .entry(rule.integration.0.to_owned())
            .or_default()
            .push(format!("{} {} {integration}", rule.id.0, rule_digest(rule)));
    }
    assert_eq!(
        found,
        golden::rows("rule_digest").unwrap(),
        "this edit suspends users' profiles: update the golden on purpose"
    );
}

/// Catches a stray `cargo` beside `cargo.txt` taking its key, so that the order the
/// directory lists them in decides which one is compared and the other goes unchecked.
#[test]
fn a_golden_file_not_named_txt_fails_the_read() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("cargo.txt"), "row\n").unwrap();
    fs::write(dir.path().join("cargo"), "row\n").unwrap();

    let error = golden::rows_in(dir.path()).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}
