//! Native items through the planner and the apply loop: who authorizes one, what holds
//! one, and what a run of one stores (`docs/04-safety-model.md` §3.2, §6; spec M4 pinned
//! 3, 4, 22, 24).
//!
//! Nothing runs a native command yet, so a reviewed item that passes every check ends in
//! [`Skip::SemanticNotReviewed`]. Each test here names the skip it expects, so a hold that
//! went missing shows as that skip, not as a pass.

use std::path::PathBuf;
use std::sync::Arc;

use kipple_core::format::encode_stored_plan;
use kipple_core::format::{decode_plan_file, decode_stored_plan, encode_plan_file};
use kipple_core::{
    AgentLabel, Authorization, Checkout, Execution, GitGap, Hold, IntegrationId, ItemMethod,
    ItemOutcome, NativeCommand, NativeKind, NativeTool, ObjectId, Plan, PlanMode, Planned,
    Proposal, Registration, RegistrationState, ResolvedTool, ReviewedPlan, ScanReport,
    SemanticFacts, Skip, Subject, ToolGap, Tracked, Unjudged, WorktreeFacts, WorktreeWitness,
};

use super::codec::TestCodec;
use super::world::{Locks, Log, Rescanned, Target, World};
use super::{
    Config, Item, TARGET, apply_as, home, id, plan_all, ports, project, refusals, rel, rescanned,
    scan_of, snapshot, world_of,
};
use kipple_core::{
    ApplyPorts, ApplyRequest, CancelToken, EntryKind, FindingId, LockGap, Method, MutationPorts,
    Recorders, ReleaseFacts, ReleaseGroup, ReleaseRole, ReleaseState, ReleaseVersion, Settlement,
    Status, apply,
};

fn tool(tool: NativeTool) -> ResolvedTool {
    ResolvedTool {
        tool,
        path: PathBuf::from("/usr/bin/tool"),
        identity: id(900),
        version: "1.0.0".to_owned(),
        runtime: None,
    }
}

fn witness() -> WorktreeWitness {
    WorktreeWitness {
        common: id(901),
        admin: id(902),
        gitfile: id(903),
        gitdir_file: id(904),
        head: ObjectId("a".repeat(40)),
        branch: Some("topic".to_owned()),
    }
}

fn worktree_facts(item: &Item) -> WorktreeFacts {
    WorktreeFacts {
        registration: Registration {
            path: item.path.clone(),
            head: Some(ObjectId("a".repeat(40))),
            checkout: Checkout::Branch("topic".to_owned()),
            locked: None,
            prunable: false,
            bare: false,
        },
        state: RegistrationState::RegisteredPresent,
        repository: home("main"),
        admin: Some(home("main/.git/worktrees/topic")),
        witness: Ok(witness()),
        tool: Ok(tool(NativeTool::Git)),
        agent: AgentLabel::Unknown,
    }
}

/// The only finding of `scan` proposing `kind` about `subject`.
fn proposing(mut scan: ScanReport, kind: NativeKind, subject: Subject) -> ScanReport {
    let integration = scan.findings[0].integration;
    let facts = scan.findings[0].facts.as_mut().unwrap();
    facts.proposal = Proposal::Native(kind);
    facts.supplied.subject = subject;
    facts.integration = integration;
    scan
}

/// npm's `_cacache`, directly below the root, and the world holding it.
fn cacache() -> (ScanReport, World) {
    let (mut item, target) = project(0, 100, TARGET);
    item.path = home("_cacache");
    let world = world_of(&[(item.clone(), target)]);
    let subject = Subject::Semantic(SemanticFacts {
        kind: NativeKind::NpmCacheClean,
        tool: Ok(tool(NativeTool::Npm)),
    });
    let scan = proposing(scan_of(&[item]), NativeKind::NpmCacheClean, subject);
    (scan, world)
}

/// A linked worktree whose own `.git` file is at its top, with `extra` beside it.
fn worktree(extra: &[&str]) -> (ScanReport, World) {
    let entries: Vec<&str> = std::iter::once(&".git").chain(extra).copied().collect();
    let (mut item, target) = project(0, 100, &entries);
    item.integration = IntegrationId::GIT;
    let world = world_of(&[(item.clone(), target)]);
    let subject = Subject::Worktree(Box::new(worktree_facts(&item)));
    let scan = proposing(scan_of(&[item]), NativeKind::GitWorktreeRemove, subject);
    (scan, world)
}

fn reviewed(scan: &ScanReport, world: &World) -> Planned {
    plan_all(scan, &Config::new(), world, PlanMode::Review)
}

/// How applying `plan`, reviewed, ended its only item.
fn applied(plan: &Plan, scan: &ScanReport, world: &World) -> ItemOutcome {
    let shown = Authorization::Reviewed(ReviewedPlan::for_core_tests(plan));
    applied_as(plan, shown, scan, world)
}

fn applied_as(plan: &Plan, shown: Authorization, scan: &ScanReport, world: &World) -> ItemOutcome {
    let receipt = apply_as(plan, shown, &rescanned(scan), &Config::new(), world);
    assert_eq!(world.state().ran, Vec::new(), "nothing ran");
    receipt.items[0].outcome.clone()
}

/// Catches `--yes`, `--only X --yes` or a profile reaching a tool's command: at planning
/// and again at apply, whatever else the item passes (pinned 24).
#[test]
fn yes_refuses_native_at_plan_and_apply() {
    let (scan, world) = cacache();
    for mode in [PlanMode::SafeOnly, PlanMode::Safe] {
        let planned = plan_all(&scan, &Config::new(), &world, mode);
        assert_eq!(
            refusals(&planned),
            [(FindingId(0), Skip::SemanticNotReviewed)]
        );
    }

    let plan = reviewed(&scan, &world).plan;
    assert_eq!(plan.entries().len(), 1, "a person may plan it");
    assert_eq!(
        applied_as(&plan, Authorization::SafeOnly, &scan, &world),
        ItemOutcome::Skipped(Skip::SemanticNotReviewed)
    );
}

/// Catches a plan file carrying a tool's command to a run no person reviewed: `plan -o`
/// plans as [`PlanMode::Safe`], which leaves it out and says so, and the writer has no
/// place for one that reached it (pinned 24).
#[test]
fn plan_file_leaves_native_out() {
    let (native, world) = cacache();
    let plan = reviewed(&native, &world).plan;
    let bytes = encode_plan_file(&plan, &TestCodec).unwrap();
    let file = decode_plan_file(&bytes, &TestCodec).unwrap();
    assert_eq!(file.len(), 0);

    let planned = plan_all(&native, &Config::new(), &world, PlanMode::Safe);
    assert_eq!(
        refusals(&planned),
        [(FindingId(0), Skip::SemanticNotReviewed)],
        "what `plan -o` lists as left out"
    );
}

/// Catches a command deleting what git tracks: a file plan leaves such an entry out, and
/// a command can't, so the whole item holds, at planning and when git starts tracking
/// it between planning and apply (pinned 4).
#[test]
fn tracked_file_in_cacache_holds_native() {
    let (mut scan, world) = cacache();
    scan.findings[0].facts.as_mut().unwrap().tracked = Tracked::Asked {
        work_tree: home(""),
        tracked: 0,
    };
    let path = scan.findings[0].path.clone();
    world.state().git.insert(path.clone(), Ok(Vec::new()));
    let plan = reviewed(&scan, &world).plan;
    assert_eq!(plan.entries().len(), 1, "nothing tracked yet");

    world.state().git.insert(path, Ok(vec![rel("a")]));

    let held = Skip::Protected(vec![Hold::TrackedInsideNative]);
    assert_eq!(
        refusals(&reviewed(&scan, &world)),
        [(FindingId(0), held.clone())]
    );
    assert_eq!(applied(&plan, &scan, &world), ItemOutcome::Skipped(held));
}

/// Catches the worktree exception silencing a repository above the worktree: only its
/// own `.git` stops counting, so an outer repository tracking anything inside holds it
/// (pinned 4).
#[test]
fn outer_repo_tracking_inside_worktree_holds() {
    let (mut scan, world) = worktree(&["a"]);
    scan.findings[0].facts.as_mut().unwrap().tracked = Tracked::Asked {
        work_tree: home("outer"),
        tracked: 0,
    };
    let path = scan.findings[0].path.clone();
    world.state().git.insert(path.clone(), Ok(Vec::new()));
    let plan = reviewed(&scan, &world).plan;
    assert_eq!(plan.entries().len(), 1, "its own `.git` doesn't count");

    world.state().git.insert(path, Ok(vec![rel("a")]));

    let held = Skip::Protected(vec![Hold::TrackedInsideNative]);
    assert_eq!(applied(&plan, &scan, &world), ItemOutcome::Skipped(held));
}

/// Adds a `.git` of `kind` at `path` inside the only target of `world`.
fn add_git(world: &World, scan: &ScanReport, path: &str, kind: EntryKind) {
    let mut state = world.state();
    let target: &mut Target = state.targets.get_mut(&scan.findings[0].path).unwrap();
    target
        .entries
        .insert(rel(path), snapshot(kind, 950, 100, 1));
}

/// Catches the exception reaching past the worktree's own top-level `.git`: a repository
/// deeper inside, a nested worktree's `.git` file or a directory in its place, holds the
/// item at planning and when it appears between planning and apply (pinned 4). The walker
/// reports it to the scan as `nested_repository`, which arrives as
/// [`Tracked::HoldsRepository`]; the frozen tree says the same at both.
#[test]
fn nested_git_inside_a_worktree_item_holds() {
    let nested = Skip::Git(GitGap::NestedRepository);
    let deeper = [
        ("sub/.git", EntryKind::Dir),
        ("sub/.git", EntryKind::File),
        ("sub/deep/.GIT", EntryKind::File),
    ];
    for (path, kind) in deeper {
        let (scan, world) = worktree(&["sub/", "sub/deep/"]);
        let plan = reviewed(&scan, &world).plan;
        assert_eq!(plan.entries().len(), 1, "its own `.git` doesn't count");

        add_git(&world, &scan, path, kind);

        let planned = reviewed(&scan, &world);
        assert_eq!(
            refusals(&planned),
            [(FindingId(0), nested.clone())],
            "{path}"
        );
        assert_eq!(
            applied(&plan, &scan, &world),
            ItemOutcome::Skipped(nested.clone()),
            "{path}"
        );
    }

    let (mut scan, world) = worktree(&["sub/"]);
    scan.findings[0].facts.as_mut().unwrap().tracked = Tracked::HoldsRepository {
        work_tree: scan.findings[0].path.join("sub"),
    };
    assert!(
        matches!(
            refusals(&reviewed(&scan, &world)).as_slice(),
            [(FindingId(0), Skip::Protected(holds))] if holds.contains(&Hold::HoldsRepository)
        ),
        "what the walker found holds it: {:?}",
        refusals(&reviewed(&scan, &world))
    );
}

/// Catches the exception read as "the item's own `.git` isn't there": past a worktree's
/// own file, the item goes on to everything else that may refuse it.
#[test]
fn a_worktrees_own_git_file_is_excused() {
    let (scan, world) = worktree(&[]);
    let plan = reviewed(&scan, &world).plan;
    assert_eq!(
        applied(&plan, &scan, &world),
        ItemOutcome::Skipped(Skip::SemanticNotReviewed),
        "past every hold"
    );
}

/// Catches the exception read as "any `.git` at the top": a linked worktree's is a file,
/// so a directory there is a repository.
#[test]
fn a_git_directory_at_a_worktrees_top_holds() {
    let (scan, world) = worktree(&[]);
    add_git(&world, &scan, ".git", EntryKind::Dir);
    assert_eq!(
        refusals(&reviewed(&scan, &world)),
        [(FindingId(0), Skip::Git(GitGap::NestedRepository))]
    );
}

/// Catches a reviewed command running before the card that runs it (the card's fail
/// closed rule): an item past every hold still changes nothing.
#[test]
fn a_reviewed_native_item_runs_nothing_yet() {
    let (scan, world) = cacache();
    let plan = reviewed(&scan, &world).plan;
    assert_eq!(
        applied(&plan, &scan, &world),
        ItemOutcome::Skipped(Skip::SemanticNotReviewed)
    );
}

/// Catches a command built with no verified tool (pinned 22).
#[test]
fn a_tool_that_cant_be_used_holds_the_item() {
    let (mut scan, world) = cacache();
    let facts = scan.findings[0].facts.as_mut().unwrap();
    facts.supplied.subject = Subject::Semantic(SemanticFacts {
        kind: NativeKind::NpmCacheClean,
        tool: Err(ToolGap::UnverifiedVersion),
    });
    assert_eq!(
        refusals(&reviewed(&scan, &world)),
        [(FindingId(0), Skip::ToolChanged(ToolGap::UnverifiedVersion))]
    );
}

/// Catches a worktree removal built with nothing to compare at apply (pinned 22).
#[test]
fn a_witness_that_couldnt_be_built_holds_the_item() {
    let (mut scan, world) = worktree(&[]);
    let facts = scan.findings[0].facts.as_mut().unwrap();
    let Subject::Worktree(worktree) = &mut facts.supplied.subject else {
        panic!("a worktree");
    };
    worktree.witness = Err(GitGap::Failed);
    assert_eq!(
        refusals(&reviewed(&scan, &world)),
        [(FindingId(0), Skip::Git(GitGap::Failed))]
    );
}

/// Catches npm pointed at a root the item isn't directly below: the command would clean
/// something the review didn't show (pinned 33).
#[test]
fn a_store_not_directly_below_its_root_holds_the_item() {
    let (item, target) = project(0, 100, TARGET);
    let world = world_of(&[(item.clone(), target)]);
    let subject = Subject::Semantic(SemanticFacts {
        kind: NativeKind::NpmCacheClean,
        tool: Ok(tool(NativeTool::Npm)),
    });
    let scan = proposing(scan_of(&[item]), NativeKind::NpmCacheClean, subject);
    assert_eq!(
        refusals(&reviewed(&scan, &world)),
        [(FindingId(0), Skip::Unjudged(Unjudged::Unanchored))]
    );
}

/// Catches a review that didn't cover the command: a different tool is a different plan,
/// so what a person saw is what its digest names (pinned 22).
#[test]
fn the_plan_digest_names_the_tool_a_person_saw() {
    let (mut scan, world) = cacache();
    let seen = reviewed(&scan, &world).plan.digest();
    let facts = scan.findings[0].facts.as_mut().unwrap();
    let mut other = tool(NativeTool::Npm);
    other.version = "1.0.1".to_owned();
    facts.supplied.subject = Subject::Semantic(SemanticFacts {
        kind: NativeKind::NpmCacheClean,
        tool: Ok(other),
    });

    assert_ne!(reviewed(&scan, &world).plan.digest(), seen);
}

/// Catches a native item stored without what a person reviewed, which reconciliation and
/// history read back (pinned 25).
#[test]
fn a_stored_native_item_reads_back_with_its_command() {
    let (scan, world) = worktree(&[]);
    let plan = reviewed(&scan, &world).plan;
    let entry = &plan.entries()[0];
    let Execution::Native(spec) = entry.execution() else {
        panic!("a native item");
    };
    assert!(
        matches!(&spec.command, NativeCommand::GitWorktreeRemove { checkout, .. } if *checkout == spec.scope),
        "git runs in the checkout it removes: {spec:?}"
    );

    let bytes = encode_stored_plan(&plan, &TestCodec).unwrap();
    let end = bytes.iter().position(|byte| *byte == b'\n').unwrap();
    let stored = decode_stored_plan(&bytes[..end], None, &TestCodec).unwrap();

    let item = stored.item(entry.item()).unwrap();
    assert_eq!(item.execution(), entry.execution());
    assert_eq!(item.method(), ItemMethod::Native);
}

/// A release retention let go, naming one lock, and the world holding it.
fn release(locks: Vec<PathBuf>) -> (ScanReport, World) {
    let (item, target) = project(0, 100, TARGET);
    let world = world_of(&[(item.clone(), target)]);
    let mut scan = scan_of(&[item]);
    let facts = scan.findings[0].facts.as_mut().unwrap();
    facts.proposal = Proposal::RemoveRelease(vec![Method::Delete]);
    facts.settlement = Settlement::Candidate;
    facts.supplied.subject = Subject::Release(ReleaseFacts {
        group: ReleaseGroup {
            component: "cli".to_owned(),
            target: "any".to_owned(),
        },
        version: ReleaseVersion("1.0.0".to_owned()),
        role: ReleaseRole::Installed,
        files: Vec::new(),
        locks,
        state: ReleaseState::Sound,
    });
    (scan, world)
}

/// What applying `plan` under `locks` did, reading it again as `rescan` does.
fn applied_under(plan: &Plan, rescan: &Rescanned, locks: &Locks, world: &World) -> ItemOutcome {
    let mutation = MutationPorts {
        locks,
        ..ports(world)
    };
    let ports = ApplyPorts {
        rescan,
        mutation: &mutation,
    };
    let mut journal = world.recorder();
    let to = Recorders {
        journal: &mut journal,
        events: &mut (),
    };
    let request = ApplyRequest {
        plan,
        authorization: Authorization::Reviewed(ReviewedPlan::for_core_tests(plan)),
    };
    let config = Config::new();
    let receipt = apply(&request, &config.judge(), &ports, to, &CancelToken::new());
    receipt.items[0].outcome.clone()
}

/// A log shared by a rescan and locks that answer `held`.
fn logged(scan: &ScanReport, held: Result<(), LockGap>, world: &World) -> (Rescanned, Locks, Log) {
    let log = Log::default();
    world.state().log = Some(Arc::clone(&log));
    let rescan = Rescanned {
        log: Some(Arc::clone(&log)),
        ..rescanned(scan)
    };
    let locks = Locks {
        log: Arc::clone(&log),
        held,
    };
    (rescan, locks, log)
}

/// Catches an item judged from a reading that predates its lock, and a lock let go before
/// the item ran: an install could start between them (pinned 13). The engine's half, that
/// such a reading takes a new snapshot, is in `tests/engine.rs`.
#[test]
fn native_snapshot_is_taken_after_the_locks() {
    let (scan, world) = release(vec![home("install.lock")]);
    let plan = reviewed(&scan, &world).plan;
    assert_eq!(
        plan.entries().len(),
        1,
        "{:?}",
        refusals(&reviewed(&scan, &world))
    );
    let (rescan, locks, log) = logged(&scan, Ok(()), &world);

    let outcome = applied_under(&plan, &rescan, &locks, &world);

    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    assert_eq!(
        *log.lock().unwrap(),
        ["locks", "now", "ran", "released"],
        "taken first, held through the run"
    );
}

/// Catches a release removed beside a running install: a lock held elsewhere, or a store
/// that names none, leaves it alone before anything of it is read again (pinned 13).
#[test]
fn a_lock_that_cant_be_held_leaves_the_release_alone() {
    let (scan, world) = release(vec![home("install.lock")]);
    let plan = reviewed(&scan, &world).plan;
    let (rescan, locks, log) = logged(&scan, Err(LockGap::Held), &world);

    let outcome = applied_under(&plan, &rescan, &locks, &world);

    assert_eq!(
        outcome,
        ItemOutcome::Skipped(Skip::InstallerBusy(LockGap::Held))
    );
    assert_eq!(*log.lock().unwrap(), [] as [&str; 0], "nothing went on");
}

/// Catches a release naming no lock passing as one with nothing to fear.
#[test]
fn a_release_that_names_no_lock_is_left_alone() {
    let (scan, world) = release(Vec::new());
    let plan = reviewed(&scan, &world).plan;
    let (rescan, locks, _) = logged(&scan, Ok(()), &world);

    let outcome = applied_under(&plan, &rescan, &locks, &world);

    assert_eq!(
        outcome,
        ItemOutcome::Skipped(Skip::InstallerBusy(LockGap::Missing))
    );
}

/// Catches an item that can't be read again running on what the apply scan said, and a
/// lock kept past the item that took it.
#[test]
fn an_item_that_cant_be_read_again_is_skipped_and_its_locks_go() {
    let (scan, world) = release(vec![home("install.lock")]);
    let plan = reviewed(&scan, &world).plan;
    let (mut rescan, locks, log) = logged(&scan, Ok(()), &world);
    rescan.refuses = Some(Skip::RevisitMissing);

    let outcome = applied_under(&plan, &rescan, &locks, &world);

    assert_eq!(outcome, ItemOutcome::Skipped(Skip::RevisitMissing));
    assert_eq!(*log.lock().unwrap(), ["locks", "now", "released"]);
}
