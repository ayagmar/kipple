//! The CLI and the home screen word one report alike (`docs/05-architecture.md` §4): over
//! one report, `kipple` and `kipple scan` without a terminal and the home screen's tree
//! show the same bucket names, totals, sizes and reasons for the same findings.

use std::path::PathBuf;
use std::time::Duration;

use kipple_core::{
    Activity, Category, Completeness, Evidence, Facts, Finding, FindingId, IntegrationId,
    IntegrationReport, Policy, RuleId, ScanEvent, ScanOutcome, ScanReport, Seen, SpaceEstimate,
};

use super::{
    CARGO, MIB, NODE, PACMAN, PYTHON, Paint, advice, catalog, look, recent, safe, screen, size,
    unknown_layout,
};
use crate::config::DetailPlace;
use crate::json::ScanOut;
use crate::scan::{write_human, write_summary};
use crate::theme::{Glyphs, Ink};
use crate::tui::app::{App, Msg};

const WIDTH: u16 = 200;
const HEIGHT: u16 = 60;

/// A finding of the report, by the last two components of its path, which is how the
/// tree labels a finding outside every project root.
struct Found {
    integration: IntegrationId,
    rule: &'static str,
    tail: &'static str,
    size: SpaceEstimate,
    facts: Facts,
}

/// A finding in each bucket, one held for more than one reason and one measured only
/// in part, so its size is a lower bound.
fn found() -> Vec<Found> {
    let mut partial = size(64 * MIB, 64 * MIB);
    partial.completeness = Completeness::Incomplete {
        unreadable: 2,
        skipped_mounts: 0,
    };
    let held_twice = Facts {
        activity: Activity::InUse {
            pid: 4242,
            seen: Seen::WorkingDirectory(PathBuf::from("/home/ada/projects/bun/node_modules")),
        },
        ..unknown_layout()
    };
    vec![
        Found {
            integration: CARGO,
            rule: "builtin.cargo.target",
            tail: "api/target",
            size: size(512 * MIB, 512 * MIB),
            facts: safe(),
        },
        Found {
            integration: CARGO,
            rule: "builtin.cargo.target",
            tail: "cli/target",
            size: partial,
            facts: safe(),
        },
        Found {
            integration: NODE,
            rule: "builtin.node.node-modules",
            tail: "web/node_modules",
            size: size(300 * MIB, 300 * MIB),
            facts: recent(),
        },
        Found {
            integration: NODE,
            rule: "builtin.node.node-modules",
            tail: "bun/node_modules",
            size: size(120 * MIB, 120 * MIB),
            facts: held_twice,
        },
        Found {
            integration: PYTHON,
            rule: "builtin.python.bytecode",
            tail: "tool/__pycache__",
            size: size(3 * MIB / 2, 3 * MIB / 2),
            facts: unknown_layout(),
        },
        Found {
            integration: PACMAN,
            rule: "builtin.pacman.cache",
            tail: "pacman/pkg",
            size: size(900 * MIB, 900 * MIB),
            facts: advice(),
        },
    ]
}

/// The report of a finished scan that found `found`.
fn report(found: &[Found]) -> ScanReport {
    let findings = (1..)
        .zip(found)
        .map(|(id, found)| Finding {
            id: FindingId(id),
            integration: found.integration,
            rule: RuleId(found.rule.to_owned()),
            category: Category::BuildArtifact,
            path: PathBuf::from(format!("/home/ada/projects/{}", found.tail)),
            evidence: vec![Evidence::Marker("Cargo.toml".to_owned())],
            size: Some(found.size),
            facts: Some(found.facts.clone()),
        })
        .collect();
    ScanReport {
        outcome: ScanOutcome::Completed,
        integrations: vec![IntegrationReport {
            id: PACMAN,
            omissions: Vec::new(),
            opened: Vec::new(),
            advice: Vec::new(),
        }],
        findings,
        unchecked_system: 0,
        by_command_line: 0,
    }
}

/// `report` as `kipple` and `kipple scan` write it without a terminal.
fn cli(report: &ScanReport) -> (String, String) {
    let out = ScanOut::new(report, &catalog().lenses, &Policy::default());
    let (mut summary, mut list) = (Vec::new(), Vec::new());
    write_summary(&mut summary, &out, &Ink::plain()).unwrap();
    write_human(&mut list, &out, &Ink::plain()).unwrap();
    (
        String::from_utf8(summary).unwrap(),
        String::from_utf8(list).unwrap(),
    )
}

/// The home screen once the scan streamed `report` and finished, as text, its tree as
/// wide as the screen.
fn home(report: &ScanReport) -> String {
    let mut app = App::new(catalog(), WIDTH, HEIGHT);
    app.config.ui.detail = DetailPlace::Off;
    for finding in &report.findings {
        let added = Finding {
            size: None,
            facts: None,
            ..finding.clone()
        };
        app.update(Msg::Scan(Box::new(ScanEvent::FindingAdded {
            finding: added,
        })));
        app.update(Msg::Scan(Box::new(ScanEvent::FindingUpdated {
            id: finding.id,
            size: finding.size,
            facts: finding.facts.clone().unwrap(),
        })));
    }
    app.update(Msg::Finished {
        report: Box::new(report.clone()),
        elapsed: Duration::from_millis(900),
    });
    screen(&app, look(Paint::Mono, Glyphs::Ascii), WIDTH, HEIGHT)
}

fn line_with<'a>(text: &'a str, wanted: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(wanted))
        .unwrap_or_else(|| panic!("no line with {wanted:?} in\n{text}"))
}

// Catches the CLI and the home screen wording a bucket apart: its name, how many
// findings it holds or their total, as `kipple` prints it and the tree heads it.
#[test]
fn each_bucket_has_the_same_name_count_and_total_in_the_cli_summary_and_the_tree() {
    let report = report(&found());
    let (summary, _) = cli(&report);
    let home = home(&report);

    for title in ["Safe to clean", "Review", "Protected"] {
        let row = line_with(&summary, &format!("  {title} "));
        let [total, count]: [&str; 2] = row
            .trim_start()
            .strip_prefix(title)
            .unwrap()
            .trim()
            .split("  ")
            .map(str::trim)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap_or_else(|_| panic!("not a total and a count: {row:?}"));
        let count = count.split(' ').next().unwrap();
        let heading = line_with(&home, &format!("{title} ({count})"));
        assert!(heading.contains(total), "{total} in {heading:?}\n{home}");
    }
}

// Catches a finding's size or reasons said one way by `kipple scan` and another on the
// home screen: a lower bound shown as exact, another unit, or another hold's words.
#[test]
fn each_finding_has_the_same_size_and_reasons_in_the_cli_list_and_the_tree() {
    let found = found();
    let report = report(&found);
    let (_, list) = cli(&report);
    let home = home(&report);

    let mut held_twice = false;
    for finding in &found {
        let listed = line_with(&list, finding.tail);
        let size = listed.trim_start().split("  ").next().unwrap();
        let reasons: Vec<&str> = listed
            .rsplit_once("  (")
            .map(|(_, reasons)| reasons.trim_end_matches(')').split(", ").collect())
            .unwrap_or_default();
        let row = line_with(&home, finding.tail);
        assert!(row.contains(size), "{size} in {row:?}\n{home}");
        let state = match reasons.as_slice() {
            [] => None,
            [one] => Some((*one).to_owned()),
            [first, rest @ ..] => {
                held_twice = true;
                Some(format!("{first} +{}", rest.len()))
            }
        };
        // The state column is the row's last, before the frame's edge.
        if let Some(state) = state {
            assert!(
                row.trim_end_matches(['|', ' ']).ends_with(&state),
                "{state} ending {row:?}\n{home}"
            );
        }
    }
    assert!(held_twice, "no finding has two reasons\n{list}");
    assert!(list.contains(">= 64.0 MiB"), "{list}");
}
