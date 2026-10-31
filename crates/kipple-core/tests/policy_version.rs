//! The golden of what [`Policy::verdict`] decides (spec M3c pinned 14): a profile made
//! under one classification is suspended under another only when [`POLICY_VERSION`]
//! changes, so the version must change exactly when a verdict does.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kipple_core::{
    Activity, ActivityGap, Consequence, Cost, Digest, Facts, FileIdentity, GitGap, HeldPath, Level,
    Lineage, Loss, Method, Modified, POLICY_VERSION, Policy, ProbeError, Proposal, Recovery,
    RuleId, RuleSettings, Seen, Tracked, WalkGap,
};

/// The version and the digest of every verdict below. A change to either alone fails.
const GOLDEN: (u32, &str) = (1, "8465c62ae444a033bfb801d45aca1aed");

const DAY: u64 = 24 * 60 * 60;

#[cfg(unix)]
const BASE: &str = "/home/u";
#[cfg(windows)]
const BASE: &str = "C:/home/u";

fn at(rest: &str) -> PathBuf {
    Path::new(BASE).join(rest)
}

/// `lines` with every path in the unix form, so the golden is the same on every OS: the
/// cases' names and verdicts print the paths they carry, and Windows writes `BASE` with
/// a drive and joins with `\\`.
fn portable(lines: &str) -> String {
    lines.replace(BASE, "/home/u").replace(r"\\", "/")
}

const fn id(file: u128) -> FileIdentity {
    FileIdentity { volume: 7, file }
}

/// Every set of losses, each once.
fn loss_sets() -> Vec<&'static [Loss]> {
    let all = Loss::ALL;
    (0..1_u32 << all.len())
        .map(|mask| {
            let set: Vec<Loss> = all
                .iter()
                .enumerate()
                .filter(|(bit, _)| mask & (1 << bit) != 0)
                .map(|(_, loss)| *loss)
                .collect();
            &*set.leak()
        })
        .collect()
}

/// Facts Safe at every level: verified, idle, untracked, old local build output.
fn safe() -> Facts {
    Facts {
        proposal: Proposal::Remove(vec![Method::Delete, Method::Trash]),
        layout_verified: true,
        consequence: Some(Consequence {
            recovery: Recovery::Automatic,
            loss: &[],
            cost: Cost::Low,
        }),
        tracked: Tracked::NoRepository,
        activity: Activity::Idle,
        modified: Modified::Ago(Duration::from_secs(400 * DAY)),
        recent_days: None,
        lineage: Lineage(vec![Some(id(10)), Some(id(3)), Some(id(1))]),
    }
}

/// Each way a single fact can differ from [`safe`], named.
fn one_fact_each() -> Vec<(String, Facts)> {
    let mut cases = Vec::new();
    let mut with = |name: &str, change: &dyn Fn(&mut Facts)| {
        let mut facts = safe();
        change(&mut facts);
        cases.push((name.to_owned(), facts));
    };
    with("report", &|f| f.proposal = Proposal::Report);
    with("advice", &|f| f.proposal = Proposal::Advice);
    with("delete-only", &|f| {
        f.proposal = Proposal::Remove(vec![Method::Delete]);
    });
    with("unverified", &|f| f.layout_verified = false);
    with("no-consequence", &|f| f.consequence = None);
    let tree = at("p");
    with("asked-none", &|f| {
        f.tracked = Tracked::Asked {
            work_tree: tree.clone(),
            tracked: 0,
        };
    });
    with("asked-some", &|f| {
        f.tracked = Tracked::Asked {
            work_tree: tree.clone(),
            tracked: 3,
        };
    });
    for gap in GitGap::ALL {
        with(&format!("inside-{gap:?}"), &|f| {
            f.tracked = Tracked::InsideRepository {
                work_tree: tree.clone(),
                gap: *gap,
            };
        });
    }
    with("unchecked", &|f| {
        f.tracked = Tracked::Unchecked(ProbeError::OutsideRoots { path: tree.clone() });
    });
    with("holds-repo", &|f| {
        f.tracked = Tracked::HoldsRepository {
            work_tree: tree.clone(),
        };
    });
    let gaps = [WalkGap::Unread, WalkGap::OtherFileSystem, WalkGap::Untimed];
    for gap in gaps {
        with(&format!("unwalked-{gap:?}"), &|f| {
            f.tracked = Tracked::Unwalked(gap);
        });
        with(&format!("modified-{gap:?}"), &|f| {
            f.modified = Modified::Unknown(gap);
        });
    }
    cases
}

/// Each way activity or age can differ from [`safe`], named.
fn in_use_or_aged_each() -> Vec<(String, Facts)> {
    let mut cases = Vec::new();
    let mut with = |name: &str, change: &dyn Fn(&mut Facts)| {
        let mut facts = safe();
        change(&mut facts);
        cases.push((name.to_owned(), facts));
    };
    let tree = at("p");
    with("idle-except-system", &|f| {
        f.activity = Activity::IdleExceptSystem {
            unchecked: 2,
            by_command_line: 1,
        };
    });
    with("in-use", &|f| {
        f.activity = Activity::InUse {
            pid: 9,
            seen: Seen::WorkingDirectory(tree.clone()),
        };
    });
    let unknown = [
        ActivityGap::NoListing(ProbeError::OutsideRoots { path: tree.clone() }),
        ActivityGap::Partial("listing".to_owned()),
        ActivityGap::Uninspected(1),
        ActivityGap::Unsupported,
    ];
    for gap in unknown {
        with(&format!("activity-{gap:?}"), &|f| {
            f.activity = Activity::Unknown(gap.clone());
        });
    }
    for level in Level::ALL {
        let window = u64::from(level.recent_days());
        for days in [window - 1, window, window + 1] {
            with(&format!("age-{days}d"), &|f| {
                f.modified = Modified::Ago(Duration::from_secs(days * DAY));
            });
        }
    }
    with("own-window-30", &|f| f.recent_days = Some(30));
    with("own-window-30-old", &|f| {
        f.recent_days = Some(30);
        f.modified = Modified::Ago(Duration::from_secs(20 * DAY));
    });
    cases
}

/// Each consequence: every recovery, set of losses and cost.
fn consequences() -> Vec<(String, Facts)> {
    let mut cases = Vec::new();
    for recovery in Recovery::ALL {
        for loss in loss_sets() {
            for cost in Cost::ALL {
                let mut facts = safe();
                facts.consequence = Some(Consequence {
                    recovery: *recovery,
                    loss,
                    cost: *cost,
                });
                cases.push((format!("{recovery:?} {loss:?} {cost:?}"), facts));
            }
        }
    }
    cases
}

fn held(path: &str, note: Option<&str>) -> HeldPath {
    HeldPath {
        forms: vec![at(path)],
        lineage: Lineage(vec![Some(id(20)), Some(id(3)), Some(id(1))]),
        note: note.map(str::to_owned),
    }
}

/// The policies the facts are judged under: each level, and each config hold.
fn policies() -> Vec<(String, Policy)> {
    let rule = RuleId("builtin.cargo.target".to_owned());
    let mut policies: Vec<(String, Policy)> = Level::ALL
        .iter()
        .map(|level| (format!("{level:?}"), Policy::at(*level)))
        .collect();
    let mut disabled = Policy::default();
    disabled.rules.insert(
        rule.clone(),
        RuleSettings {
            disabled: true,
            ..RuleSettings::default()
        },
    );
    policies.push(("disabled".to_owned(), disabled));
    let mut window = Policy::default();
    window.rules.insert(
        rule,
        RuleSettings {
            recent_days: Some(30),
            ..RuleSettings::default()
        },
    );
    policies.push(("rule-window-30".to_owned(), window));
    let kept = Policy {
        keep: vec![held("p", Some("mine"))],
        ..Policy::default()
    };
    policies.push(("kept".to_owned(), kept));
    let excluded = Policy {
        exclude: vec![held("p", None)],
        ..Policy::default()
    };
    policies.push(("excluded".to_owned(), excluded));
    policies
}

/// Catches a change to how findings are classified made without bumping
/// [`POLICY_VERSION`], which would let a profile made under the old classification run
/// under the new one.
#[test]
fn the_classification_matches_its_version() {
    let rule = RuleId("builtin.cargo.target".to_owned());
    let path = at("p/target");
    let mut lines = String::new();
    let mut cases = one_fact_each();
    cases.extend(in_use_or_aged_each());
    cases.extend(consequences());
    for (policy_name, policy) in policies() {
        for (facts_name, facts) in &cases {
            let verdict = policy.verdict(&rule, &path, facts);
            writeln!(lines, "{policy_name} | {facts_name} => {verdict:?}").unwrap();
        }
    }
    // Which paths a kept or excluded entry holds: at, inside, above, beside, and by
    // identity under another spelling.
    let entry = held("p", None);
    for (rest, lineage) in [
        ("p", vec![Some(id(20))]),
        ("p/target", vec![Some(id(21)), Some(id(20))]),
        ("", vec![Some(id(3))]),
        ("q", vec![Some(id(30)), Some(id(3))]),
        ("P/target", vec![Some(id(21)), Some(id(20))]),
        ("other/target", vec![Some(id(40)), Some(id(20))]),
        ("unknown/target", vec![None, Some(id(3))]),
    ] {
        let holds = entry.holds(&at(rest), &Lineage(lineage.clone()));
        // The path as written here, so the golden is the same on every OS.
        writeln!(lines, "holds {rest:?} {lineage:?} => {holds}").unwrap();
    }
    let digest = Digest::of_bytes(portable(&lines).as_bytes()).to_string();
    assert_eq!(
        (POLICY_VERSION, digest.as_str()),
        GOLDEN,
        "classification changed: bump POLICY_VERSION, then update GOLDEN to the new pair"
    );
}
