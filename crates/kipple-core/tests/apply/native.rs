//! Native items through the planner and the apply loop: who authorizes one, what holds
//! one, how its run is journalled and settled, and what a run of one stores
//! (`docs/04-safety-model.md` §3.2, §6, §7, §8, §8a; spec M4 pinned 3, 4, 22, 24, 28–32).
//!
//! The world's scripted runner plays the tool's command: it removes what the script says,
//! deepest first, ends as the script says, and crashes where a test sets a crash. The
//! platform's own runner is tested on real files in `kipple-platform`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;

use kipple_core::format::encode_stored_plan;
use kipple_core::format::{FormatError, decode_plan_file, decode_stored_plan, encode_plan_file};
use kipple_core::{
    AgentLabel, Authorization, Checkout, Execution, GitGap, Hold, IntegrationId, ItemMethod,
    ItemOutcome, NativeCommand, NativeKind, NativeTool, ObjectId, Plan, PlanMode, Planned,
    Proposal, Registration, RegistrationState, ResolvedTool, ReviewedPlan, ScanReport,
    SemanticFacts, Skip, Subject, ToolGap, Tracked, Unjudged, WorktreeFacts, WorktreeWitness,
};

use super::codec::TestCodec;
use super::world::{Crash, Crashed, Locks, Log, Record, Rescanned, Script, Target, World};
use super::{
    Config, Item, SIZE, TARGET, apply_as, crash_then_reconcile, everything, found, home, id,
    plan_all, ports, project, refusals, rel, rescanned, scan_of, snapshot, stored, world_of,
};
use kipple_core::format::{ApplyEventOut, ReceiptOut};
use kipple_core::{
    Activity, Allocated, Answer, ApplyError, ApplyEvent, ApplyFile, ApplyPorts, ApplyRequest,
    CancelToken, CleanRequest, Completeness, Confirm, Consent, Dropped, Effect, EntryKind,
    ErrorCode, EventSink, FindingId, Found, ItemEvent, ItemId, LockGap, Measured, Method,
    MutationPorts, NativeEnd, NativeOutput, NativeStatus, Printed, Recorders, RelPath,
    ReleaseFacts, ReleaseGroup, ReleaseRole, ReleaseState, ReleaseVersion, Review, Seen, Session,
    Settlement, SpaceEstimate, Status, apply, reconcile, run_apply, run_clean,
};

/// `tool`, at a path absolute on every OS, as a resolver gives it: a stored plan refuses
/// any other, and `/usr/bin` has no drive on Windows.
fn tool(tool: NativeTool) -> ResolvedTool {
    ResolvedTool {
        tool,
        path: home("bin/tool"),
        identity: id(900),
        fingerprint: snapshot(EntryKind::File, 900, 1, 1).fingerprint,
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
        ignored: Ok(Vec::new()),
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

/// A linked worktree whose own `.git` file is at its top, with `extra` beside it, and the
/// world holding it, where git lists it beside its repository's main worktree.
fn worktree(extra: &[&str]) -> (ScanReport, World) {
    let entries: Vec<&str> = std::iter::once(&".git").chain(extra).copied().collect();
    let (mut item, target) = project(0, 100, &entries);
    item.integration = IntegrationId::GIT;
    let world = world_of(&[(item.clone(), target)]);
    let facts = worktree_facts(&item);
    let listed = vec![
        Registration {
            path: home("main"),
            ..facts.registration.clone()
        },
        facts.registration.clone(),
    ];
    world.state().worktrees.insert(home("main"), Ok(listed));
    let subject = Subject::Worktree(Box::new(facts));
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

/// How applying `plan` as `shown` ended its only item, which it skipped: nothing ran.
fn applied_as(plan: &Plan, shown: Authorization, scan: &ScanReport, world: &World) -> ItemOutcome {
    let receipt = apply_as(plan, shown, &rescanned(scan), &Config::new(), world);
    assert_eq!(world.state().ran, Vec::new(), "nothing ran");
    receipt.items[0].outcome.clone()
}

/// How running `plan`, reviewed, ended its only item, rescanning as `rescan`.
fn ran(plan: &Plan, rescan: &Rescanned, world: &World) -> ItemOutcome {
    let shown = Authorization::Reviewed(ReviewedPlan::for_core_tests(plan));
    let receipt = apply_as(plan, shown, rescan, &Config::new(), world);
    receipt.items[0].outcome.clone()
}

/// How running `plan`, reviewed, ended its only item, with the world's command playing
/// `script`.
fn scripted(plan: &Plan, scan: &ScanReport, world: &World, script: Script) -> ItemOutcome {
    world.state().script = script;
    ran(plan, &rescanned(scan), world)
}

/// A command's end by itself, exiting with `code`.
const fn exited(code: i32) -> NativeStatus {
    NativeStatus::Ended(NativeEnd {
        exit: Some(code),
        signal: None,
    })
}

/// The error a native item's outcome carries, if any.
const fn error_of(outcome: &ItemOutcome) -> Option<ErrorCode> {
    match outcome {
        ItemOutcome::Failed { error, .. }
        | ItemOutcome::Indeterminate(Found::Unproven(error) | Found::Unsynced(error)) => {
            Some(error.code)
        }
        ItemOutcome::Done(_)
        | ItemOutcome::Partial(_)
        | ItemOutcome::Skipped(_)
        | ItemOutcome::Cancelled(_)
        | ItemOutcome::Indeterminate(_) => None,
    }
}

/// Catches `--yes` or `--only X --yes` reaching a tool's command: at planning and again
/// at apply, whatever else the item passes (pinned 24).
#[test]
fn yes_refuses_native_at_plan_and_apply() {
    let (scan, world) = cacache();
    let planned = plan_all(&scan, &Config::new(), &world, PlanMode::SafeOnly);
    assert_eq!(
        refusals(&planned),
        [(FindingId(0), Skip::SemanticNotReviewed)]
    );

    let plan = reviewed(&scan, &world).plan;
    assert_eq!(plan.entries().len(), 1, "a person may plan it");
    assert_eq!(
        applied_as(&plan, Authorization::SafeOnly, &scan, &world),
        ItemOutcome::Skipped(Skip::SemanticNotReviewed)
    );
}

/// Catches a plan file carrying a tool's command to a run no person reviewed, or losing
/// one unsaid: `plan -o` plans as [`PlanMode::SafeFiles`], which leaves it out and says
/// so, and the writer refuses one that reached it (pinned 24).
#[test]
fn plan_file_leaves_native_out() {
    let (native, world) = cacache();
    let planned = plan_all(&native, &Config::new(), &world, PlanMode::SafeFiles);
    assert_eq!(
        refusals(&planned),
        [(FindingId(0), Skip::SemanticNotReviewed)],
        "what `plan -o` lists as left out"
    );

    let plan = reviewed(&native, &world).plan;
    let written = encode_plan_file(&plan, &TestCodec);
    assert!(
        matches!(written, Err(FormatError::NativeItem(_))),
        "never dropped unsaid: {written:?}"
    );
}

/// Answers yes at the prompt, keeping how many items it was shown.
#[derive(Default)]
struct Approves(usize);

impl Confirm for Approves {
    fn confirm(&mut self, review: &Review<'_>) -> Answer {
        self.0 = review.plan.entries().len();
        Answer::Yes
    }
}

/// Catches the prompt never reaching a tool's command, which leaves a person no way to
/// review one at the command line, and a yes there not running it (pinned 23, 24).
#[test]
fn a_person_at_the_prompt_sees_a_native_item_and_a_yes_runs_it() {
    let (scan, world) = cacache();
    let config = Config::new();
    let mutation = ports(&world);
    let rescan = rescanned(&scan);
    let session = Session {
        judge: config.judge(),
        store: &world,
        ports: ApplyPorts {
            rescan: &rescan,
            mutation: &mutation,
        },
        codec: &TestCodec,
    };
    let selection = everything(&scan);
    let request = CleanRequest {
        scan: &scan,
        selection: &selection,
    };
    let mut person = Approves::default();

    let finished = run_clean(
        &request,
        &session,
        Consent::Ask(&mut person),
        &mut (),
        &CancelToken::new(),
    );

    assert_eq!(person.0, 1, "the prompt showed it");
    assert_eq!(finished.receipt.items[0].outcome.status(), Status::Done);
    assert_eq!(world.state().ran, [ItemId(0)]);
}

/// Catches a plan file reaching a tool's command through a person's prompt: the file named
/// files, its rule now runs a command, and the item drops before anyone is asked
/// (pinned 24).
#[test]
fn a_file_item_whose_rule_now_runs_a_command_drops() {
    let (mut scan, world) = cacache();
    let native = scan.findings[0].facts.clone();
    let facts = scan.findings[0].facts.as_mut().unwrap();
    facts.proposal = Proposal::Remove(vec![Method::Delete]);
    facts.supplied.subject = Subject::Plain;
    let planned = plan_all(&scan, &Config::new(), &world, PlanMode::SafeFiles);
    let file = encode_plan_file(&planned.plan, &TestCodec).unwrap();
    let imported = decode_plan_file(&file, &TestCodec).unwrap();
    scan.findings[0].facts = native;
    let config = Config::new();
    let mutation = ports(&world);
    let rescan = rescanned(&scan);
    let session = Session {
        judge: config.judge(),
        store: &world,
        ports: ApplyPorts {
            rescan: &rescan,
            mutation: &mutation,
        },
        codec: &TestCodec,
    };
    let applying = ApplyFile {
        imported: &imported,
        scan: &scan,
    };
    let mut person = Approves::default();

    let finished = run_apply(
        &applying,
        &session,
        Consent::Ask(&mut person),
        &mut (),
        &CancelToken::new(),
    );

    assert_eq!(person.0, 0, "no one is asked about it");
    let why: Vec<&Dropped> = finished.dropped.iter().map(|item| &item.why).collect();
    assert_eq!(
        why,
        [&Dropped::Refused(Skip::MethodNotOffered(Method::Delete))],
        "the file's method for that rule is a method named, which a command gives way to"
    );
    assert_eq!(world.state().ran, Vec::new(), "nothing ran");
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
/// own file, the item goes on to everything else that may refuse it, and runs.
#[test]
fn a_worktrees_own_git_file_is_excused() {
    let (scan, world) = worktree(&[]);
    let plan = reviewed(&scan, &world).plan;
    let outcome = ran(&plan, &rescanned(&scan), &world);
    assert_eq!(
        outcome.status(),
        Status::Done,
        "past every hold: {outcome:?}"
    );
    let listed = world.state().worktrees[&home("main")].clone().unwrap();
    assert_eq!(listed.len(), 1, "git no longer lists it");
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

/// Catches a review that didn't cover the command: another tool, another file of it, or
/// a worktree tied to its repository another way is a different plan, so what a person
/// saw is what its digest names (pinned 22).
#[test]
fn digest_covers_command_tool_and_witness() {
    let cleaning_with = |found: ResolvedTool| {
        let (mut scan, world) = cacache();
        scan.findings[0].facts.as_mut().unwrap().supplied.subject =
            Subject::Semantic(SemanticFacts {
                kind: NativeKind::NpmCacheClean,
                tool: Ok(found),
            });
        reviewed(&scan, &world).plan.digest()
    };
    let seen = cleaning_with(tool(NativeTool::Npm));
    let mut newer = tool(NativeTool::Npm);
    newer.version = "1.0.1".to_owned();
    assert_ne!(cleaning_with(newer), seen, "a version");
    let mut other = tool(NativeTool::Npm);
    other.identity = id(999);
    assert_ne!(cleaning_with(other), seen, "a file");

    let removing_with = |witnessed: WorktreeWitness| {
        let (mut scan, world) = worktree(&[]);
        let facts = scan.findings[0].facts.as_mut().unwrap();
        if let Subject::Worktree(worktree) = &mut facts.supplied.subject {
            worktree.witness = Ok(witnessed);
        }
        reviewed(&scan, &world).plan.digest()
    };
    let mut moved = witness();
    moved.admin = id(990);
    assert_ne!(removing_with(moved), removing_with(witness()), "a witness");
}

/// Catches a native item stored without what a person reviewed, which reconciliation and
/// history read back (pinned 25).
#[test]
fn a_stored_native_item_reads_back_with_its_command() {
    let (scan, world) = worktree(&[]);
    let plan = reviewed(&scan, &world).plan;
    let Execution::Native(spec) = plan.entries()[0].execution() else {
        panic!("a native item");
    };
    assert!(
        matches!(&spec.command, NativeCommand::GitWorktreeRemove { checkout, .. } if *checkout == spec.scope),
        "git runs in the checkout it removes: {spec:?}"
    );
    reads_back(&plan);

    // npm's command starts in its cache root, whose anchor has nothing below the root.
    let (store, npm) = cacache();
    let cleaned = reviewed(&store, &npm).plan;
    let Execution::Native(clean) = cleaned.entries()[0].execution() else {
        panic!("a native item");
    };
    assert!(
        matches!(&clean.command, NativeCommand::NpmCacheClean { cache } if cache.relative == RelPath::default()),
        "the root itself: {clean:?}"
    );
    reads_back(&cleaned);
}

/// Asserts that `plan`'s only item reads back from its stored form as it was.
fn reads_back(plan: &Plan) {
    let entry = &plan.entries()[0];
    let bytes = encode_stored_plan(plan, &TestCodec).unwrap();
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
        ["locks", "freeze", "now", "ran", "released"],
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
    assert_eq!(*log.lock().unwrap(), ["locks", "freeze", "now", "released"]);
}

/// Catches a command started before its intent is durable, so a crash leaves a change the
/// journal never heard of: the intent lands first, and one that can't land starts nothing
/// (pinned 27, 31).
#[test]
fn native_runs_only_after_durable_intent() {
    let (scan, world) = cacache();
    let plan = reviewed(&scan, &world).plan;

    let outcome = ran(&plan, &rescanned(&scan), &world);

    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    let state = world.state();
    assert_eq!(state.ran, [ItemId(0)]);
    assert_eq!(state.unjournalled, Vec::new(), "its intent was there first");
    assert_eq!(state.order_problem(), None);
    assert!(
        matches!(&state.records[1], Record::Intent(intent) if intent.method == ItemMethod::Native),
        "the intent says a command runs: {:?}",
        state.records[1]
    );
}

/// Catches a command started when its intent couldn't be recorded: nothing would say it
/// ran.
#[test]
fn an_intent_that_cant_be_recorded_starts_nothing() {
    let (scan, world) = cacache();
    let plan = reviewed(&scan, &world).plan;
    world.state().journal_fails_at = Some(1);

    let refused = ran(&plan, &rescanned(&scan), &world);

    assert_eq!(
        refused,
        ItemOutcome::Skipped(Skip::NotStarted(kipple_core::Stop::JournalFailed))
    );
    assert_eq!(world.state().ran, Vec::new(), "nothing started");
}

/// Catches an intent read as "the command ran" or "it didn't", and a command started
/// again for it: a crash between the intent and the spawn, during the run, or before the
/// outcome reconciles as indeterminate with what is there, and nothing runs (pinned 31).
#[test]
fn crash_after_intent_reconciles_and_never_reruns() {
    let cases = [
        (Crash::AfterWrite(1), &[][..], Found::Unchanged),
        (
            Crash::AfterRemovals(2),
            &[ItemId(0)][..],
            Found::PartlyGone { gone: 2, of: 7 },
        ),
        (Crash::BeforeWrite(2), &[ItemId(0)][..], Found::Gone),
    ];
    for (crash, started, expected) in cases {
        let (scan, world) = cacache();
        let plan = reviewed(&scan, &world).plan;
        let config = Config::new();

        let reconciled = crash_then_reconcile(&plan, &scan, &config, &world, crash);

        assert_eq!(found(&reconciled), [Some(expected)], "{crash:?}");
        assert_eq!(
            world.state().ran,
            started,
            "{crash:?}: started once at most"
        );
    }
}

/// Catches a journal that fails after the command ran read as nothing having happened:
/// the run stops with the intent pending, and reconciliation settles it without running
/// the command again.
#[test]
fn a_journal_failing_after_the_run_leaves_it_to_reconciliation() {
    let (scan, world) = cacache();
    let plan = reviewed(&scan, &world).plan;
    world.state().journal_fails_at = Some(2);
    world.state().journal_recovers = true;

    ran(&plan, &rescanned(&scan), &world);

    assert_eq!(world.state().pending().len(), 1, "its outcome never landed");
    let run = stored(&world, &plan);
    let mut journal = world.recorder();
    let reconciled = reconcile(&run, &ports(&world), &mut journal, &CancelToken::new()).unwrap();
    assert_eq!(found(&reconciled), [Some(Found::Gone)]);
    assert_eq!(world.state().ran, [ItemId(0)], "run once");
}

/// Catches a command that missed its deadline read by what it removed: kipple killed its
/// own child, whose own children may still be at work, so the item is indeterminate even
/// when its target is gone (pinned 28). The kill itself is the platform's
/// `deadline_kills_own_child_and_is_indeterminate`.
#[test]
fn a_missed_deadline_is_indeterminate_whatever_it_removed() {
    let killed = NativeStatus::TimedOut(NativeEnd {
        exit: None,
        signal: Some(9),
    });
    for removes in [Some(0), Some(2), None] {
        let (scan, world) = cacache();
        let plan = reviewed(&scan, &world).plan;
        let script = Script {
            status: killed,
            removes,
            ..Script::default()
        };

        let outcome = scripted(&plan, &scan, &world, script);

        assert_eq!(outcome.status(), Status::Indeterminate, "{removes:?}");
        assert_eq!(error_of(&outcome), Some(ErrorCode::NativeTimedOut));
    }
}

/// Catches a removal that can't be made durable recorded as done: a failed sync is
/// indeterminate though the command exited 0 and its target is gone (pinned 30).
#[test]
fn done_waits_for_the_sync_barrier() {
    let (scan, world) = cacache();
    let plan = reviewed(&scan, &world).plan;
    let script = Script {
        synced: Err(ApplyError::new(ErrorCode::Io)),
        ..Script::default()
    };

    let outcome = scripted(&plan, &scan, &world, script);

    assert_eq!(
        outcome,
        ItemOutcome::Indeterminate(Found::Unsynced(ApplyError::new(ErrorCode::Io)))
    );
}

/// Catches a worktree recorded removed while git can't say it is: a `list` that fails
/// after the run leaves the item indeterminate (pinned 32).
#[test]
fn failed_list_after_the_run_is_indeterminate() {
    let (scan, world) = worktree(&[]);
    let plan = reviewed(&scan, &world).plan;
    world
        .state()
        .worktrees
        .insert(home("main"), Err(GitGap::Failed));

    let outcome = ran(&plan, &rescanned(&scan), &world);

    assert_eq!(outcome.status(), Status::Indeterminate, "{outcome:?}");
    assert_eq!(error_of(&outcome), Some(ErrorCode::NativeUnconfirmed));
}

/// Catches a worktree recorded removed while git still lists it, its directory gone: git
/// may have kept its registration, so the item is indeterminate (pinned 32).
#[test]
fn a_worktree_git_still_lists_after_the_run_is_indeterminate() {
    let (scan, world) = worktree(&[]);
    let plan = reviewed(&scan, &world).plan;
    let listing = ListsAgain(world.state().worktrees[&home("main")].clone());
    let mutation = MutationPorts {
        worktrees: &listing,
        ..ports(&world)
    };
    let shown = Authorization::Reviewed(ReviewedPlan::for_core_tests(&plan));
    let receipt = apply_through(&plan, shown, &rescanned(&scan), &mutation, &world);
    assert_eq!(
        receipt.status(),
        Status::Indeterminate,
        "still named: {receipt:?}"
    );
}

/// A git that lists what it was given, whatever the world's command removed.
#[derive(Debug)]
struct ListsAgain(Result<Vec<Registration>, GitGap>);

impl kipple_core::WorktreeQuery for ListsAgain {
    fn list(&self, _: &std::path::Path, _: &CancelToken) -> Result<Vec<Registration>, GitGap> {
        self.0.clone()
    }

    fn inspect(&self, _: &Registration, _: &CancelToken) -> kipple_core::WorktreeState {
        kipple_core::WorktreeState::unknown(GitGap::Unsupported)
    }
}

/// How applying `plan` as `shown` through `mutation` ended its only item.
fn apply_through(
    plan: &Plan,
    shown: Authorization,
    rescan: &Rescanned,
    mutation: &MutationPorts<'_>,
    world: &World,
) -> ItemOutcome {
    let ports = ApplyPorts { rescan, mutation };
    let mut journal = world.recorder();
    let to = Recorders {
        journal: &mut journal,
        events: &mut (),
    };
    let request = ApplyRequest {
        plan,
        authorization: shown,
    };
    let config = Config::new();
    let receipt = apply(&request, &config.judge(), &ports, to, &CancelToken::new());
    receipt.items[0].outcome.clone()
}

/// Catches a command that never started read as anything but failed with no effect: the
/// runner's final check refused it, and nothing ran (pinned 29, 32).
#[test]
fn a_command_that_never_started_failed() {
    let (scan, world) = cacache();
    let plan = reviewed(&scan, &world).plan;
    let refused = Script {
        refusal: Some(ErrorCode::ToolChanged),
        ..Script::default()
    };
    assert_eq!(
        scripted(&plan, &scan, &world, refused),
        ItemOutcome::Failed {
            error: ApplyError::new(ErrorCode::ToolChanged),
            effect: None
        },
        "never started"
    );
    assert_eq!(world.state().ran, Vec::new());
}

/// Catches a command kipple lost sight of after it started read as failed or done: its
/// wait or its output couldn't be read, so nobody saw how it ended, and it is
/// indeterminate whether it changed nothing, everything, or couldn't even be stopped.
#[test]
fn a_command_lost_after_it_started_is_indeterminate() {
    let killed = NativeEnd {
        exit: None,
        signal: Some(9),
    };
    let cases = [
        (Some(killed), Some(0), ErrorCode::NativeLost),
        (Some(killed), None, ErrorCode::NativeLost),
        (None, Some(0), ErrorCode::NativeLostRunning),
    ];
    for (stopped, removes, code) in cases {
        let (scan, world) = cacache();
        let plan = reviewed(&scan, &world).plan;
        let lost = Script {
            removes,
            status: NativeStatus::Lost { stopped },
            ..Script::default()
        };

        let outcome = scripted(&plan, &scan, &world, lost);

        assert_eq!(
            outcome.status(),
            Status::Indeterminate,
            "{code:?}: {outcome:?}"
        );
        assert_eq!(error_of(&outcome), Some(code), "{outcome:?}");
    }
}

/// Catches a command that changed nothing read as anything but failed, and one that
/// ended badly or by a signal after removing something read as failed, which would let
/// the next run treat what is left as untouched (pinned 32).
#[test]
fn a_command_that_changed_nothing_failed_and_one_that_did_is_indeterminate() {
    let signalled = NativeStatus::Ended(NativeEnd {
        exit: None,
        signal: Some(15),
    });
    let cases = [
        (exited(1), Some(0), Status::Failed, ErrorCode::NativeFailed),
        (signalled, Some(0), Status::Failed, ErrorCode::NativeFailed),
        (
            exited(0),
            Some(0),
            Status::Failed,
            ErrorCode::NativeUnconfirmed,
        ),
        (
            exited(1),
            Some(3),
            Status::Indeterminate,
            ErrorCode::NativeFailed,
        ),
        (
            signalled,
            Some(3),
            Status::Indeterminate,
            ErrorCode::NativeFailed,
        ),
        (
            exited(243),
            None,
            Status::Indeterminate,
            ErrorCode::NativeFailed,
        ),
    ];
    for (status, removes, expected, code) in cases {
        let (scan, world) = cacache();
        let plan = reviewed(&scan, &world).plan;
        let script = Script {
            removes,
            status,
            ..Script::default()
        };

        let outcome = scripted(&plan, &scan, &world, script);

        assert_eq!(
            outcome.status(),
            expected,
            "{status:?} {removes:?}: {outcome:?}"
        );
        assert_eq!(error_of(&outcome), Some(code), "{status:?} {removes:?}");
        assert_eq!(world.state().ran, [ItemId(0)]);
    }
}

/// Catches a worktree git refused to remove read as anything but failed while git still
/// lists it, unchanged.
#[test]
fn a_worktree_git_refused_to_remove_failed() {
    let (scan, world) = worktree(&[]);
    let plan = reviewed(&scan, &world).plan;
    let failed = Script {
        status: exited(128),
        removes: Some(0),
        ..Script::default()
    };
    assert_eq!(
        scripted(&plan, &scan, &world, failed).status(),
        Status::Failed,
        "a worktree git still lists, unchanged"
    );
}

/// Catches a receipt that reports bytes it never measured: freed space is what the target
/// measured in full before the re-check against after the run, and "not measured" when
/// either fell short (pinned 32).
#[test]
fn receipt_measures_or_says_not_measured() {
    let partial = SpaceEstimate {
        completeness: Completeness::Incomplete {
            unreadable: 1,
            skipped_mounts: 0,
        },
        ..SIZE
    };
    let cases = [
        (
            Some(SIZE),
            None,
            Measured::Bytes {
                before: Allocated(8192),
                after: Allocated(0),
            },
        ),
        (
            Some(SIZE),
            Some(0),
            Measured::Bytes {
                before: Allocated(8192),
                after: Allocated(8192),
            },
        ),
        (Some(partial), None, Measured::Unknown),
        (None, None, Measured::Unknown),
    ];
    for (size, removes, expected) in cases {
        let (scan, world) = cacache();
        let plan = reviewed(&scan, &world).plan;
        world.state().script = Script {
            removes,
            status: if removes.is_some() {
                exited(1)
            } else {
                exited(0)
            },
            ..Script::default()
        };
        let rescan = Rescanned {
            size,
            ..rescanned(&scan)
        };
        let shown = Authorization::Reviewed(ReviewedPlan::for_core_tests(&plan));

        let receipt = apply_as(&plan, shown, &rescan, &Config::new(), &world);

        let effect = match &receipt.items[0].outcome {
            ItemOutcome::Done(Effect::Native(effect))
            | ItemOutcome::Failed {
                effect: Some(Effect::Native(effect)),
                ..
            } => *effect,
            other @ (ItemOutcome::Done(_)
            | ItemOutcome::Failed { .. }
            | ItemOutcome::Partial(_)
            | ItemOutcome::Skipped(_)
            | ItemOutcome::Cancelled(_)
            | ItemOutcome::Indeterminate(_)) => panic!("a native effect: {other:?}"),
        };
        assert_eq!(effect.reclaimed, expected, "{size:?} {removes:?}");
        let total = receipt.totals().native;
        assert_eq!(total.items, 1);
        assert_eq!(
            total.unmeasured,
            u64::from(expected == Measured::Unknown),
            "{size:?}"
        );
    }
}

/// Catches the measurement walk sitting between the re-check and the spawn, where the
/// target could change unseen: the target is measured first, then frozen and judged
/// again, and the command starts last (pinned 29). git is asked after the run.
#[test]
fn measure_runs_before_the_recheck() {
    for (scan, world, order) in [
        {
            let (scan, world) = cacache();
            (scan, world, &["size", "freeze", "now", "ran"][..])
        },
        {
            let (scan, world) = worktree(&[]);
            (scan, world, &["size", "freeze", "now", "ran", "list"][..])
        },
    ] {
        let plan = reviewed(&scan, &world).plan;
        let log = Log::default();
        world.state().log = Some(Arc::clone(&log));
        let rescan = Rescanned {
            log: Some(Arc::clone(&log)),
            size: Some(SIZE),
            ..rescanned(&scan)
        };

        let outcome = ran(&plan, &rescan, &world);

        assert_eq!(outcome.status(), Status::Done);
        assert_eq!(*log.lock().unwrap(), order);
    }
}

/// Collects every event of a run.
#[derive(Default)]
struct Heard(Vec<ApplyEvent>);

impl EventSink for Heard {
    fn event(&mut self, event: ApplyEvent) {
        self.0.push(event);
    }
}

/// Catches what a tool printed reaching anything kept or shown as data: a secret in its
/// stderr is in the item's end, for the frontend to show once, and nowhere in the
/// journal, the stored plan, the receipt or any event's JSON (pinned 32, 04 §8a).
#[test]
fn stderr_never_persisted() {
    const SECRET: &str = "npm_token=s3cr3t";
    let (scan, world) = cacache();
    world.state().script = Script {
        status: exited(1),
        removes: Some(0),
        output: NativeOutput {
            stdout: Printed::all(format!("stdout {SECRET}").into_bytes()),
            stderr: Printed::all(format!("stderr {SECRET}").into_bytes()),
        },
        ..Script::default()
    };
    let config = Config::new();
    let mutation = ports(&world);
    let rescan = rescanned(&scan);
    let session = Session {
        judge: config.judge(),
        store: &world,
        ports: ApplyPorts {
            rescan: &rescan,
            mutation: &mutation,
        },
        codec: &TestCodec,
    };
    let selection = everything(&scan);
    let request = CleanRequest {
        scan: &scan,
        selection: &selection,
    };
    let mut heard = Heard::default();

    let finished = run_clean(
        &request,
        &session,
        Consent::Ask(&mut Approves::default()),
        &mut heard,
        &CancelToken::new(),
    );

    let shown = heard.0.iter().find_map(|event| {
        if let ApplyEvent::Item {
            event:
                ItemEvent::Ended {
                    output: Some(output),
                    ..
                },
            ..
        } = event
        {
            Some(output.stderr.bytes.clone())
        } else {
            None
        }
    });
    assert_eq!(shown, Some(format!("stderr {SECRET}").into_bytes()));
    let mut kept: Vec<Vec<u8>> = Vec::new();
    for files in world.state().runs.values() {
        kept.push(files.journal.clone());
        kept.extend(files.plan.clone());
    }
    kept.push(serde_json::to_vec(&ReceiptOut::of(&finished, &TestCodec)).unwrap());
    for (seq, event) in (0..).zip(&heard.0) {
        kept.push(serde_json::to_vec(&ApplyEventOut::of(event, seq, &TestCodec)).unwrap());
    }
    let leaked = kept.iter().any(|bytes| {
        bytes
            .windows(SECRET.len())
            .any(|window| window == SECRET.as_bytes())
    });
    assert!(
        !leaked,
        "no record, receipt or event holds what the tool printed"
    );
}

/// A plan of `scan`'s worktree item, then a project's `target/` after it.
fn worktree_then_files() -> (ScanReport, World, Plan) {
    let (mut scan, world) = worktree(&[]);
    let (item, target) = project(1, 200, TARGET);
    world.state().targets.insert(item.path.clone(), target);
    let mut files = scan_of(std::slice::from_ref(&item));
    files.findings[0].id = FindingId(1);
    scan.findings.append(&mut files.findings);
    scan.integrations.append(&mut files.integrations);
    let plan = reviewed(&scan, &world).plan;
    assert_eq!(
        plan.entries().len(),
        2,
        "{:?}",
        reviewed(&scan, &world).refused
    );
    (scan, world, plan)
}

/// Catches a first Ctrl+C abandoning a command mid-run or its evidence: the run waits for
/// the command, asks git with its own token, and then stops (pinned 28).
#[test]
fn a_first_interrupt_waits_for_the_command_and_its_evidence() {
    let (scan, world, plan) = worktree_then_files();
    let cancel = CancelToken::new();
    world.state().script = Script {
        interrupts: Some(cancel.clone()),
        ..Script::default()
    };
    let rescan = rescanned(&scan);
    let mutation = ports(&world);
    let through = ApplyPorts {
        rescan: &rescan,
        mutation: &mutation,
    };
    let mut journal = world.recorder();
    let to = Recorders {
        journal: &mut journal,
        events: &mut (),
    };
    let request = ApplyRequest {
        plan: &plan,
        authorization: Authorization::Reviewed(ReviewedPlan::for_core_tests(&plan)),
    };
    let receipt = apply(&request, &Config::new().judge(), &through, to, &cancel);
    assert_eq!(receipt.items[0].outcome.status(), Status::Done, "it waited");
    assert_eq!(
        receipt.items[1].outcome.status(),
        Status::Skipped,
        "then stopped"
    );
    assert_eq!(world.state().ran, [ItemId(0)]);
}

/// Catches a second Ctrl+C, which ends kipple while the child runs on, leaving an item
/// read as anything but indeterminate, or one run again (pinned 28).
#[test]
fn second_interrupt_leaves_the_item_indeterminate() {
    let (scan, world, plan) = worktree_then_files();
    world.state().crash = Some(Crash::AfterRemovals(1));
    let killed = catch_unwind(AssertUnwindSafe(|| ran(&plan, &rescanned(&scan), &world)));
    assert!(
        matches!(&killed, Err(payload) if payload.is::<Crashed>()),
        "kipple ended mid-run"
    );
    world.state().crash = None;
    let run = stored(&world, &plan);
    let mut journal = world.recorder();
    let reconciled = reconcile(&run, &ports(&world), &mut journal, &CancelToken::new()).unwrap();
    assert!(
        matches!(
            found(&reconciled).as_slice(),
            [Some(Found::PartlyGone { .. })]
        ),
        "what the child left: {reconciled:?}"
    );
    assert_eq!(world.state().ran, [ItemId(0)], "never run again");
}

/// Catches a native item whose target is in use after kipple ended mid-run planned or
/// run again: activity in the target, which is what a child kipple left running shows
/// (the platform's `a_running_child_shows_its_checked_directory_to_the_process_probe`),
/// holds the item at planning and at apply, and the command started once (pinned 28).
#[test]
fn a_native_item_in_use_after_a_crash_is_held_at_plan_and_apply() {
    let (mut scan, world) = worktree(&["a", "b"]);
    let plan = reviewed(&scan, &world).plan;
    world.state().crash = Some(Crash::AfterRemovals(1));
    let killed = catch_unwind(AssertUnwindSafe(|| ran(&plan, &rescanned(&scan), &world)));
    assert!(killed.is_err(), "kipple ended mid-run");
    world.state().crash = None;

    let path = scan.findings[0].path.clone();
    let busy = Activity::InUse {
        pid: 4242,
        seen: Seen::WorkingDirectory(path),
    };
    scan.findings[0].facts.as_mut().unwrap().activity = busy.clone();

    let held = refusals(&reviewed(&scan, &world));
    assert!(
        matches!(held.as_slice(), [(_, Skip::Protected(holds))] if holds.contains(&Hold::InUse)),
        "the next plan holds it: {held:?}"
    );
    let rescan = Rescanned {
        activity: Some(busy),
        ..rescanned(&scan)
    };
    let outcome = ran(&plan, &rescan, &world);
    assert!(
        matches!(&outcome, ItemOutcome::Skipped(Skip::Protected(holds)) if holds.contains(&Hold::InUse)),
        "and the old plan's run skips it: {outcome:?}"
    );
    assert_eq!(world.state().ran, [ItemId(0)], "started once");
}

/// Catches a command run by a tool other than the one a person saw, at the scan apply
/// runs: another file, another version, or none, skips before its intent (pinned 29).
/// A swap after that is the runner's, which fails to start.
#[test]
fn a_tool_found_again_as_another_skips_before_its_intent() {
    let (mut scan, world) = cacache();
    let plan = reviewed(&scan, &world).plan;
    let mut swapped = tool(NativeTool::Npm);
    swapped.identity = id(777);
    for (now, skip) in [
        (Ok(swapped), Skip::ToolChanged(ToolGap::Changed)),
        (Err(ToolGap::Missing), Skip::ToolChanged(ToolGap::Missing)),
    ] {
        scan.findings[0].facts.as_mut().unwrap().supplied.subject =
            Subject::Semantic(SemanticFacts {
                kind: NativeKind::NpmCacheClean,
                tool: now,
            });
        assert_eq!(applied(&plan, &scan, &world), ItemOutcome::Skipped(skip));
        let intended = world
            .state()
            .records
            .iter()
            .any(|record| matches!(record, Record::Intent(_)));
        assert!(!intended, "skipped before its intent");
    }
}
