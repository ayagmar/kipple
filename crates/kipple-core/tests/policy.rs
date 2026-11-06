//! The bucket table of `docs/04-safety-model.md` §2a: the first matching row wins,
//! unknown facts hold a finding in Protected and never make it Review, and the level moves
//! only the line between Safe and Review.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kipple_core::{
    Activity, ActivityGap, AgentLabel, Bucket, Caution, Checkout, Consequence, Cost, Facts,
    FileIdentity, GitGap, HeldPath, Hold, IntegrationId, Level, Lineage, Loss, Method, Modified,
    NativeKind, ObjectId, Policy, ProbeError, Proposal, Recovery, Registration, RegistrationState,
    ReleaseFacts, ReleaseGroup, ReleaseRole, ReleaseState, ReleaseVersion, Retained, RuleId,
    RuleSettings, Seen, SemanticFacts, Settlement, Subject, Supplied, ToolGap, Tracked, Verdict,
    WalkGap, WorktreeFacts,
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
        supplied: Supplied::default(),
        unmet: Vec::new(),
        settlement: Settlement::Unsettled,
        integration: IntegrationId("test"),
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
                    seen: Seen::Executable(PathBuf::from("/p/target/debug/app")),
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

/// Catches git's full answer still holding a finding inside a repository: the files it
/// tracks are left out of the plan, so the rest is judged as anywhere else (D-066).
#[test]
fn a_finding_inside_a_repository_git_answered_for_is_not_held() {
    let facts = Facts {
        tracked: Tracked::Asked {
            work_tree: PathBuf::from("/p"),
            tracked: 2,
        },
        ..build_output()
    };
    assert_eq!(balanced(&facts), balanced(&build_output()));
}

#[test]
fn a_repository_around_or_inside_a_finding_or_one_not_ruled_out_holds_it_in_protected() {
    let cases = [
        (
            Tracked::InsideRepository {
                work_tree: PathBuf::from("/p"),
                gap: GitGap::Failed,
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

const fn id(file: u128) -> FileIdentity {
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

/// Catches a path that skips settle removing a release, because a missing decision read as
/// no objection (04 §2a).
#[test]
fn unsettled_release_holds() {
    let holds = |settlement, level| {
        let facts = Facts {
            settlement,
            ..subject_facts(REMOVE_RELEASE(), release(), CODEX)
        };
        match classify(&facts, level) {
            Verdict::Protected(holds) => holds,
            Verdict::Safe | Verdict::Review(_) => Vec::new(),
        }
    };

    for level in Level::ALL.iter().copied() {
        assert_eq!(holds(Settlement::Unsettled, level), [Hold::Unsettled]);
        let kept = Settlement::Retained(Retained::Previous);
        assert_eq!(holds(kept, level), [Hold::Retained]);
        assert_eq!(holds(Settlement::Candidate, level), []);
    }
}

/// Catches settlement leaking into removals that have nothing to settle.
#[test]
fn only_a_release_removal_waits_for_settlement() {
    let plain = build_output();

    assert_eq!(plain.settlement, Settlement::Unsettled);
    assert_eq!(balanced(&plain), Verdict::Safe);
}

fn release() -> Subject {
    Subject::Release(ReleaseFacts {
        group: ReleaseGroup {
            component: "cli".to_owned(),
            target: "x86_64-unknown-linux-musl".to_owned(),
        },
        version: ReleaseVersion("0.165.0".to_owned()),
        role: ReleaseRole::Installed,
        files: Vec::new(),
        locks: Vec::new(),
        state: ReleaseState::Sound,
    })
}

fn worktree() -> Subject {
    Subject::Worktree(Box::new(WorktreeFacts {
        registration: Registration {
            path: PathBuf::from("/home/u/wt"),
            head: Some(ObjectId("0dc8".to_owned())),
            checkout: Checkout::Branch("wt".to_owned()),
            locked: None,
            prunable: false,
            bare: false,
        },
        state: RegistrationState::RegisteredPresent,
        repository: PathBuf::from("/home/u/repo"),
        admin: None,
        witness: Err(GitGap::Unsupported),
        tool: Err(ToolGap::Missing),
        agent: AgentLabel::Unknown,
    }))
}

const fn semantic(kind: NativeKind) -> Subject {
    Subject::Semantic(SemanticFacts {
        kind,
        tool: Err(ToolGap::Missing),
    })
}

/// A finding of `integration` proposing `proposal`, that its integration says is `subject`,
/// fit to remove in every other way.
fn subject_facts(proposal: Proposal, subject: Subject, integration: IntegrationId) -> Facts {
    Facts {
        proposal,
        supplied: Supplied {
            subject,
            ..Supplied::default()
        },
        settlement: Settlement::Candidate,
        integration,
        ..build_output()
    }
}

fn holds(facts: &Facts) -> Vec<Hold> {
    match balanced(facts) {
        Verdict::Protected(holds) => holds,
        Verdict::Safe | Verdict::Review(_) => Vec::new(),
    }
}

const GIT: IntegrationId = IntegrationId::GIT;
const CODEX: IntegrationId = IntegrationId("codex");
const REMOVE: fn() -> Proposal = || Proposal::Remove(vec![Method::Delete]);
const REMOVE_RELEASE: fn() -> Proposal = || Proposal::RemoveRelease(vec![Method::Delete]);
const WORKTREE_REMOVE: Proposal = Proposal::Native(NativeKind::GitWorktreeRemove);
const CACHE_CLEAN: Proposal = Proposal::Native(NativeKind::NpmCacheClean);

/// Catches a rule removing a directory as a release when nothing said it is one, so settle
/// never saw it.
#[test]
fn release_removal_needs_a_release_subject() {
    let others = [
        Subject::Plain,
        worktree(),
        semantic(NativeKind::NpmCacheClean),
    ];
    for subject in others {
        let facts = subject_facts(REMOVE_RELEASE(), subject, CODEX);
        assert_eq!(holds(&facts), [Hold::SubjectMismatch]);
    }
    assert_eq!(
        holds(&subject_facts(REMOVE_RELEASE(), release(), CODEX)),
        []
    );
}

/// Catches a worktree going down the file path, which skips everything its own operation
/// proves, and anything else that isn't plain files with it.
#[test]
fn worktree_subject_under_remove_files_is_refused() {
    let others = [worktree(), release(), semantic(NativeKind::NpmCacheClean)];
    for subject in others {
        let facts = subject_facts(REMOVE(), subject, GIT);
        assert_eq!(holds(&facts), [Hold::SubjectMismatch]);
    }
}

/// Catches a worktree passing as a release, the other way to reach its files.
#[test]
fn worktree_subject_under_release_removal_is_refused() {
    let facts = subject_facts(REMOVE_RELEASE(), worktree(), GIT);

    assert_eq!(holds(&facts), [Hold::SubjectMismatch]);
}

/// Catches the worktree exception opening for anything but git's own worktrees: it is
/// keyed three ways (04 §3.2).
#[test]
fn a_worktree_removal_needs_git_and_a_worktree() {
    assert_eq!(holds(&subject_facts(WORKTREE_REMOVE, worktree(), GIT)), []);
    let wrong = [
        subject_facts(WORKTREE_REMOVE, worktree(), CODEX),
        subject_facts(WORKTREE_REMOVE, Subject::Plain, GIT),
        subject_facts(WORKTREE_REMOVE, release(), GIT),
        subject_facts(
            WORKTREE_REMOVE,
            semantic(NativeKind::GitWorktreeRemove),
            GIT,
        ),
    ];
    for facts in &wrong {
        assert_eq!(holds(facts), [Hold::SubjectMismatch]);
    }
}

/// Catches a tool's command running on something its integration never said it may
/// clean, or a subject that names another command.
#[test]
fn a_cache_clean_needs_its_own_semantic_subject() {
    let cleans = semantic(NativeKind::NpmCacheClean);

    assert_eq!(holds(&subject_facts(CACHE_CLEAN, cleans, CODEX)), []);
    let wrong = [
        subject_facts(CACHE_CLEAN, Subject::Plain, CODEX),
        subject_facts(CACHE_CLEAN, semantic(NativeKind::GitWorktreeRemove), CODEX),
        subject_facts(CACHE_CLEAN, worktree(), GIT),
    ];
    for facts in &wrong {
        assert_eq!(holds(facts), [Hold::SubjectMismatch]);
    }
}

/// Catches the tracked-file exception reaching a native operation it wasn't written for,
/// and a native command deleting files git tracks: it can't leave them out as a file plan
/// does.
#[test]
fn other_native_kind_keeps_tracked_protection() {
    let work_tree = || PathBuf::from("/home/u/repo");
    let tracked = |tracked| Facts {
        tracked,
        ..subject_facts(CACHE_CLEAN, semantic(NativeKind::NpmCacheClean), CODEX)
    };

    let holding = tracked(Tracked::HoldsRepository {
        work_tree: work_tree(),
    });
    assert_eq!(holds(&holding), [Hold::HoldsRepository]);
    let inside = tracked(Tracked::Asked {
        work_tree: work_tree(),
        tracked: 2,
    });
    assert_eq!(holds(&inside), [Hold::TrackedInsideNative]);
    let none = tracked(Tracked::Asked {
        work_tree: work_tree(),
        tracked: 0,
    });
    assert_eq!(holds(&none), []);

    let repository_above_tracks_it = Facts {
        tracked: Tracked::Asked {
            work_tree: work_tree(),
            tracked: 1,
        },
        ..subject_facts(WORKTREE_REMOVE, worktree(), GIT)
    };
    assert_eq!(
        holds(&repository_above_tracks_it),
        [Hold::TrackedInsideNative]
    );
}

/// Catches a report of a worktree or release being held as a mismatch, which would hide
/// that it only reports.
#[test]
fn a_report_goes_with_any_subject() {
    for subject in [release(), worktree()] {
        let facts = subject_facts(Proposal::Report, subject, GIT);
        assert_eq!(holds(&facts), [Hold::ReportOnly]);
    }
}
