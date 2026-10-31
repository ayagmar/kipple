//! Mutating commands end to end through `run_clean`, `run_restore` and their events
//! (spec M3b pinned 1, 3, 4, 10, 12, 17; D-067).

use kipple_core::format::{decode_plan_file, encode_plan_file};
use kipple_core::{
    Activity, ActivityGap, Answer, ApplyEvent, ApplyPorts, CancelToken, CleanRequest, Completeness,
    Confirm, Consent, Depth, Digest, Ending, EventSink, Finished, GitGap, Hold, ItemEvent, ItemId,
    ItemOutcome, Method, Reconciled, RestoreAsk, RestoreResult, Restoring, Review, RunRead,
    ScanReport, Session, Skip, Status, StoreRefusal, Total, Tracked, read_run, run_clean,
    run_restore,
};
use kipple_core::{ApplyFile, Dropped, ImportedPlan, PlanMode, run_apply};

use super::codec::TestCodec;
use super::world::{Crash, Rescanned, World};
use super::{
    Config, everything, home, plan_all, ports, project, rel, rescanned, scan_of, world_of,
};

/// Answers every prompt with its answer, keeping the digest of each plan shown.
struct Person {
    answer: Answer,
    shown: Vec<Digest>,
}

impl Confirm for Person {
    fn confirm(&mut self, review: &Review<'_>) -> Answer {
        self.shown.push(review.plan.digest());
        self.answer
    }
}

const fn person(answer: Answer) -> Person {
    Person {
        answer,
        shown: Vec::new(),
    }
}

/// Every event, in order.
#[derive(Debug, Default)]
struct Events(Vec<ApplyEvent>);

impl EventSink for Events {
    fn event(&mut self, event: ApplyEvent) {
        self.0.push(event);
    }
}

/// What one command runs against.
struct Machine {
    world: World,
    config: Config,
    scan: ScanReport,
    rescan: Rescanned,
}

impl Machine {
    /// Two projects' `target/`, Safe, deleted unless `method` says otherwise.
    fn new(method: Method) -> Self {
        let projects = [
            project(0, 100, &["a", "debug/", "debug/b"]),
            project(1, 200, &["c"]),
        ];
        let world = world_of(&projects);
        let items: Vec<_> = projects.into_iter().map(|(item, _)| item).collect();
        let scan = scan_of(&items);
        let mut config = Config::new();
        config.methods.config = Some(method);
        Self {
            world,
            config,
            rescan: rescanned(&scan),
            scan,
        }
    }

    fn clean(
        &self,
        consent: Consent<'_>,
        events: &mut dyn EventSink,
        cancel: &CancelToken,
    ) -> Finished {
        let mutation = ports(&self.world);
        let session = Session {
            judge: self.config.judge(),
            store: &self.world,
            ports: ApplyPorts {
                rescan: &self.rescan,
                mutation: &mutation,
            },
            codec: &TestCodec,
        };
        let selection = everything(&self.scan);
        let request = CleanRequest {
            scan: &self.scan,
            selection: &selection,
        };
        run_clean(&request, &session, consent, events, cancel)
    }

    fn restore(&self, ask: &RestoreAsk, events: &mut Events) -> kipple_core::RestoreFinished {
        let mutation = ports(&self.world);
        let session = Session {
            judge: self.config.judge(),
            store: &self.world,
            ports: ApplyPorts {
                rescan: &self.rescan,
                mutation: &mutation,
            },
            codec: &TestCodec,
        };
        run_restore(ask, &session, events, &CancelToken::new())
    }

    fn apply(&self, imported: &ImportedPlan, consent: Consent<'_>) -> Finished {
        let mutation = ports(&self.world);
        let session = Session {
            judge: self.config.judge(),
            store: &self.world,
            ports: ApplyPorts {
                rescan: &self.rescan,
                mutation: &mutation,
            },
            codec: &TestCodec,
        };
        let file = ApplyFile {
            imported,
            scan: &self.scan,
        };
        run_apply(
            &file,
            &session,
            consent,
            &mut Events::default(),
            &CancelToken::new(),
        )
    }

    fn statuses(finished: &Finished) -> Vec<Status> {
        finished
            .receipt
            .items
            .iter()
            .map(|item| item.outcome.status())
            .collect()
    }
}

/// Catches `--yes` deleting more than Safe deletes, or moving a directory whole to the
/// trash without a review (pinned 1): with trash configured nothing is planned, and it
/// exits 3; with delete configured both items go and it exits 0.
#[test]
fn yes_cleans_safe_deletes_only() {
    let trash = Machine::new(Method::Trash);
    let refused = trash.clean(Consent::Yes, &mut Events::default(), &CancelToken::new());
    assert_eq!(refused.ending, Ending::NoneRan);
    assert_eq!(refused.ending.code(), 3);
    assert!(trash.world.state().runs.is_empty(), "nothing was stored");
    assert!(trash.world.state().trash.is_empty(), "nothing moved");

    let delete = Machine::new(Method::Delete);
    let finished = delete.clean(Consent::Yes, &mut Events::default(), &CancelToken::new());
    assert_eq!(finished.ending, Ending::Done);
    assert_eq!(finished.ending.code(), 0);
    assert_eq!(Machine::statuses(&finished), [Status::Done, Status::Done]);
    assert!(
        delete.world.state().targets.is_empty(),
        "every target was removed"
    );
}

/// Catches a run whose last record couldn't be written ending as done (D-067): the
/// item was deleted, but the stored run doesn't say so, so the command exits 1, not 0.
#[test]
fn a_failed_last_record_fails_the_command() {
    let counted = Machine::new(Method::Delete);
    counted.clean(Consent::Yes, &mut Events::default(), &CancelToken::new());
    let writes = counted.world.state().writes;

    let machine = Machine::new(Method::Delete);
    machine.world.state().journal_fails_at = Some(writes - 1);
    let finished = machine.clean(Consent::Yes, &mut Events::default(), &CancelToken::new());
    assert_eq!(Machine::statuses(&finished), [Status::Done, Status::Done]);
    assert_eq!(finished.ending, Ending::Failed);
    assert_eq!(finished.ending.code(), 1);
}

/// Catches a prompt that authorizes a plan other than the one shown, or a "no" that
/// stores or runs anything (pinned 1): yes authorizes exactly the plan shown, by its
/// digest, trash items included; no runs nothing and exits 3.
#[test]
fn a_person_authorizes_exactly_the_plan_shown() {
    let machine = Machine::new(Method::Trash);
    let mut no = person(Answer::No);
    let declined = machine.clean(
        Consent::Ask(&mut no),
        &mut Events::default(),
        &CancelToken::new(),
    );
    assert_eq!(declined.ending, Ending::Declined);
    assert_eq!(declined.ending.code(), 3);
    assert!(
        machine.world.state().runs.is_empty(),
        "a declined plan stores no run"
    );
    assert_eq!(machine.world.state().ran, Vec::new());

    let mut yes = person(Answer::Yes);
    let finished = machine.clean(
        Consent::Ask(&mut yes),
        &mut Events::default(),
        &CancelToken::new(),
    );
    assert_eq!(finished.ending, Ending::Done);
    assert_eq!(Machine::statuses(&finished), [Status::Done, Status::Done]);
    let run = finished.run.unwrap();
    let RunRead::Read(stored) = read_run(&machine.world, &run, Depth::Headers, &TestCodec) else {
        panic!("the run reads back");
    };
    assert_eq!(
        yes.shown,
        [stored.plan().digest()],
        "the plan run is the plan shown"
    );
    assert_eq!(machine.world.state().trash.len(), 2);
}

/// Catches a command that runs while another holds the store's lock (pinned 10): it
/// refuses before storing or touching anything, and exits 1.
#[test]
fn a_busy_store_refuses_before_anything_runs() {
    let machine = Machine::new(Method::Delete);
    machine.world.state().busy = true;
    let finished = machine.clean(Consent::Yes, &mut Events::default(), &CancelToken::new());
    assert_eq!(finished.ending, Ending::Refused(StoreRefusal::Busy));
    assert_eq!(finished.ending.code(), 1);
    let state = machine.world.state();
    assert!(state.runs.is_empty(), "a refused lock stores no run");
    assert_eq!(state.ran, Vec::new());
    assert_eq!(state.targets.len(), 2);
}

/// Catches a new run started over one a crash left pending (pinned 10): the next command
/// reconciles it first, tells its frontend so, then stores and runs its own.
#[test]
fn a_pending_run_is_reconciled_before_the_next_one_starts() {
    let machine = Machine::new(Method::Delete);
    machine.world.state().crash = Some(Crash::AfterWrite(1));
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        machine.clean(Consent::Yes, &mut Events::default(), &CancelToken::new())
    }));
    assert!(crashed.is_err(), "the first run crashed after its intent");
    machine.world.state().crash = None;
    let first = machine.world.state().runs.keys().next().cloned().unwrap();

    let mut events = Events::default();
    let finished = machine.clean(Consent::Yes, &mut events, &CancelToken::new());
    assert_eq!(finished.ending, Ending::Done);
    let kinds: Vec<&str> = events
        .0
        .iter()
        .map(|event| match event {
            ApplyEvent::Reconciled { .. } => "reconciled",
            ApplyEvent::RunStarted { .. } => "run-started",
            ApplyEvent::Item {
                event: ItemEvent::Started { .. },
                ..
            } => "item-started",
            ApplyEvent::Item {
                event: ItemEvent::Ended { .. },
                ..
            } => "item-ended",
            ApplyEvent::Unreadable { .. } => "unreadable",
            ApplyEvent::RestoreStarted { .. } => "restore-started",
            ApplyEvent::RestoreEnded { .. } => "restore-ended",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "reconciled",
            "run-started",
            "item-started",
            "item-ended",
            "item-started",
            "item-ended"
        ]
    );
    assert!(
        matches!(
            &events.0[0],
            ApplyEvent::Reconciled { run, reconciled: Reconciled::Item { item: ItemId(0), outcome: ItemOutcome::Indeterminate(_) } }
                if *run == first
        ),
        "the interrupted item is recorded indeterminate in its own run: {:?}",
        events.0[0]
    );
}

/// Catches a restore that isn't journalled, runs twice, or claims an item it never
/// moved (pinned 12): it moves the item back once, then says already restored; a deleted
/// item isn't in the trash.
#[test]
fn a_restore_moves_an_item_back_once() {
    let machine = Machine::new(Method::Trash);
    let finished = machine.clean(
        Consent::Ask(&mut person(Answer::Yes)),
        &mut Events::default(),
        &CancelToken::new(),
    );
    let run = finished.run.unwrap();
    let ask = RestoreAsk {
        run: run.clone(),
        items: vec![ItemId(0)],
    };
    let restored = machine.restore(&ask, &mut Events::default());
    assert_eq!(restored.ending, Ending::Done);
    assert_eq!(
        restored.items[0].restoring,
        Restoring::Ran(RestoreResult::Restored)
    );
    assert!(
        machine
            .world
            .state()
            .targets
            .contains_key(&home("p0/target")),
        "the item is back at its place"
    );
    let RunRead::Read(stored) = read_run(&machine.world, &run, Depth::Headers, &TestCodec) else {
        panic!("the run reads back");
    };
    assert_eq!(
        stored.receipt().items[0].restored,
        Some(RestoreResult::Restored)
    );

    let again = machine.restore(&ask, &mut Events::default());
    assert_eq!(again.items[0].restoring, Restoring::AlreadyRestored);
    assert_eq!(again.ending, Ending::NoneRan);

    let deleted = Machine::new(Method::Delete);
    let cleaned = deleted.clean(Consent::Yes, &mut Events::default(), &CancelToken::new());
    let every = RestoreAsk {
        run: cleaned.run.unwrap(),
        items: Vec::new(),
    };
    let nothing = deleted.restore(&every, &mut Events::default());
    assert!(
        nothing
            .items
            .iter()
            .all(|item| item.restoring == Restoring::NotTrashed),
        "a deleted item isn't in the trash: {:?}",
        nothing.items
    );
    assert_eq!(nothing.ending, Ending::NoneRan);
}

/// Catches a restore that moved an item back reported as "nothing ran" (exit 3) because
/// another item it was asked for had nothing to restore: the item is back, so it is done.
#[test]
fn a_restore_that_moved_an_item_back_is_done_whatever_else_it_was_asked() {
    let machine = Machine::new(Method::Trash);
    let finished = machine.clean(
        Consent::Ask(&mut person(Answer::Yes)),
        &mut Events::default(),
        &CancelToken::new(),
    );
    let ask = RestoreAsk {
        run: finished.run.unwrap(),
        items: vec![ItemId(0), ItemId(99)],
    };

    let restored = machine.restore(&ask, &mut Events::default());

    assert_eq!(
        restored.items[0].restoring,
        Restoring::Ran(RestoreResult::Restored)
    );
    assert_eq!(restored.items[1].restoring, Restoring::NoSuchItem);
    assert_eq!(restored.ending, Ending::Done);
}

/// Catches a trash move a crash interrupted that can't be restored once reconciled
/// (docs/04 §7): reconciliation finds the item in the trash, and restore moves it back.
#[test]
fn a_reconciled_trash_move_restores() {
    let machine = Machine::new(Method::Trash);
    machine.world.state().crash = Some(Crash::BeforeWrite(2));
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        machine.clean(
            Consent::Ask(&mut person(Answer::Yes)),
            &mut Events::default(),
            &CancelToken::new(),
        )
    }));
    assert!(crashed.is_err(), "the run crashed after its first move");
    machine.world.state().crash = None;
    let run = machine.world.state().runs.keys().next().cloned().unwrap();

    let ask = RestoreAsk {
        run,
        items: vec![ItemId(0)],
    };
    let restored = machine.restore(&ask, &mut Events::default());
    assert_eq!(
        restored.items[0].restoring,
        Restoring::Ran(RestoreResult::Restored)
    );
    assert!(
        machine
            .world
            .state()
            .targets
            .contains_key(&home("p0/target")),
        "the item is back at its place"
    );
}

/// Catches a plan file whose items the current config refuses reported as stale, which
/// sends the person to make a new plan that fails the same way, or its drops lost under
/// `--yes`: every item drops for its method, it exits 3 as none ran, and says why.
#[test]
fn a_file_the_config_refuses_is_not_stale() {
    let mut machine = Machine::new(Method::Delete);
    let planned = plan_all(
        &machine.scan,
        &machine.config,
        &machine.world,
        PlanMode::Safe,
    );
    let file = encode_plan_file(&planned.plan, &TestCodec).unwrap();
    let imported = decode_plan_file(&file, &TestCodec).unwrap();
    machine.config.methods.config = Some(Method::Trash);

    let finished = machine.apply(&imported, Consent::Yes);
    assert_eq!(finished.ending, Ending::NoneRan);
    let why: Vec<&Dropped> = finished.dropped.iter().map(|item| &item.why).collect();
    assert_eq!(why.len(), 2, "both items say why they dropped: {why:?}");
    assert!(
        why.iter().all(|why| !why.stale()),
        "a refused item isn't stale: {why:?}"
    );
    assert!(machine.world.state().runs.is_empty(), "nothing was stored");
}

/// Catches a cancel before the run starts that stores or runs anything, or exits as if
/// it finished (pinned 17): exit 130.
#[test]
fn a_cancel_before_the_run_exits_130() {
    let machine = Machine::new(Method::Delete);
    let cancel = CancelToken::new();
    cancel.cancel();
    let finished = machine.clean(Consent::Yes, &mut Events::default(), &cancel);
    assert_eq!(finished.ending, Ending::Cancelled);
    assert_eq!(finished.ending.code(), 130);
    assert_eq!(machine.world.state().ran, Vec::new());
}

/// Catches a cancel during the wait for another's lock reported as a busy store, exit 1
/// (pinned 17): it is a cancel, exit 130, and nothing runs.
#[test]
fn a_cancel_while_waiting_for_the_lock_exits_130() {
    let machine = Machine::new(Method::Delete);
    machine.world.state().busy = true;
    let cancel = CancelToken::new();
    cancel.cancel();
    let finished = machine.clean(Consent::Yes, &mut Events::default(), &cancel);
    assert_eq!(finished.ending, Ending::Cancelled);
    assert_eq!(finished.ending.code(), 130);
    assert_eq!(machine.world.state().ran, Vec::new());
}

/// A person who presses Ctrl+C while asked, then answers.
struct Interrupted<'token> {
    answer: Answer,
    cancel: &'token CancelToken,
}

impl Confirm for Interrupted<'_> {
    fn confirm(&mut self, _: &Review<'_>) -> Answer {
        self.cancel.cancel();
        self.answer
    }
}

/// Catches Ctrl+C at the prompt exiting 3 when the person then answers no, or running
/// when they answer yes (pinned 17): either way the command is cancelled, exit 130, and
/// nothing is stored or run.
#[test]
fn a_cancel_at_the_prompt_exits_130_whatever_the_answer() {
    for answer in [Answer::No, Answer::Yes] {
        let machine = Machine::new(Method::Delete);
        let cancel = CancelToken::new();
        let mut person = Interrupted {
            answer,
            cancel: &cancel,
        };
        let finished = machine.clean(Consent::Ask(&mut person), &mut Events::default(), &cancel);
        assert_eq!(finished.ending, Ending::Cancelled, "{answer:?}");
        assert!(machine.world.state().runs.is_empty(), "{answer:?}: no run");
        assert_eq!(machine.world.state().ran, Vec::new(), "{answer:?}");
    }
}

/// Catches the typed-size question comparing only the bytes measured (spec M3c pinned
/// 16): a plan whose size is a lower bound may be above the threshold, so it is confirmed
/// by typing its size, not by a bare `y`, however little was measured.
#[test]
fn a_total_that_is_a_lower_bound_exceeds_a_threshold_above_it() {
    let threshold = 1_000_000;
    let incomplete = Completeness::Incomplete {
        unreadable: 1,
        skipped_mounts: 0,
    };
    for complete in [incomplete, Completeness::Cancelled] {
        let total = Total {
            known: 10,
            complete,
        };
        assert!(total.exceeds(threshold), "{complete:?} exceeds it");
    }
    let measured = Total {
        known: 10,
        complete: Completeness::Complete,
    };
    assert!(
        !measured.exceeds(threshold),
        "a measured total below it doesn't"
    );
}

/// Cancels the token as the run's last item ends, as Ctrl+C during its last moments
/// does.
struct CancelAtLastEnd<'token> {
    events: Events,
    cancel: &'token CancelToken,
}

impl EventSink for CancelAtLastEnd<'_> {
    fn event(&mut self, event: ApplyEvent) {
        if let ApplyEvent::Item {
            event: ItemEvent::Ended { item, .. },
            ..
        } = &event
            && *item == ItemId(1)
        {
            self.cancel.cancel();
        }
        self.events.event(event);
    }
}

/// Catches a cancel that no item noticed ending the command as done, exit 0 (pinned
/// 17): what finished stays finished, and the command exits 130.
#[test]
fn a_cancel_during_the_last_item_still_exits_130() {
    let machine = Machine::new(Method::Delete);
    let cancel = CancelToken::new();
    let mut events = CancelAtLastEnd {
        events: Events::default(),
        cancel: &cancel,
    };
    let finished = machine.clean(Consent::Yes, &mut events, &cancel);
    assert_eq!(Machine::statuses(&finished), [Status::Done, Status::Done]);
    assert_eq!(finished.ending, Ending::Cancelled);
    assert_eq!(finished.ending.code(), 130);
}

/// Catches a declined plan file losing what dropped from it (D-067: the dropped items,
/// with why, are part of what the command reports): the person said no, and the receipt
/// still names the item that is gone.
#[test]
fn a_declined_plan_file_still_says_what_dropped() {
    let mut machine = Machine::new(Method::Delete);
    let planned = plan_all(
        &machine.scan,
        &machine.config,
        &machine.world,
        PlanMode::Safe,
    );
    let file = encode_plan_file(&planned.plan, &TestCodec).unwrap();
    let imported = decode_plan_file(&file, &TestCodec).unwrap();
    machine.scan.findings.pop();

    let mut no = person(Answer::No);
    let finished = machine.apply(&imported, Consent::Ask(&mut no));
    assert_eq!(finished.ending, Ending::Declined);
    assert_eq!(
        no.shown.len(),
        1,
        "the person was asked about the item left"
    );
    assert_eq!(finished.dropped.len(), 1, "{:?}", finished.dropped);
    assert!(machine.world.state().runs.is_empty(), "nothing was stored");
}

/// Catches apply trusting the scan's activity or git's answer from planning (pinned 3,
/// 4, 15): a program using the item now, or git tracking more of it now, only takes
/// away; git failing now skips it.
#[test]
fn activity_and_git_are_asked_again_at_apply() {
    let mut busy = Machine::new(Method::Delete);
    busy.rescan.activity = Some(Activity::Unknown(ActivityGap::Uninspected(1)));
    let skipped = busy.clean(Consent::Yes, &mut Events::default(), &CancelToken::new());
    assert!(
        skipped.receipt.items.iter().all(|item| matches!(
            &item.outcome,
            ItemOutcome::Skipped(Skip::Protected(holds)) if holds.contains(&Hold::UnknownActivity)
        )),
        "{:?}",
        skipped.receipt
    );
    assert_eq!(busy.world.state().ran, Vec::new());

    let mut tracked = Machine::new(Method::Delete);
    for finding in tracked
        .scan
        .findings
        .iter_mut()
        .chain(tracked.rescan.scan.findings.iter_mut())
    {
        finding.facts.as_mut().unwrap().tracked = Tracked::Asked {
            work_tree: finding.path.parent().unwrap().to_owned(),
            tracked: 0,
        };
    }
    {
        let mut state = tracked.world.state();
        state.git.insert(home("p0/target"), Ok(Vec::new()));
        state.git.insert(home("p1/target"), Err(GitGap::Failed));
    }
    let planned = plan_all(
        &tracked.scan,
        &tracked.config,
        &tracked.world,
        PlanMode::SafeOnly,
    );
    assert_eq!(planned.plan.entries().len(), 1, "git failing refuses p1");
    tracked
        .world
        .state()
        .git
        .insert(home("p0/target"), Ok(vec![rel("debug/b")]));
    let finished = tracked.clean(Consent::Yes, &mut Events::default(), &CancelToken::new());
    let outcome = &finished.receipt.items[0].outcome;
    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    assert_eq!(
        outcome.effect().map(|effect| effect.excluded),
        Some(1),
        "the entry git tracks now is left out"
    );
    let state = tracked.world.state();
    let left = &state.targets[&home("p0/target")];
    assert!(
        left.entries.contains_key(&rel("debug/b")),
        "what git tracks now stays"
    );
    assert!(!left.entries.contains_key(&rel("a")), "the rest goes");
}
