//! The golden of every built-in rule's digests (spec M3c pinned 14): a profile entry is
//! suspended when its rule's or its integration's digest changes, so a change here
//! suspends users' profiles and must be made on purpose.

use std::fmt::Write as _;

use kipple_adapters::builtin;
use kipple_core::format::{IntegrationDigests, rule_digest};

/// Each built-in rule, sorted: its ID, `rule_digest` and its integration's digest.
const GOLDEN: &str = concat!(
    "builtin.cargo.target 77568c28442c45e9cbcd1e0c8317183e 681de0f46ac6a7ef0496971574f45e22\n",
    "builtin.journald.journal 7ddd52b26f00c6e0341e088a0db55113 c08db1b47ecc611547f7018449ba1f6c\n",
    "builtin.node.node-modules e95174707a0cac438dfbf529cd2b2a8e afc3bd47088197da0261e7a4a008f1ae\n",
    "builtin.pacman.cache 01c3421f5e06fa7c585166476836659a 82c88127fc81ff2561673cd54ee7c38e\n",
    "builtin.python.bytecode 65abdd8e3a541321719067576da9f507 fe9f7715d34d564c0490757abcc60600\n",
    "builtin.python.venv 033fc31f3e5b739c95f9cab7ca19906e fe9f7715d34d564c0490757abcc60600\n",
    "builtin.trash.contents 61d5c672438818fdad2497e53f1534ec 028a371bc5dbc30c8bd156f53857e128\n",
);

/// Catches an edit to a built-in rule or integration descriptor that changes what a
/// profile froze without anyone deciding to suspend profiles.
#[test]
fn every_builtin_rule_keeps_its_digests() {
    let integrations = IntegrationDigests::of(&builtin::integrations());
    let packs = builtin::packs(&semver::Version::new(0, 0, 0)).unwrap();
    let mut rules: Vec<_> = packs.into_iter().flat_map(|pack| pack.rules).collect();
    rules.sort_by(|a, b| a.id.cmp(&b.id));
    let mut lines = String::new();
    for rule in &rules {
        let integration = integrations
            .get(rule.integration)
            .map_or_else(|| "none".to_owned(), |digest| digest.to_string());
        writeln!(lines, "{} {} {integration}", rule.id.0, rule_digest(rule)).unwrap();
    }
    assert_eq!(
        lines, GOLDEN,
        "this edit suspends users' profiles: update the golden on purpose"
    );
}
