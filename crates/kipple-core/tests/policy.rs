//! The bucket table of `docs/04-safety-model.md` §2a at the Balanced level: the first
//! matching row wins, unknown facts hold a finding in Protected and never make it Review,
//! and the level moves only the line between Safe and Review.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use kipple_core::{
    Activity, ActivityGap, Caution, Consequence, Cost, Facts, Hold, Level, Loss, Method, Modified,
    ProbeError, Proposal, Recovery, Tracked, Verdict, classify,
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
