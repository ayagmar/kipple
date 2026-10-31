//! The golden of what each built-in integration finds and says about it over its layout
//! fixtures, negative ones included (spec M3c pinned 14): a profile freezes each
//! integration's `revision`, so the revision must change whenever this output does. The
//! golden is a history, so changed output can't pass by its digest being pasted over the
//! old one.
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]

#[path = "fixtures/demo_home.rs"]
mod demo_home;
#[path = "fixtures/scan.rs"]
mod harness;
#[path = "fixtures/tree.rs"]
mod tree;
#[path = "fixtures/python/venv_v3_14_8.rs"]
mod venv_v3_14_8;
// The demo home's own fixture modules.
#[path = "fixtures/node/bun_v1_4_2.rs"]
mod bun_v1_4_2;
#[path = "fixtures/cargo/v1_99_0.rs"]
mod cargo_v1_99_0;
#[path = "fixtures/claude-code/settings.rs"]
mod claude_settings;
#[path = "fixtures/journald/v262.rs"]
mod journald_v262;
#[path = "fixtures/node/npm_v11_19_0.rs"]
mod npm_v11_19_0;
#[path = "fixtures/pacman/v7_1_0.rs"]
mod pacman_v7_1_0;
#[path = "fixtures/node/pnpm_v12_9_1.rs"]
mod pnpm_v12_9_1;
#[path = "fixtures/trash/freedesktop_v1_0.rs"]
mod trash_v1_0;
#[path = "fixtures/python/uv_v0_12_23.rs"]
mod uv_v0_12_23;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use kipple_adapters::builtin;
use kipple_core::{Digest, ScanReport};
use tree::FixtureTree;

/// Every revision each built-in integration has had, by ID, oldest first: its ID, the
/// revision and the digest of what it found over the fixtures. A change to what one finds
/// appends a row with its next revision; a row is never edited, so pasting a new digest
/// over the current one fails.
/// Linux runs every integration; the other systems leave some out, so the digests are
/// Linux's, which CI runs.
const HISTORY: &[(&str, u32, &str)] = &[
    ("cargo", 1, "0e697897cb5b69e21c7e9c0e34286df0"),
    ("claude-code", 1, "6c62272e07bb014262b821756295c58d"),
    ("journald", 1, "e49780136435bcd315cc754678ba943d"),
    ("node", 1, "8129e5b8c2750c6079f0f1086839e6d5"),
    ("pacman", 1, "3cacf4638284236d5a3f07f4f76e18c2"),
    ("python", 1, "26706de1cc2c65b5a5c04770e8cb3ab2"),
    ("trash", 1, "0ec6cbf58676b78efbb8d575ed1f1bca"),
];

/// Layouts kipple must not take for supported ones: a cargo target without its tag or
/// with a forged one, and bytecode beside other files or inside an environment.
fn negative() -> FixtureTree {
    let mut tree = FixtureTree::new();
    tree.file("home/projects/a/Cargo.toml", b"")
        .dir("home/projects/a/target/debug")
        .file("home/projects/b/Cargo.toml", b"")
        .file(
            "home/projects/b/target/CACHEDIR.TAG",
            b"Signature: not the one",
        )
        .file("home/projects/p/pkg/__pycache__/mod.cpython-314.pyc", b"")
        .file("home/projects/p/pkg/__pycache__/notes.txt", b"mine")
        .file(
            "home/projects/loose/.venv/pkg/__pycache__/x.cpython-314.pyc",
            b"",
        );
    venv_v3_14_8::environment(&mut tree, "home/projects/env");
    tree
}

/// What each integration found in `report`, a line per finding: its rule, path below the
/// fixture, category, evidence, proposal, layout, consequence and the holds its facts
/// give at the default policy. No identity or time, which change from run to run.
fn lines(base: &std::path::Path, report: &ScanReport, into: &mut BTreeMap<String, String>) {
    let policy = kipple_core::Policy::default();
    for finding in &report.findings {
        let below = finding.path.strip_prefix(base).unwrap();
        let parts: Vec<_> = below.iter().map(|part| part.to_string_lossy()).collect();
        let relative = parts.join("/");
        let facts = finding.facts.as_ref().map(|facts| {
            let verdict = policy.verdict(&finding.rule, &finding.path, facts);
            format!(
                "{:?} {} {:?} {:?}",
                facts.proposal, facts.layout_verified, facts.consequence, verdict
            )
        });
        let line = into.entry(finding.integration.0.to_owned()).or_default();
        writeln!(
            line,
            "{} {} {:?} {:?} {facts:?}",
            finding.rule.0, relative, finding.category, finding.evidence
        )
        .unwrap();
    }
}

/// Catches an integration whose findings or facts changed while its revision didn't,
/// which would keep profiles made under the old behaviour active.
#[test]
fn every_integration_changes_only_with_its_revision() {
    if std::env::consts::OS != "linux" {
        return;
    }
    let mut found = BTreeMap::new();
    for tree in [demo_home::tree(demo_home::looked_at()), negative()] {
        let harness::Scanned { temp, report } = harness::scan(&tree);
        lines(temp.path(), &report, &mut found);
    }
    let wrong: Vec<String> = builtin::descriptors()
        .into_iter()
        .filter_map(|descriptor| {
            let id = descriptor.id.0;
            let text = found.get(id).map_or("", String::as_str);
            let digest = Digest::of_bytes(text.as_bytes()).to_string();
            out_of_history(id, descriptor.revision, &digest)
        })
        .collect();
    assert_eq!(wrong, [] as [String; 0]);
}

/// Why `revision` and `digest`, the integration `id` now, aren't its newest row in
/// [`HISTORY`]; `None` when they are.
fn out_of_history(id: &str, revision: u32, digest: &str) -> Option<String> {
    let rows: Vec<(u32, &str)> = HISTORY
        .iter()
        .filter(|(listed, ..)| *listed == id)
        .map(|(_, listed, its)| (*listed, *its))
        .collect();
    let revisions: Vec<u32> = rows.iter().map(|(listed, _)| *listed).collect();
    if !revisions.is_sorted_by(|a, b| a < b) {
        return Some(format!(
            "{id}: its revisions aren't increasing: {revisions:?}"
        ));
    }
    if let Some((_, listed)) = rows
        .iter()
        .find(|(listed, other)| *listed == revision && *other != digest)
    {
        let next = revisions.last().map_or(1, |newest| newest + 1);
        return Some(format!(
            "{id}: its output changed, but revision {revision} is listed with {listed}: bump \
             its revision to {next} and append (\"{id}\", {next}, \"{digest}\") to HISTORY; \
             never edit a row"
        ));
    }
    let newest = rows.last().copied();
    (newest != Some((revision, digest))).then(|| {
        format!(
            "{id}: its newest row is {newest:?}, but it is now ({revision}, \"{digest}\"): a \
             change bumps its revision and appends that row; never edit one"
        )
    })
}
