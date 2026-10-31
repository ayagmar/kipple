//! The golden of every built-in rule's digests (spec M3c pinned 14): a profile entry is
//! suspended when its rule's or its integration's digest changes, so a change here
//! suspends users' profiles and must be made on purpose.

use std::fmt::Write as _;

use kipple_adapters::builtin;
use kipple_core::format::{IntegrationDigests, rule_digest};

/// Each built-in rule, sorted: its ID, `rule_digest` and its integration's digest.
const GOLDEN: &str = concat!(
    "builtin.cargo.target 5be092e8eff9dac8e6c1712153dbac3f98704b949c54e35ff1d8b368bb5a687e f35126608c3f8623dfc13c458f0f39818c853f0f644016c80145da9536545daf\n",
    "builtin.journald.journal 7ee77803e2b10d9fa442f8cee9b57a7b8749accb6cb8243c892cfbeca4ac659b f1df5652ca060cdca9a8bb85351091634af40365870f2a6149a6f2aecd943d16\n",
    "builtin.node.node-modules aca7248e1b74dfed1bb93b803f25a450b350bb1c5ba0da6a6f8b86aa92014e3f 72ad6d904bd9dec43e1298f1036a82b10e18b908f9d6d9c66acfcfdf07eeb09e\n",
    "builtin.pacman.cache 1fd8fd869472857371bb461315c5f5dd63ec7d07c5421d0bf51050fb39822d8e db8bf2bf21df0f4ff57ec6ee3abe6055c02cc403bfa35a909a6ce7d7bce86e03\n",
    "builtin.python.bytecode ce0f23b5decd148cb6773a27e95f480492fd00fc88e579979501d0676199c168 3771f5783228cd965aa4cbc4700a5a4965f233bbf77338647fc1c5645c790b4f\n",
    "builtin.python.venv 89a5748a695bbd12efc920a8b782f7322726d622031b80474717c53a84a3f78c 3771f5783228cd965aa4cbc4700a5a4965f233bbf77338647fc1c5645c790b4f\n",
    "builtin.trash.contents 72259c1bf104edda7f3d1ed84df4e262b2e692b6e65a9df04b0325a39ab1fe7f 7f05355aa41505d0c08d705d204ef20d951c3ccee5bd272e88eca4f14e94800f\n",
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
