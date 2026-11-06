//! git worktrees against git 2.56.0's on-disk layout, built with `FixtureTree`, with what git
//! says scripted. The rules are `report` with no predicates until m4c, so these tests read
//! the facts supplied, never a verdict. The tests that run real git are in
//! `crates/kipple/tests/git_worktrees.rs`: this crate can't reach the platform's `GitQuery`.
#![expect(
    clippy::indexing_slicing,
    reason = "a finding missing from a report fails the test"
)]

#[path = "fixtures/git/demo.rs"]
mod demo;
#[path = "fixtures/git/harness.rs"]
mod harness;
#[path = "fixtures/tree.rs"]
#[expect(dead_code, reason = "this test binary uses part of the builder")]
mod tree;
#[path = "fixtures/git/v2_56_0.rs"]
mod v2_56_0;

use std::path::{Path, PathBuf};

use harness::{Built, Run, Script, built, built_with, clean};
use kipple_adapters::builtin;
use kipple_core::{
    AgentLabel, ConfigGate, Evidence, FactValue, FileIdentity, Finding, GitGap, HeldPath, Lineage,
    Omission, OwnerStatus, Proposal, Protection, Protections, RegistrationState, Subject,
    WorktreeState,
};
use tree::FixtureTree;
use v2_56_0::{APP, Links, PRESENT, SWIFT_FOX, WARSAW};

const MAIN: &str = "home/projects/app";
const CHECKOUT: &str = "home/projects/app-wt/fix-1";
const ANOMALIES: &str = "builtin.git.worktree-anomalies";
const INVENTORY: &str = "builtin.git.worktree-inventory";

/// One repository at [`MAIN`] with a linked worktree at each of `paths`.
fn repository_with(paths: &[&str], add: impl Fn(&mut FixtureTree)) -> Built {
    let mut tree = FixtureTree::new();
    v2_56_0::repository(&mut tree, MAIN);
    for (index, path) in paths.iter().enumerate() {
        v2_56_0::linked(
            &mut tree,
            MAIN,
            &format!("wt{index}"),
            path,
            Links::Relative,
        );
    }
    add(&mut tree);
    built(&tree)
}

/// Scans a tree of [`CHECKOUT`] alone, with `state` said of it.
fn scan_checkout(
    add: impl Fn(&mut FixtureTree),
    state: impl Fn(&Path, &Path) -> WorktreeState,
) -> Run {
    let tree = repository_with(&[CHECKOUT], add);
    let (main, path) = (tree.at(MAIN), tree.at(CHECKOUT));
    let script = Script::repository(&main, &[&path]).state(&path, state(&main, &path));
    tree.scan(script)
}

/// Adds node dependencies a Node rule removes, at `at`.
fn node_project(tree: &mut FixtureTree, at: &str) {
    tree.file(&format!("{at}/package.json"), b"{\"name\":\"web\"}")
        .file(
            &format!("{at}/package-lock.json"),
            b"{\"lockfileVersion\":3}",
        )
        .file(
            &format!("{at}/node_modules/.package-lock.json"),
            b"{\"lockfileVersion\":3}",
        );
}

fn eight(tree: impl FnOnce(&Path) -> FixtureTree) -> Run {
    let base = built_with(tree);
    let main = base.at(APP);
    let paths: Vec<PathBuf> = PRESENT.iter().map(|(_, path)| base.at(path)).collect();
    let missing = base.at(SWIFT_FOX);
    let linked: Vec<&Path> = paths
        .iter()
        .map(PathBuf::as_path)
        .chain([missing.as_path()])
        .collect();
    let mut gone = clean(&main, &missing);
    gone.registration = Ok(RegistrationState::RegisteredMissing);
    let script = Script::repository(&main, &linked)
        .record(&missing, |record| record.prunable = true)
        .state(&missing, gone);
    base.scan(script)
}

fn eight_absolute() -> Run {
    eight(|base| {
        let mut tree = FixtureTree::new();
        v2_56_0::eight(&mut tree, Links::Absolute(base));
        tree
    })
}

fn agent(finding: &Finding) -> Option<AgentLabel> {
    finding.evidence.iter().find_map(|evidence| {
        if let Evidence::Worktree { agent } = evidence {
            Some(*agent)
        } else {
            None
        }
    })
}

fn is_report(finding: &Finding) -> bool {
    let proposal = finding.facts.as_ref().map(|facts| &facts.proposal);
    matches!(proposal, Some(Proposal::Report | Proposal::Advice))
}

const fn state_name(state: RegistrationState) -> FactValue {
    FactValue::Enum(state.name())
}

/// Catches a worktree labelled from anything but its place, or a mixed set of states read as
/// one: S5 git §8's table, row by row, with the state of the two that aren't clean.
#[test]
fn eight_agent_worktrees_classified_with_labels() {
    let run = eight_absolute();
    let at = |path: &str| run.built.at(path);
    let expected = [
        ("feature-a", AgentLabel::ClaudeCode),
        ("agent-a1b2c3d4e5f607182", AgentLabel::ClaudeCode),
        ("fix-b", AgentLabel::ClaudeCode),
        ("ab12", AgentLabel::Codex),
        ("worktree-brave-river-1a2b", AgentLabel::Herdr),
        ("fix-1", AgentLabel::Unknown),
    ];

    for ((id, label), (listed, path)) in expected.iter().zip(PRESENT) {
        assert_eq!(*id, listed);
        assert_eq!(agent(run.only(&at(path))), Some(*label), "{path}");
        assert_eq!(
            run.fact(&at(path), "registration_state"),
            state_name(RegistrationState::RegisteredPresent),
            "{path}"
        );
    }
    assert_eq!(agent(run.only(&at(SWIFT_FOX))), Some(AgentLabel::Cursor));
    assert_eq!(
        run.fact(&at(SWIFT_FOX), "registration_state"),
        state_name(RegistrationState::RegisteredMissing)
    );
    assert_eq!(agent(run.only(&at(WARSAW))), Some(AgentLabel::Conductor));
    assert_eq!(
        run.fact(&at(WARSAW), "registration_state"),
        state_name(RegistrationState::PresentUnregistered)
    );
    assert_eq!(run.of("git").len(), 8);
}

/// Catches a report twin of a worktree: it would stand at the same path as the removal and
/// shelter it behind a report, or count one checkout twice.
#[test]
fn clean_registered_worktree_has_exactly_one_finding() {
    let run = scan_checkout(|_| {}, clean);

    let path = run.built.at(CHECKOUT);
    assert_eq!(run.at(&path).len(), 1);
    assert_eq!(run.of("git").len(), 1, "and none for anything else");
}

/// Catches a registration dropped from the report, or one found twice because its repository
/// was reached through two of its worktrees, which each list it.
#[test]
fn every_registration_has_exactly_one_finding() {
    let run = eight_absolute();

    for (_, path) in PRESENT {
        assert_eq!(run.at(&run.built.at(path)).len(), 1, "{path}");
    }
    assert_eq!(run.at(&run.built.at(SWIFT_FOX)).len(), 1);
}

/// Catches case 6, a `.git` file whose admin entry is gone, taken for a repository git can be
/// asked about: `list` fails there, and the checkout would drop out as an omission.
#[test]
fn broken_gitdir_under_worktrees_is_present_unregistered() {
    let run = eight_absolute();
    let warsaw = run.built.at(WARSAW);

    assert_eq!(
        run.fact(&warsaw, "registration_state"),
        state_name(RegistrationState::PresentUnregistered)
    );
    assert_eq!(run.only(&warsaw).rule.0, ANOMALIES);
    let omissions = run.report.integrations.iter().flat_map(|i| &i.omissions);
    let git = omissions.filter(|o| matches!(o, Omission::GitInventory { .. }));
    assert_eq!(git.count(), 0, "no list was asked of it");
}

/// Catches a registered path with no directory offered as space to reclaim, or left without
/// `git worktree prune`, the one thing that clears it.
#[test]
fn registered_missing_reclaims_nothing_and_advises_prune() {
    let run = eight_absolute();
    let finding = run.only(&run.built.at(SWIFT_FOX));

    assert_eq!(finding.rule.0, ANOMALIES);
    assert!(is_report(finding), "a missing checkout is report only");
    let reclaim = finding.size.as_ref().map_or(0, |size| size.apparent.0);
    assert_eq!(reclaim, 0);
    assert_eq!(
        run.snippets()
            .iter()
            .filter(|s| s.contains("prune"))
            .count(),
        1,
        "advised once"
    );
}

/// Catches a checkout no repository lists reaching a removal: nothing proves whose it is, and
/// its subject carries no registration to remove it by.
#[test]
fn present_unregistered_is_report_only() {
    let run = eight_absolute();
    let finding = run.only(&run.built.at(WARSAW));

    assert!(
        is_report(finding),
        "an unregistered checkout is report only"
    );
    let subject = finding.facts.as_ref().map(|facts| &facts.supplied.subject);
    assert!(
        matches!(subject, Some(Subject::Plain)),
        "it has no registration: {subject:?}"
    );
}

/// Catches `prune` advised for a registration whose checkout is there but disagrees: prune
/// drops the admin entry, which strands that checkout.
#[test]
fn registered_invalid_never_advises_prune() {
    let run = scan_checkout(
        |_| {},
        |main, path| {
            let mut state = clean(main, path);
            state.registration = Ok(RegistrationState::RegisteredInvalid);
            state
        },
    );

    let path = run.built.at(CHECKOUT);
    assert_eq!(
        run.fact(&path, "registration_state"),
        state_name(RegistrationState::RegisteredInvalid)
    );
    assert_eq!(run.only(&path).rule.0, ANOMALIES);
    assert!(
        run.snippets().iter().all(|s| !s.contains("prune")),
        "no prune in {:?}",
        run.snippets()
    );
}

/// Catches a row git couldn't answer defaulting to a harmless value: each gap, whether git is
/// missing, too old, failed, timed out or was cut short, leaves every row it covers unknown.
#[test]
fn git_failure_leaves_facts_unknown() {
    const ROWS: [&str; 11] = [
        "worktree.linked",
        "repository_identity",
        "registration_state",
        "worktree.submodules",
        "worktree.config_gate",
        "git.tracked_changes",
        "git.untracked_files",
        "git.hidden_changes",
        "worktree.operation_in_progress",
        "worktree.per_worktree_refs",
        "worktree.index_locked",
    ];
    for gap in [
        GitGap::Missing,
        GitGap::TooOld,
        GitGap::Failed,
        GitGap::Truncated,
    ] {
        let run = scan_checkout(|_| {}, |_, _| WorktreeState::unknown(gap));

        let path = run.built.at(CHECKOUT);
        for row in ROWS {
            assert_eq!(run.fact(&path, row), FactValue::Unknown, "{row} on {gap:?}");
        }
        assert_eq!(
            run.fact(&path, "worktree.head_reachable"),
            FactValue::Unknown
        );
        assert_eq!(
            run.fact(&path, "worktree.unclassified_ignored"),
            FactValue::Unknown
        );
    }
}

/// Catches a list that failed reading as a repository with no worktrees: its checkouts would
/// vanish from the report with nothing saying so, and the layout would read as supported.
#[test]
fn a_list_that_fails_is_an_omission_and_no_support() {
    let tree = repository_with(&[CHECKOUT], |_| {});
    let main = tree.at(MAIN);
    let script = Script::repository(&main, &[]).list_fails(GitGap::Truncated);

    let run = tree.scan(script);

    let git = run.report.integrations.iter().find(|i| i.id.0 == "git");
    let git = git.unwrap_or_else(|| panic!("git ran"));
    let gaps = git.omissions.iter().filter_map(|omission| {
        if let Omission::GitInventory { gap, .. } = omission {
            Some(*gap)
        } else {
            None
        }
    });
    assert_eq!(gaps.collect::<Vec<_>>(), [GitGap::Truncated]);
    assert_eq!(git.owner, OwnerStatus::Unsupported);
    assert_eq!(run.of("git").len(), 0);
}

/// Catches a build artifact kipple itself can remove counting against its worktree: every
/// agent worktree with dependencies would read as holding unclassified files.
#[test]
fn clean_worktree_with_a_classified_ignored_artifact_supplies_no_block() {
    let run = scan_checkout(
        |tree| node_project(tree, CHECKOUT),
        |main, path| {
            let mut state = clean(main, path);
            state.ignored = Ok(vec![PathBuf::from("node_modules/")]);
            state
        },
    );

    assert_eq!(
        run.fact(&run.built.at(CHECKOUT), "worktree.unclassified_ignored"),
        FactValue::Count(0)
    );
}

/// Catches a report standing for proof: a nested worktree is a finding at the ignored path,
/// and only says it exists, so what it holds would be lost with its parent, unclassified.
#[test]
fn report_finding_does_not_classify_an_ignored_path() {
    const NESTED: &str = "home/projects/app-wt/fix-1/scratch";
    let tree = repository_with(&[CHECKOUT, NESTED], |_| {});
    let (main, path, nested) = (tree.at(MAIN), tree.at(CHECKOUT), tree.at(NESTED));
    let mut state = clean(&main, &path);
    state.ignored = Ok(vec![PathBuf::from("scratch/")]);
    let script = Script::repository(&main, &[&path, &nested]).state(&path, state);

    let run = tree.scan(script);

    assert_eq!(run.at(&nested).len(), 1, "the nested worktree is a finding");
    assert_eq!(
        run.fact(&path, "worktree.unclassified_ignored"),
        FactValue::Count(1)
    );
}

/// Catches a checkout no repository lists classifying the ignored path it stands at: it is a
/// plain finding with a verified layout, and it holds a repository nothing was asked about.
#[test]
fn an_unregistered_checkout_does_not_classify_an_ignored_path() {
    const STRAY: &str = "home/projects/app-wt/fix-1/scratch";
    let tree = repository_with(&[CHECKOUT], |tree| {
        v2_56_0::gitfile(tree, MAIN, "gone", STRAY, Links::Relative);
    });
    let (main, path) = (tree.at(MAIN), tree.at(CHECKOUT));
    let mut state = clean(&main, &path);
    state.ignored = Ok(vec![PathBuf::from("scratch/")]);
    let script = Script::repository(&main, &[&path]).state(&path, state);

    let run = tree.scan(script);

    assert_eq!(
        run.at(&path.join("scratch")).len(),
        1,
        "the stray is a finding"
    );
    assert_eq!(
        run.fact(&path, "worktree.unclassified_ignored"),
        FactValue::Count(1)
    );
}

/// Catches an ignored path no finding stands at reading as classified: an `.env` is what the
/// worktree exists to keep from being lost.
#[test]
fn an_ignored_file_nothing_found_is_unclassified() {
    let run = scan_checkout(
        |tree| {
            tree.file(&format!("{CHECKOUT}/.env"), b"NOT_A_SECRET=1\n");
        },
        |main, path| {
            let mut state = clean(main, path);
            state.ignored = Ok(vec![PathBuf::from(".env")]);
            state
        },
    );

    assert_eq!(
        run.fact(&run.built.at(CHECKOUT), "worktree.unclassified_ignored"),
        FactValue::Count(1)
    );
}

/// Catches a failed `status` settling to zero: with no list of what is ignored, nothing shows
/// the worktree holds none.
#[test]
fn an_ignored_list_git_did_not_give_stays_unknown() {
    let run = scan_checkout(
        |_| {},
        |main, path| {
            let mut state = clean(main, path);
            state.ignored = Err(GitGap::Failed);
            state
        },
    );

    assert_eq!(
        run.fact(&run.built.at(CHECKOUT), "worktree.unclassified_ignored"),
        FactValue::Unknown
    );
}

/// Catches a linked worktree closing to Node's rule: a project in a worktree is a project,
/// and its dependencies come back with one install.
#[test]
fn artifacts_inside_a_project_worktree_stay_removable() {
    let run = scan_checkout(|tree| node_project(tree, CHECKOUT), clean);

    let dependencies = run.built.at(CHECKOUT).join("node_modules");
    let node = run.of("node");
    let found = node.iter().find(|finding| finding.path == dependencies);
    let found = found.unwrap_or_else(|| panic!("found in {node:?}"));
    let proposal = found.facts.as_ref().map(|facts| &facts.proposal);
    assert!(
        matches!(proposal, Some(Proposal::Remove(_))),
        "its rule removes it: {proposal:?}"
    );
}

/// Catches the recorded limit lifted by accident: a worktree in Codex's closed home holds its
/// artifact until Codex opens the area, so that artifact can't be removed and can't classify
/// the ignored path either.
#[test]
fn artifact_in_a_codex_worktree_is_held_and_unclassified() {
    const CODEX: &str = "home/.codex/worktrees/ab12/app";
    let tree = repository_with(&[CODEX], |tree| node_project(tree, CODEX)).projects_at("home");
    let (main, path) = (tree.at(MAIN), tree.at(CODEX));
    let mut state = clean(&main, &path);
    state.ignored = Ok(vec![PathBuf::from("node_modules/")]);
    let script = Script::repository(&main, &[&path]).state(&path, state);

    let run = tree.scan(script);

    assert_eq!(
        run.fact(&path, "worktree.unclassified_ignored"),
        FactValue::Count(1)
    );
    let dependencies = path.join("node_modules");
    let node = run.of("node");
    let found = node.iter().find(|finding| finding.path == dependencies);
    let found = found.unwrap_or_else(|| panic!("found in {node:?}"));
    let identity = |file| Some(FileIdentity { volume: 1, file });
    let descriptors: Vec<_> = builtin::integrations()
        .iter()
        .map(|integration| integration.descriptor())
        .collect();
    let held = |at: &Path| HeldPath {
        forms: vec![at.to_owned()],
        lineage: Lineage(vec![identity(2)]),
        note: None,
    };
    let data = run.built.at("home/.local/share/kipple");
    let protections = Protections::of_host(&run.built.roots(), Some(&data), &descriptors, &held);
    assert_eq!(
        protections.check(
            &dependencies,
            &Lineage(vec![identity(1)]),
            found.integration,
            &run.report.owners()
        ),
        Some(Protection::Subresource)
    );
}

/// Catches the main worktree offered for removal: it is the repository, and git's first
/// record, whatever it looks like, bare included.
#[test]
fn main_worktree_never_selected() {
    let eight = eight_absolute();
    assert_eq!(eight.at(&eight.built.at(APP)).len(), 0);

    let tree = repository_with(&[CHECKOUT], |_| {});
    let (main, path) = (tree.at(MAIN), tree.at(CHECKOUT));
    let script = Script::repository(&main, &[&path]).record(&main, |record| record.bare = true);
    let bare = tree.scan(script);
    assert_eq!(bare.at(&main).len(), 0);
    assert_eq!(bare.at(&path).len(), 1, "its worktree still is one");
}

/// Catches a submodule's `.git` file read as a worktree's: it would turn up as unregistered,
/// and its repository be listed.
#[test]
fn submodule_gitfile_is_no_worktree() {
    let run = repository_with(&[], |tree| {
        tree.file(&format!("{MAIN}/sm/.git"), b"gitdir: ../.git/modules/sm\n")
            .file(&format!("{MAIN}/.git/modules/sm/HEAD"), b"0000\n");
    });
    let main = run.at(MAIN);

    let run = run.scan(Script::repository(&main, &[]));

    assert_eq!(run.of("git").len(), 0);
}

/// Catches a `.git` that isn't UTF-8 dropping its checkout with nothing said: git reads bytes,
/// so a repository below such a directory has to be asked. These are a file's bytes, so no
/// system refuses them.
#[test]
fn a_git_file_that_is_not_utf8_is_asked_about() {
    let tree = repository_with(&[], |tree| {
        tree.file(
            &format!("{CHECKOUT}/.git"),
            b"gitdir: /outside/caf\xff/.git/worktrees/wt\n",
        );
    });
    let (main, path) = (tree.at("outside/app"), tree.at(CHECKOUT));

    let run = tree.scan(Script::repository(&main, &[&path]));

    assert_eq!(run.at(&path).len(), 1);
}

/// Catches `worktree.useRelativePaths` links read against the wrong directory, or `demo.rs`
/// drifting from the layout: a relative `gitdir` reads like an absolute one.
#[test]
fn relative_links_are_read() {
    let run = eight(|_| {
        let mut tree = FixtureTree::new();
        demo::demo(&mut tree);
        tree
    });

    assert_eq!(run.of("git").len(), 8);
    let warsaw = run.built.at(WARSAW);
    assert_eq!(
        run.fact(&warsaw, "registration_state"),
        state_name(RegistrationState::PresentUnregistered)
    );
}

/// Catches a question that failed reading as an answer: a `rev-parse` past its deadline says
/// nothing of whether a repository lists a checkout, so "it moved" would be a false claim.
#[test]
fn a_failed_question_about_an_unlisted_checkout_is_an_omission() {
    let tree = repository_with(&[CHECKOUT], |_| {});
    let (main, path) = (tree.at(MAIN), tree.at(CHECKOUT));
    let mut state = clean(&main, &path);
    state.place = Err(GitGap::Failed);
    let script = Script::repository(&main, &[])
        .unlisted(&path, &main)
        .state(&path, state);

    let run = tree.scan(script);

    assert_eq!(run.at(&path).len(), 0);
    let omissions = run.report.integrations.iter().flat_map(|i| &i.omissions);
    let about_it = omissions.filter(|omission| {
        matches!(omission, Omission::GitInventory { repository, .. } if *repository == path)
    });
    assert_eq!(about_it.count(), 1);
}

/// Catches a cancel reading as an answer the same way, and `finish` going on past one.
#[test]
fn a_cancel_while_asking_about_an_unlisted_checkout_claims_nothing() {
    let tree = repository_with(&[CHECKOUT], |_| {});
    let (main, path) = (tree.at(MAIN), tree.at(CHECKOUT));
    let mut state = clean(&main, &path);
    state.place = Err(GitGap::Failed);
    let script = Script::repository(&main, &[])
        .unlisted(&path, &main)
        .state(&path, state)
        .cancels_at(&path);

    let run = tree.scan(script);

    assert_eq!(run.at(&path).len(), 0);
    let omissions = run.report.integrations.iter().flat_map(|i| &i.omissions);
    let git = omissions.filter(|o| matches!(o, Omission::GitInventory { .. }));
    assert_eq!(git.count(), 0, "a cancel is no failure of git's");
}

/// Catches a checkout that is truly elsewhere going unsaid once failures hold: the link is
/// broken, which is an answer, so it is reported as moved.
#[test]
fn a_checkout_whose_link_is_broken_is_unregistered() {
    let tree = repository_with(&[CHECKOUT], |_| {});
    let (main, path) = (tree.at(MAIN), tree.at(CHECKOUT));
    let mut state = clean(&main, &path);
    state.registration = Ok(RegistrationState::RegisteredInvalid);
    state.place = Err(GitGap::Failed);
    let script = Script::repository(&main, &[])
        .unlisted(&path, &main)
        .state(&path, state);

    let run = tree.scan(script);

    assert_eq!(
        run.fact(&path, "registration_state"),
        state_name(RegistrationState::PresentUnregistered)
    );
}

/// Catches a label guessed from a name: `worktree-<name>` is a Claude habit, and a branch or
/// directory with it elsewhere proves nothing.
#[test]
fn label_comes_from_location_only() {
    const NAMED: &str = "home/projects/app-wt/worktree-brave";
    let tree = repository_with(&[NAMED], |_| {});
    let (main, path) = (tree.at(MAIN), tree.at(NAMED));
    let script = Script::repository(&main, &[&path]).record(&path, |record| {
        record.checkout = kipple_core::Checkout::Branch("worktree-brave".to_owned());
    });

    let run = tree.scan(script);

    assert_eq!(agent(run.only(&path)), Some(AgentLabel::Unknown));
}

/// Catches a label buying anything: two worktrees in the same state supply the same facts
/// and the same proposal, whether an agent's location holds them or none does.
#[test]
fn label_grants_nothing() {
    const AGENT: &str = "home/projects/app/.claude/worktrees/one";
    let tree = repository_with(&[AGENT, CHECKOUT], |_| {});
    let (main, labelled, plain) = (tree.at(MAIN), tree.at(AGENT), tree.at(CHECKOUT));

    let run = tree.scan(Script::repository(&main, &[&labelled, &plain]));

    let values = |path: &Path| {
        let measured = run.only(path).facts.as_ref();
        measured.map(|facts| facts.supplied.values.clone())
    };
    assert_eq!(values(&labelled), values(&plain));
    assert_eq!(run.only(&labelled).rule, run.only(&plain).rule);
    assert_ne!(agent(run.only(&labelled)), agent(run.only(&plain)));
}

/// Catches an agent's directory being a finding for existing: an empty one is no worktree.
#[test]
fn empty_agent_directory_is_no_finding() {
    let run = repository_with(&[], |tree| {
        tree.dir("home/.cursor/worktrees/app/empty")
            .dir(&format!("{MAIN}/.claude/worktrees/empty"));
    });
    let main = run.at(MAIN);

    let run = run.scan(Script::repository(&main, &[]));

    assert_eq!(run.of("git").len(), 0);
}

/// Catches a registered path outside every root reaching a rule that removes: nothing
/// confined it, so it is reported and no more.
#[test]
fn worktree_outside_every_root_is_inventory_only() {
    const ELSEWHERE: &str = "srv/checkouts/app";
    let tree = repository_with(&[ELSEWHERE], |_| {});
    let (main, path) = (tree.at(MAIN), tree.at(ELSEWHERE));

    let run = tree.scan(Script::repository(&main, &[&path]));

    let finding = run.only(&path);
    assert_eq!(finding.rule.0, INVENTORY);
    assert_eq!(
        run.fact(&path, "registration_state"),
        state_name(RegistrationState::RegisteredPresent)
    );
}

/// Catches a config gate's verdict lost on the way to its fact: `blocked` is what stops the
/// rest being asked.
#[test]
fn a_blocked_config_is_supplied_as_blocked() {
    let run = scan_checkout(
        |_| {},
        |main, path| {
            let mut state = clean(main, path);
            state.config_gate = Ok(ConfigGate::Blocked(kipple_core::ConfigClass::Filter));
            state.tracked_changes = Err(GitGap::Failed);
            state
        },
    );

    let path = run.built.at(CHECKOUT);
    assert_eq!(
        run.fact(&path, "worktree.config_gate"),
        FactValue::Enum("blocked")
    );
    assert_eq!(run.fact(&path, "git.tracked_changes"), FactValue::Unknown);
}

/// Catches a path with a space or a byte past ASCII mangled between the list and the report,
/// and a lock lost on a registration whose directory is gone.
#[test]
fn a_locked_checkout_that_is_gone_keeps_its_lock_and_its_name() {
    const ODD: &str = "home/projects/app-wt/\u{e9}t\u{e9} one";
    let tree = repository_with(&[], |_| {});
    let (main, path) = (tree.at(MAIN), tree.at(ODD));
    let mut gone = clean(&main, &path);
    gone.registration = Ok(RegistrationState::RegisteredMissing);
    let script = Script::repository(&main, &[&path])
        .record(&path, |record| record.locked = Some("agent".to_owned()))
        .state(&path, gone);

    let run = tree.scan(script);

    assert_eq!(run.fact(&path, "worktree.locked"), FactValue::Bool(true));
    assert_eq!(run.only(&path).rule.0, ANOMALIES);
}
