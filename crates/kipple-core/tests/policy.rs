//! The bucket table of `docs/04-safety-model.md` §2a: the first matching row wins,
//! unknown facts hold a finding in Protected and never make it Review, and the level moves
//! only the line between Safe and Review.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use kipple_core::{
    Activity, ActivityGap, Bucket, Caution, Consequence, Cost, Facts, Hold, Level, Loss, Method,
    Modified, ProbeError, Proposal, Recovery, Tracked, Verdict, classify,
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
    }
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
                modified: Modified::Unknown,
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
        (Tracked::Unwalked, Hold::UnknownGit),
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
            tracked: Tracked::Unwalked,
            ..build_output()
        },
        Facts {
            activity: Activity::Unknown(ActivityGap::Uninspected(1)),
            ..build_output()
        },
        Facts {
            modified: Modified::Unknown,
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
