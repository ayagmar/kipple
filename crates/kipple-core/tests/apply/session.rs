//! Mutating commands end to end through `run_clean`, `run_restore` and their events
//! (spec M3b pinned 1, 3, 4, 10, 12, 17; D-067).

use std::path::Path;

use kipple_core::{
    Activity, ActivityGap, Answer, ApplyEvent, ApplyPorts, CancelToken, CleanRequest, Confirm,
    Consent, Depth, Digest, Ending, EventSink, Finished, GitGap, Hold, ItemEvent, ItemId,
    ItemOutcome, Method, Reconciled, RestoreAsk, RestoreResult, Restoring, Review, RunRead,
    ScanReport, Session, Skip, Status, StoreRefusal, Tracked, read_run, run_clean, run_restore,
};

use super::codec::TestCodec;
use super::world::{Crash, Rescanned, World};
use super::{Config, everything, ports, project, rel, rescanned, scan_of, world_of};

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

    fn clean(&self, consent: Consent<'_>, events: &mut Events, cancel: &CancelToken) -> Finished {
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
            .contains_key(Path::new("/home/u/p0/target")),
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
        state.git.insert("/home/u/p0/target".into(), Ok(Vec::new()));
        state
            .git
            .insert("/home/u/p1/target".into(), Err(GitGap::Failed));
    }
    let planned = super::plan_all(
        &tracked.scan,
        &tracked.config,
        &tracked.world,
        kipple_core::PlanMode::SafeOnly,
    );
    assert_eq!(planned.plan.entries().len(), 1, "git failing refuses p1");
    tracked
        .world
        .state()
        .git
        .insert("/home/u/p0/target".into(), Ok(vec![rel("debug/b")]));
    let finished = tracked.clean(Consent::Yes, &mut Events::default(), &CancelToken::new());
    let outcome = &finished.receipt.items[0].outcome;
    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    assert_eq!(
        outcome.effect().map(|effect| effect.excluded),
        Some(1),
        "the entry git tracks now is left out"
    );
    let state = tracked.world.state();
    let left = &state.targets[Path::new("/home/u/p0/target")];
    assert!(
        left.entries.contains_key(&rel("debug/b")),
        "what git tracks now stays"
    );
    assert!(!left.entries.contains_key(&rel("a")), "the rest goes");
}
