//! The bucket table of `docs/04-safety-model.md` §2a: the first matching row wins,
//! unknown facts hold a finding in Protected and never make it Review, and the level moves
//! only the line between Safe and Review.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kipple_core::{
    Activity, ActivityGap, Bucket, Caution, Consequence, Cost, Facts, FileIdentity, HeldPath, Hold,
    Level, Lineage, Loss, Method, Modified, Policy, ProbeError, Proposal, Recovery, RuleId,
    RuleSettings, Tracked, Verdict, WalkGap,
};

const DAY: Duration = Duration::from_hours(24);

/// A Cargo target directory as kipple sees one it may propose: recognised, rebuilt
/// locally, outside any repository, used by nothing, untouched for 41 days.
fn build_output() -> Facts {
    Facts {
        proposal: Proposal::Remove(vec![Method::Delete, Method::Trash]),
        layout_verified: true,
        consequence: Some(Consequence {
            recovery: Recovery::RebuildLocal,
            loss: &[Loss::CacheWarmth],
            cost: Cost::Unknown,
        }),
        tracked: Tracked::NoRepository,
        activity: Activity::Idle,
        modified: Modified::Ago(41 * DAY),
        recent_days: None,
        lineage: Lineage::default(),
    }
}

/// The bucket of a Cargo target directory with `facts` at `level`, nothing else configured.
fn classify(facts: &Facts, level: Level) -> Verdict {
    let rule = RuleId("builtin.cargo.target".to_owned());
    Policy::at(level).verdict(&rule, Path::new("/home/u/projects/app/target"), facts)
}

fn balanced(facts: &Facts) -> Verdict {
    classify(facts, Level::Balanced)
}

fn denied(path: &str) -> ProbeError {
    ProbeError::io(&PathBuf::from(path), io::ErrorKind::PermissionDenied.into())
}

#[test]
fn verified_idle_untracked_old_build_output_is_safe_even_with_an_unknown_restore_cost() {
    assert_eq!(balanced(&build_output()), Verdict::Safe);
}

#[test]
fn balanced_puts_reinstalled_and_redownloaded_things_in_safe_but_not_a_lost_rollback() {
    let with = |recovery, loss: &'static [Loss]| Facts {
        consequence: Some(Consequence {
            recovery,
            loss,
            cost: Cost::Unknown,
        }),
        ..build_output()
    };

    let node_modules = with(Recovery::Reinstall, &[Loss::OfflineAbility]);
    let download = with(Recovery::Redownload, &[Loss::OfflineAbility]);
    let bytecode = with(Recovery::Automatic, &[Loss::CacheWarmth]);
    let old_release = with(Recovery::Redownload, &[Loss::RollbackAbility]);

    assert_eq!(balanced(&node_modules), Verdict::Safe);
    assert_eq!(balanced(&download), Verdict::Safe);
    assert_eq!(balanced(&bytecode), Verdict::Safe);
    assert_eq!(balanced(&old_release), Verdict::Review(vec![Caution::Loss]));
}

#[test]
fn what_never_comes_back_loses_history_or_costs_a_lot_is_review_at_any_level() {
    let gone = Facts {
        consequence: Some(Consequence {
            recovery: Recovery::None,
            loss: &[Loss::History],
            cost: Cost::High,
        }),
        ..build_output()
    };

    assert_eq!(
        balanced(&gone),
        Verdict::Review(vec![Caution::Recovery, Caution::Loss, Caution::HighCost])
    );
}

#[test]
fn something_changed_within_the_recent_window_is_review_and_a_rule_can_only_lengthen_it() {
    let aged = |days, recent_days| Facts {
        modified: Modified::Ago(days * DAY),
        recent_days,
        ..build_output()
    };
    let recent = Verdict::Review(vec![Caution::Recent]);

    // Balanced's window is 7 days.
    assert_eq!(balanced(&aged(6, None)), recent);
    assert_eq!(balanced(&aged(7, None)), Verdict::Safe);
    // A rule asking for 2 days doesn't shorten it; one asking for 30 lengthens it.
    assert_eq!(balanced(&aged(6, Some(2))), recent);
    assert_eq!(balanced(&aged(20, Some(30))), recent);
    assert_eq!(balanced(&aged(30, Some(30))), Verdict::Safe);
}

#[test]
fn a_report_only_rule_is_protected_however_safe_its_facts_look() {
    let report = Facts {
        proposal: Proposal::Report,
        ..build_output()
    };
    let advice = Facts {
        proposal: Proposal::Advice,
        ..build_output()
    };

    assert_eq!(
        balanced(&report),
        Verdict::Protected(vec![Hold::ReportOnly])
    );
    assert_eq!(balanced(&advice), Verdict::Protected(vec![Hold::Advice]));
}

#[test]
fn each_unknown_or_blocking_fact_holds_the_finding_in_protected_and_never_in_review() {
    let cases = [
        (
            Facts {
                layout_verified: false,
                ..build_output()
            },
            Hold::UnknownLayout,
        ),
        (
            Facts {
                consequence: None,
                ..build_output()
            },
            Hold::UnknownConsequence,
        ),
        (
            Facts {
                activity: Activity::InUse {
                    pid: 4121,
                    executable: PathBuf::from("/p/target/debug/app"),
                },
                ..build_output()
            },
            Hold::InUse,
        ),
        (
            Facts {
                activity: Activity::Unknown(ActivityGap::Uninspected(3)),
                ..build_output()
            },
            Hold::UnknownActivity,
        ),
        (
            Facts {
                activity: Activity::Unknown(ActivityGap::Partial("hidepid=2".to_owned())),
                ..build_output()
            },
            Hold::UnknownActivity,
        ),
        (
            Facts {
                modified: Modified::Unknown(WalkGap::Unread),
                ..build_output()
            },
            Hold::UnknownModified,
        ),
        (
            Facts {
                modified: Modified::Unknown(WalkGap::OtherFileSystem),
                ..build_output()
            },
            Hold::UnknownModified,
        ),
    ];

    for (facts, hold) in cases {
        assert_eq!(
            balanced(&facts),
            Verdict::Protected(vec![hold]),
            "{facts:?}"
        );
    }
}

#[test]
fn a_protected_finding_shows_every_reason_it_is_held_not_only_the_first() {
    let facts = Facts {
        layout_verified: false,
        activity: Activity::Unknown(ActivityGap::Uninspected(1)),
        // Recent too, but holding it is what decides the bucket.
        modified: Modified::Ago(DAY),
        ..build_output()
    };

    assert_eq!(
        balanced(&facts),
        Verdict::Protected(vec![Hold::UnknownLayout, Hold::UnknownActivity])
    );
}

#[test]
fn a_repository_around_or_inside_a_finding_or_one_not_ruled_out_holds_it_in_protected() {
    let cases = [
        (
            Tracked::InsideRepository {
                work_tree: PathBuf::from("/p"),
            },
            Hold::UnknownGit,
        ),
        (Tracked::Unchecked(denied("/home")), Hold::UnknownGit),
        (Tracked::Unwalked(WalkGap::Unread), Hold::UnknownGit),
        (
            Tracked::Unwalked(WalkGap::OtherFileSystem),
            Hold::UnknownGit,
        ),
        (
            Tracked::HoldsRepository {
                work_tree: PathBuf::from("/p/.venv/src/pkg"),
            },
            Hold::HoldsRepository,
        ),
    ];

    for (tracked, hold) in cases {
        let facts = Facts {
            tracked,
            ..build_output()
        };
        assert_eq!(
            balanced(&facts),
            Verdict::Protected(vec![hold]),
            "{facts:?}"
        );
    }
}

/// What each level lets a Safe finding have, written out from `docs/04-safety-model.md`
/// §2a's table rather than read back from the code.
const fn table(level: Level) -> (&'static [Recovery], &'static [Loss], u32) {
    match level {
        Level::Careful => (
            &[Recovery::Automatic, Recovery::RebuildLocal],
            &[Loss::CacheWarmth],
            14,
        ),
        Level::Balanced => (
            &[
                Recovery::Automatic,
                Recovery::RebuildLocal,
                Recovery::Reinstall,
                Recovery::Redownload,
            ],
            &[Loss::CacheWarmth, Loss::OfflineAbility],
            7,
        ),
        Level::Thorough => (
            &[
                Recovery::Automatic,
                Recovery::RebuildLocal,
                Recovery::Reinstall,
                Recovery::Redownload,
            ],
            &[
                Loss::CacheWarmth,
                Loss::OfflineAbility,
                Loss::RollbackAbility,
            ],
            2,
        ),
    }
}

/// The bucket the table gives a finding that recovers by `recovery` and loses `loss`.
fn expected(level: Level, recovery: Recovery, loss: Loss) -> Verdict {
    let (recoveries, losses, _) = table(level);
    let mut cautions = Vec::new();
    if !recoveries.contains(&recovery) {
        cautions.push(Caution::Recovery);
    }
    if !losses.contains(&loss) {
        cautions.push(Caution::Loss);
    }
    if cautions.is_empty() {
        Verdict::Safe
    } else {
        Verdict::Review(cautions)
    }
}

#[test]
fn each_level_puts_each_recovery_and_loss_where_its_table_says() {
    let cases = Level::ALL.iter().flat_map(|&level| {
        Recovery::ALL
            .iter()
            .flat_map(move |&recovery| Loss::ALL.iter().map(move |loss| (level, recovery, loss)))
    });

    for (level, recovery, loss) in cases {
        let facts = Facts {
            consequence: Some(Consequence {
                recovery,
                loss: std::slice::from_ref(loss),
                cost: Cost::Low,
            }),
            ..build_output()
        };
        assert_eq!(
            classify(&facts, level),
            expected(level, recovery, *loss),
            "{level}: {recovery} losing {loss}"
        );
    }
}

#[test]
fn a_level_never_makes_history_none_high_cost_or_recent_safe() {
    let with = |recovery, loss: &'static [Loss], cost| Facts {
        consequence: Some(Consequence {
            recovery,
            loss,
            cost,
        }),
        ..build_output()
    };
    let history = with(Recovery::Automatic, &[Loss::History], Cost::Low);
    let none = with(Recovery::None, &[], Cost::Low);
    let costly = with(Recovery::Automatic, &[], Cost::High);
    let recent = Facts {
        modified: Modified::Ago(Duration::ZERO),
        ..build_output()
    };

    for &level in Level::ALL {
        assert_eq!(
            classify(&history, level),
            Verdict::Review(vec![Caution::Loss])
        );
        assert_eq!(
            classify(&none, level),
            Verdict::Review(vec![Caution::Recovery])
        );
        assert_eq!(
            classify(&costly, level),
            Verdict::Review(vec![Caution::HighCost])
        );
        assert_eq!(
            classify(&recent, level),
            Verdict::Review(vec![Caution::Recent])
        );
    }
}

#[test]
fn no_level_moves_a_held_finding_out_of_protected() {
    let held = [
        Facts {
            proposal: Proposal::Report,
            ..build_output()
        },
        Facts {
            layout_verified: false,
            ..build_output()
        },
        Facts {
            consequence: None,
            ..build_output()
        },
        Facts {
            tracked: Tracked::Unwalked(WalkGap::Unread),
            ..build_output()
        },
        Facts {
            activity: Activity::Unknown(ActivityGap::Uninspected(1)),
            ..build_output()
        },
        Facts {
            modified: Modified::Unknown(WalkGap::Unread),
            ..build_output()
        },
    ];

    for facts in &held {
        let at = |level| classify(facts, level);
        assert_eq!(at(Level::Careful).bucket(), Bucket::Protected, "{facts:?}");
        assert_eq!(at(Level::Careful), at(Level::Balanced), "{facts:?}");
        assert_eq!(at(Level::Thorough), at(Level::Balanced), "{facts:?}");
    }
}

#[test]
fn each_level_has_its_own_recent_window_and_a_rule_can_only_lengthen_it() {
    let aged = |days, recent_days| Facts {
        modified: Modified::Ago(days * DAY),
        recent_days,
        ..build_output()
    };
    let recent = Verdict::Review(vec![Caution::Recent]);

    for &level in Level::ALL {
        let (_, _, window) = table(level);
        assert_eq!(classify(&aged(window - 1, None), level), recent, "{level}");
        assert_eq!(
            classify(&aged(window, None), level),
            Verdict::Safe,
            "{level}"
        );
        // A rule's shorter window changes nothing; a longer one wins.
        assert_eq!(
            classify(&aged(window - 1, Some(1)), level),
            recent,
            "{level}"
        );
        assert_eq!(classify(&aged(29, Some(30)), level), recent, "{level}");
        assert_eq!(
            classify(&aged(30, Some(30)), level),
            Verdict::Safe,
            "{level}"
        );
    }
}

const TARGET: &str = "/home/u/projects/app/target";

fn cargo() -> RuleId {
    RuleId("builtin.cargo.target".to_owned())
}

/// The Cargo rule with `settings` of its own.
fn own(settings: RuleSettings) -> BTreeMap<RuleId, RuleSettings> {
    BTreeMap::from([(cargo(), settings)])
}

fn held(path: &str) -> HeldPath {
    HeldPath {
        forms: vec![PathBuf::from(path)],
        ..HeldPath::default()
    }
}

const fn id(file: u64) -> FileIdentity {
    FileIdentity { volume: 1, file }
}

/// Build output at `TARGET` whose own identity is 30, under ancestors 20 and 10.
fn identified() -> Facts {
    Facts {
        lineage: Lineage(vec![Some(id(30)), Some(id(20)), Some(id(10))]),
        ..build_output()
    }
}

#[test]
fn a_kept_or_excluded_path_holds_a_finding_at_inside_or_above_it_whatever_the_case() {
    // At, inside (the path is the project: the target is in it), above (removing the
    // target would remove the kept file), a case variant and a trailing separator.
    let holding = [
        TARGET,
        "/home/u/projects/app",
        "/home/u/projects/app/target/release/bench",
        "/HOME/U/Projects/App/Target",
        "/home/u/projects/app/target/",
    ];
    for path in holding {
        let kept = Policy {
            keep: vec![held(path)],
            ..Policy::default()
        };
        let excluded = Policy {
            exclude: vec![held(path)],
            ..Policy::default()
        };
        let facts = build_output();
        assert_eq!(
            kept.verdict(&cargo(), Path::new(TARGET), &facts),
            Verdict::Protected(vec![Hold::Kept]),
            "{path}"
        );
        assert_eq!(
            excluded.verdict(&cargo(), Path::new(TARGET), &facts),
            Verdict::Protected(vec![Hold::Excluded]),
            "{path}"
        );
    }
    // A sibling whose name only starts the same isn't held.
    let sibling = Policy {
        keep: vec![held("/home/u/projects/app/targets")],
        ..Policy::default()
    };
    assert_eq!(
        sibling.verdict(&cargo(), Path::new(TARGET), &build_output()),
        Verdict::Safe
    );
}

#[test]
fn a_kept_path_matches_by_file_identity_whatever_path_reaches_it() {
    // Reached through a linked parent, or named in another Unicode normalisation: the
    // path differs, the identity doesn't.
    let elsewhere = |own, above: Vec<Option<FileIdentity>>| Policy {
        keep: vec![HeldPath {
            forms: vec![PathBuf::from("/var/home/u/p")],
            lineage: Lineage([vec![Some(id(own))], above].concat()),
            note: None,
        }],
        ..Policy::default()
    };
    let at = |policy: &Policy, facts: &Facts| policy.verdict(&cargo(), Path::new(TARGET), facts);
    let kept = Verdict::Protected(vec![Hold::Kept]);

    // The finding itself, a directory above it, a directory below it.
    assert_eq!(at(&elsewhere(30, vec![Some(id(5))]), &identified()), kept);
    assert_eq!(at(&elsewhere(20, vec![Some(id(5))]), &identified()), kept);
    assert_eq!(at(&elsewhere(40, vec![Some(id(30))]), &identified()), kept);
    // Unrelated identity: not held.
    assert_eq!(
        at(&elsewhere(99, vec![Some(id(5))]), &identified()),
        Verdict::Safe
    );
    // A finding whose own identity or ancestry couldn't be read might be below it.
    let unread = Facts {
        lineage: Lineage(vec![Some(id(30)), None, Some(id(10))]),
        ..build_output()
    };
    assert_eq!(at(&elsewhere(99, vec![Some(id(5))]), &unread), kept);
}

#[test]
fn a_tilde_entry_under_an_unknown_home_holds_every_finding_as_excluded() {
    let unexpanded = HeldPath::default();
    for policy in [
        Policy {
            keep: vec![unexpanded.clone()],
            ..Policy::default()
        },
        Policy {
            exclude: vec![unexpanded],
            ..Policy::default()
        },
    ] {
        assert_eq!(
            policy.verdict(&cargo(), Path::new(TARGET), &build_output()),
            Verdict::Protected(vec![Hold::Excluded])
        );
    }
}

#[test]
fn kept_excluded_and_disabled_hold_at_every_level_and_every_rule_level() {
    let disabled = RuleSettings {
        disabled: true,
        ..RuleSettings::default()
    };
    for &level in Level::ALL {
        for &rule_level in Level::ALL {
            let settings = RuleSettings {
                level: Some(rule_level),
                ..RuleSettings::default()
            };
            let policy = |rule| Policy {
                level,
                rules: own(rule),
                ..Policy::default()
            };
            let off = policy(RuleSettings {
                level: Some(rule_level),
                ..disabled
            });
            let kept = Policy {
                keep: vec![held(TARGET)],
                ..policy(settings)
            };
            let excluded = Policy {
                exclude: vec![held(TARGET)],
                ..policy(settings)
            };
            let verdict =
                |config: &Policy| config.verdict(&cargo(), Path::new(TARGET), &build_output());
            assert_eq!(verdict(&off), Verdict::Protected(vec![Hold::Disabled]));
            assert_eq!(verdict(&kept), Verdict::Protected(vec![Hold::Kept]));
            assert_eq!(verdict(&excluded), Verdict::Protected(vec![Hold::Excluded]));
        }
    }
    // A held finding still shows every other reason it is held.
    let unknown = Facts {
        layout_verified: false,
        ..build_output()
    };
    let off = Policy {
        rules: own(disabled),
        ..Policy::default()
    };
    assert_eq!(
        off.verdict(&cargo(), Path::new(TARGET), &unknown),
        Verdict::Protected(vec![Hold::Disabled, Hold::UnknownLayout])
    );
}

#[test]
fn a_rules_own_level_wins_over_the_global_one_for_that_rule_only() {
    // Reinstalled: Safe at Balanced, Review at Careful.
    let reinstalled = Facts {
        consequence: Some(Consequence {
            recovery: Recovery::Reinstall,
            loss: &[],
            cost: Cost::Unknown,
        }),
        ..build_output()
    };
    let careful_cargo = Policy {
        level: Level::Balanced,
        rules: own(RuleSettings {
            level: Some(Level::Careful),
            ..RuleSettings::default()
        }),
        ..Policy::default()
    };
    let node = RuleId("builtin.node.node-modules".to_owned());
    let at = |rule: &RuleId| careful_cargo.verdict(rule, Path::new(TARGET), &reinstalled);

    assert_eq!(at(&cargo()), Verdict::Review(vec![Caution::Recovery]));
    assert_eq!(at(&node), Verdict::Safe);
    assert_eq!(careful_cargo.level_of(&cargo()), Level::Careful);
    assert_eq!(careful_cargo.level_of(&node), Level::Balanced);
}

#[test]
fn a_configured_recent_window_only_lengthens_the_effective_one() {
    let aged = |days, recent_days| Facts {
        modified: Modified::Ago(days * DAY),
        recent_days,
        ..build_output()
    };
    let with = |days| Policy {
        rules: own(RuleSettings {
            recent_days: Some(days),
            ..RuleSettings::default()
        }),
        ..Policy::default()
    };
    let at = |policy: &Policy, facts: &Facts| policy.verdict(&cargo(), Path::new(TARGET), facts);
    let recent = Verdict::Review(vec![Caution::Recent]);

    // Longer than Balanced's 7 days: it wins.
    assert_eq!(at(&with(30), &aged(29, None)), recent);
    assert_eq!(at(&with(30), &aged(30, None)), Verdict::Safe);
    // Shorter: no effect.
    assert_eq!(at(&with(1), &aged(6, None)), recent);
    // The longest of the level's, the rule's and the config's.
    assert_eq!(at(&with(10), &aged(19, Some(20))), recent);
    assert_eq!(
        with(10).recent_window_days(&cargo(), &aged(0, Some(20))),
        20
    );
}
