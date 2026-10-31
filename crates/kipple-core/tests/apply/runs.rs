//! Runs as the store keeps them: the receipt read back, torn tails, reconciliation after
//! any crash, and what records may hold (spec M3b pinned 3, 6, 8, 9, 19).

use std::collections::BTreeSet;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use kipple_core::format::{decode_line, encode_line, encode_stored_plan};
use kipple_core::{
    ApplyError, ApplyPorts, CancelToken, CleanRequest, Component, Consent, Depth, Digest, Effect,
    Ending, ErrorCode, Finished, Found, Intent, ItemId, ItemOutcome, Method, OsError, OsErrorKind,
    PlanMode, Receipt, Record, RestoreResult, RunId, RunRead, RunState, Session, Skip, Stop,
    TrashSlot, Trashed, Unreadable, history, read_run, run_clean, run_reconcile,
};

use super::codec::TestCodec;
use super::world::{Crash, Crashed, Rescanned, World};
use super::{
    Config, Item, SplitMix, Taint, change_item, everything, ports, project, random_entries,
    rescanned, scan_of, world_of,
};

/// Answers yes to every prompt.
struct Yes;

impl kipple_core::Confirm for Yes {
    fn confirm(&mut self, _: &kipple_core::Review<'_>) -> kipple_core::Answer {
        kipple_core::Answer::Yes
    }
}

/// Runs `clean` of every finding of `scan` on `world`, rescanning as `rescan`.
fn clean(
    world: &World,
    config: &Config,
    scan: &kipple_core::ScanReport,
    rescan: &Rescanned,
    cancel: &CancelToken,
) -> Finished {
    let mutation = ports(world);
    let session = Session {
        judge: config.judge(),
        store: world,
        ports: ApplyPorts {
            rescan,
            mutation: &mutation,
        },
        codec: &TestCodec,
    };
    let selection = everything(scan);
    let request = CleanRequest {
        scan,
        selection: &selection,
    };
    run_clean(&request, &session, Consent::Ask(&mut Yes), &mut (), cancel)
}

/// Reconciles every pending run of `world`, as the next mutating command does first.
fn reconcile_all(world: &World, config: &Config) -> Ending {
    let mutation = ports(world);
    let rescan = Rescanned {
        scan: scan_of(&[]),
        activity: None,
    };
    let session = Session {
        judge: config.judge(),
        store: world,
        ports: ApplyPorts {
            rescan: &rescan,
            mutation: &mutation,
        },
        codec: &TestCodec,
    };
    run_reconcile(&session, &mut (), &CancelToken::new())
}

/// The one run `world` stored, read back to its headers.
fn the_run(world: &World) -> Option<kipple_core::StoredRun> {
    let id = world.state().runs.keys().next().cloned()?;
    let read = read_run(world, &id, Depth::Headers, &TestCodec);
    assert!(
        !matches!(read, RunRead::Unreadable(..)),
        "a crash leaves a readable run: {read:?}"
    );
    if let RunRead::Read(run) = read {
        Some(run)
    } else {
        None
    }
}

/// What a generated run did.
#[derive(Debug)]
struct Outcome {
    crashed: bool,
    finished: Option<Finished>,
}

/// A generated `clean` of up to three items, with changes between plan and apply, failed
/// and torn writes, crashes after any record or removal, and cancels.
fn generated_clean(seed: u64) -> (World, Config, Outcome) {
    let mut rng = SplitMix(seed);
    let count = 1 + rng.pick(3);
    let projects: Vec<_> = (0..count)
        .map(|n| {
            let entries = random_entries(&mut rng);
            let entries: Vec<&str> = entries.iter().map(String::as_str).collect();
            project(n as u128, 1000 * (n as u128 + 1), &entries)
        })
        .collect();
    let world = world_of(&projects);
    let items: Vec<Item> = projects.into_iter().map(|(item, _)| item).collect();
    let scan = scan_of(&items);
    let mut config = Config::new();
    if rng.one_in(3) {
        config.methods.config = Some(Method::Trash);
    }
    let mut rescan = scan.clone();
    let _taints: Vec<Taint> = (0..count)
        .map(|index| change_item(&mut rng, &world, &mut rescan, &items[index], index))
        .collect();
    let writes = 2 * count + 1;
    {
        let mut state = world.state();
        if rng.one_in(4) {
            state.journal_fails_at = Some(rng.pick(writes + 1));
            state.journal_recovers = rng.one_in(2);
        }
        match rng.below(8) {
            0 => state.crash = Some(Crash::AfterWrite(rng.pick(writes))),
            1 => state.crash = Some(Crash::BeforeWrite(rng.pick(writes))),
            2 => state.crash = Some(Crash::AfterRemovals(1 + rng.pick(6))),
            3 | 4 => {
                let write = rng.pick(writes);
                let bytes = rng.pick(400);
                state.crash = Some(Crash::Torn { write, bytes });
            }
            _ => {}
        }
        if rng.one_in(5) {
            state.cancel_after = Some(rng.pick(8));
        }
    }
    let rescan = rescanned(&rescan);
    let ran = catch_unwind(AssertUnwindSafe(|| {
        clean(&world, &config, &scan, &rescan, &CancelToken::new())
    }));
    assert!(
        ran.as_ref()
            .err()
            .is_none_or(|payload| payload.is::<Crashed>()),
        "seed {seed}: only a crash unwinds"
    );
    let outcome = Outcome {
        crashed: ran.is_err(),
        finished: ran.ok(),
    };
    (world, config, outcome)
}

/// The names each item of `receipt` says it removed.
fn removed_by_item(receipt: &Receipt) -> Vec<u64> {
    receipt
        .items
        .iter()
        .map(|item| {
            item.outcome
                .effect()
                .map_or(0, |effect| effect.removed.names)
        })
        .collect()
}

/// Catches a receipt read back from the store that differs from the one apply returned,
/// one that claims more than the disk shows after a crash, a torn tail that makes a run
/// unreadable, and reconciliation that runs anything again or leaves anything pending,
/// including after an upgrade changed every judgement digest (spec M3b pinned 3, 8, 9).
#[test]
fn a_run_read_back_says_what_ran_and_reconciling_it_runs_nothing() {
    for seed in 0..600 {
        let (world, mut config, outcome) = generated_clean(seed);
        let Some(run) = the_run(&world) else {
            let state = world.state();
            assert!(
                state.removals.is_empty() && state.ran.is_empty(),
                "seed {seed}: a run that never started removed nothing"
            );
            continue;
        };
        let read = check_read_back(seed, &world, &run, &outcome);
        if SplitMix(seed).one_in(2) {
            // An upgrade: every judgement and rule digest differs from the stored ones.
            config.rules[0].text.title.push_str(" (v2)");
            config.policy.level = kipple_core::Level::Thorough;
        }
        let after = check_reconciled_run(seed, &world, &config, &run);
        if outcome.crashed {
            assert_eq!(
                after.totals().removed.names,
                read.totals().removed.names,
                "seed {seed}: reconciliation never counts what it found gone as removed"
            );
        }
    }
}

/// The receipt `run` reads back as: the one apply returned when no write failed, and
/// never more than the disk shows.
fn check_read_back(
    seed: u64,
    world: &World,
    run: &kipple_core::StoredRun,
    outcome: &Outcome,
) -> Receipt {
    let read = run.receipt();
    let failed = world.state().failed_write.is_some();
    if let Some(finished) = outcome.finished.as_ref().filter(|_| !failed) {
        assert_eq!(
            read, finished.receipt,
            "seed {seed}: the receipt read back is the one apply returned"
        );
    }
    let removals = world.state().removals.clone();
    for (item, names) in (0..).zip(removed_by_item(&read)) {
        let on_disk = removals.iter().filter(|r| r.item == ItemId(item)).count();
        assert!(
            names <= on_disk as u64,
            "seed {seed}: item {item} claims {names} removed, the disk shows {on_disk}"
        );
    }
    read
}

/// Reconciles `world` under `config` once its faults are gone, and returns `run`'s
/// receipt after: nothing ran again, nothing stays pending, and each item that was
/// pending is indeterminate with what was found.
fn check_reconciled_run(
    seed: u64,
    world: &World,
    config: &Config,
    run: &kipple_core::StoredRun,
) -> Receipt {
    let (ran, removed) = {
        let mut state = world.state();
        state.crash = None;
        state.journal_fails_at = None;
        state.cancel_after = None;
        (state.ran.clone(), state.removals.len())
    };
    let pending: BTreeSet<ItemId> = run.pending().iter().map(|intent| intent.item).collect();
    assert_eq!(reconcile_all(world, config), Ending::Done, "seed {seed}");
    {
        let state = world.state();
        assert_eq!(state.ran, ran, "seed {seed}: reconciliation runs nothing");
        assert_eq!(
            state.removals.len(),
            removed,
            "seed {seed}: and removes nothing"
        );
    }
    let after = the_run(world).unwrap();
    assert_eq!(after.pending(), Vec::<&Intent>::new(), "seed {seed}");
    assert!(
        pending.is_empty() || !after.torn(),
        "seed {seed}: the next append cut the torn tail"
    );
    let receipt = after.receipt();
    for item in receipt
        .items
        .iter()
        .filter(|item| pending.contains(&item.item))
    {
        assert!(
            matches!(
                &item.outcome,
                ItemOutcome::Indeterminate(found) if !matches!(found, Found::Unreadable(_) | Found::Pending)
            ),
            "seed {seed}: item {} reconciles by its stored digests, after an upgrade too: {:?}",
            item.item.0,
            item.outcome
        );
    }
    receipt
}

/// A run of one item, stored with `records`.
fn run_with_records(records: &[Record]) -> (World, RunId) {
    let (item, target) = project(0, 100, &["a", "b"]);
    let world = world_of(&[(item.clone(), target)]);
    let scan = scan_of(&[item]);
    let config = Config::new();
    let planned = super::plan_all(&scan, &config, &world, PlanMode::Review);
    let mut first = vec![Record::Plan {
        digest: planned.plan.digest(),
    }];
    first.extend_from_slice(records);
    world.state().records = first;
    let id = world.store(&planned.plan);
    (world, id)
}

const fn skipped(item: u32) -> Record {
    Record::Outcome {
        item: ItemId(item),
        outcome: ItemOutcome::Skipped(Skip::NotReviewed),
    }
}

/// Catches a torn last line making a run unreadable, or a corrupt line in the middle
/// trusted (pinned 8): a bad last line is ignored and the next append starts where it
/// began; a bad line followed by a good one makes the run unreadable.
#[test]
fn a_torn_last_line_is_ignored_and_a_bad_line_inside_makes_the_run_unreadable() {
    let (world, id) = run_with_records(&[]);
    let sound = world.state().runs[&id].journal.len();
    let line = encode_line(&skipped(0), &TestCodec);
    for cut in 0..line.len() - 1 {
        world
            .state()
            .runs
            .get_mut(&id)
            .unwrap()
            .journal
            .truncate(sound);
        world
            .state()
            .runs
            .get_mut(&id)
            .unwrap()
            .journal
            .extend_from_slice(&line[..cut]);
        let RunRead::Read(run) = read_run(&world, &id, Depth::Headers, &TestCodec) else {
            panic!("a torn tail at {cut} leaves the run readable");
        };
        assert_eq!(run.records().len(), 1, "only the plan record counts");
        assert_eq!(run.torn(), cut > 0);
    }

    let mut flipped = line;
    flipped[20] ^= 1;
    let mut journal = world.state().runs[&id].journal[..sound].to_vec();
    journal.extend_from_slice(&flipped);
    journal.extend_from_slice(&encode_line(&skipped(0), &TestCodec));
    world.state().runs.get_mut(&id).unwrap().journal = journal;
    assert!(
        matches!(
            read_run(&world, &id, Depth::Headers, &TestCodec),
            RunRead::Unreadable(_, Unreadable::Journal(at)) if at == sound as u64
        ),
        "a corrupt line followed by a sound one makes the run unreadable"
    );
    let summaries = history(&world, kipple_core::LockState::Free, &TestCodec).unwrap();
    assert_eq!(summaries[0].state, RunState::Unreadable);
    assert_eq!(
        summaries[0].receipt.totals().removed.names,
        0,
        "nothing in an unreadable run counts as removed"
    );
    assert_eq!(
        reconcile_all(&world, &Config::new()),
        Ending::Done,
        "an unreadable run never blocks the next command"
    );
}

/// Catches records out of the journal's order trusted: an outcome twice, an intent for
/// another plan, or a restore of an item that never ended (pinned 6).
#[test]
fn records_out_of_order_make_the_run_unreadable() {
    let intent = |plan| {
        Record::Intent(Intent {
            plan,
            item: ItemId(0),
            method: Method::Delete,
            reviewed: Digest(1),
            authorized: Digest(1),
            entries: 1,
            trash: None,
        })
    };
    let slot = TrashSlot {
        trash: super::world::trash_dir(),
        name: Component::new("item-0").unwrap(),
    };
    for records in [
        vec![skipped(0), skipped(0)],
        vec![intent(Digest(99))],
        vec![Record::RestoreIntent {
            item: ItemId(0),
            slot,
        }],
        vec![skipped(7)],
    ] {
        let (world, id) = run_with_records(&records);
        assert!(
            matches!(
                read_run(&world, &id, Depth::Headers, &TestCodec),
                RunRead::Unreadable(_, Unreadable::Order)
            ),
            "{records:?} is out of order"
        );
    }
}

/// A run in `history` whose intent has no outcome is `running` while another kipple
/// holds the lock and `interrupted` when nobody does (pinned 10).
#[test]
fn history_says_running_only_while_the_lock_is_held() {
    let intent = Record::Intent(Intent {
        plan: Digest(0),
        item: ItemId(0),
        method: Method::Delete,
        reviewed: Digest(0),
        authorized: Digest(0),
        entries: 1,
        trash: None,
    });
    let (world, id) = run_with_records(&[]);
    let read = read_run(&world, &id, Depth::Trees, &TestCodec);
    let RunRead::Read(run) = read else {
        panic!("the run reads back: {read:?}");
    };
    let mut intent = intent;
    if let Record::Intent(pending) = &mut intent {
        pending.plan = run.plan().digest();
        pending.reviewed = run.plan().items()[0].tree_digest();
    }
    let line = encode_line(&intent, &TestCodec);
    world
        .state()
        .runs
        .get_mut(&id)
        .unwrap()
        .journal
        .extend_from_slice(&line);
    for (lock, state) in [
        (kipple_core::LockState::Held, RunState::Running),
        (kipple_core::LockState::Free, RunState::Interrupted),
    ] {
        let summaries = history(&world, lock, &TestCodec).unwrap();
        assert_eq!(summaries[0].state, state);
        assert_eq!(
            summaries[0].receipt.items[0].outcome,
            ItemOutcome::Indeterminate(Found::Pending)
        );
    }
}

/// Every kind of record, each error with an OS message and a path in it.
fn every_record() -> Vec<Record> {
    let error = ApplyError {
        code: ErrorCode::Io,
        os: Some(OsError {
            kind: OsErrorKind::PermissionDenied,
            code: Some(13),
        }),
        at: Some(super::rel("debug/app")),
        detail: Some(Arc::new(io::Error::other("SECRET free text"))),
    };
    let slot = TrashSlot {
        trash: super::world::trash_dir(),
        name: Component::new("item-0").unwrap(),
    };
    let trashed = Effect {
        trashed: Some(Trashed { slot: slot.clone() }),
        ..Effect::default()
    };
    vec![
        Record::Plan { digest: Digest(1) },
        Record::Intent(Intent {
            plan: Digest(1),
            item: ItemId(0),
            method: Method::Trash,
            reviewed: Digest(2),
            authorized: Digest(3),
            entries: 4,
            trash: Some(slot.clone()),
        }),
        Record::Outcome {
            item: ItemId(0),
            outcome: ItemOutcome::Done(trashed),
        },
        Record::Outcome {
            item: ItemId(1),
            outcome: ItemOutcome::Failed {
                error: error.clone(),
                effect: Effect::default(),
            },
        },
        Record::Outcome {
            item: ItemId(2),
            outcome: ItemOutcome::Skipped(Skip::Freeze(error.clone())),
        },
        Record::Outcome {
            item: ItemId(3),
            outcome: ItemOutcome::Skipped(Skip::NotStarted(Stop::Cancelled)),
        },
        Record::Outcome {
            item: ItemId(4),
            outcome: ItemOutcome::Indeterminate(Found::Unreadable(error.clone())),
        },
        Record::RestoreIntent {
            item: ItemId(0),
            slot: slot.clone(),
        },
        Record::RestoreOutcome {
            item: ItemId(0),
            result: RestoreResult::Failed(error),
        },
        Record::TrashRecordRemoved {
            item: ItemId(0),
            slot,
        },
    ]
}

/// The keys a record's strings may be under: each holds a code, a digest, a name or a
/// path, never free text.
const STRUCTURED: &[&str] = &[
    "schema",
    "digest",
    "plan",
    "reviewed",
    "authorized",
    "type",
    "method",
    "status",
    "skip",
    "found",
    "code",
    "os_kind",
    "at",
    "name",
    "path",
    "result",
    "identity",
    "raw",
    "kipple",
    "root",
    "root_path",
    "root_identity",
    "relative",
    "rule",
    "bucket",
    "cautions",
    "level",
    "policy",
    "rule_digest",
    "held",
    "parent",
    "stop",
    "file_system",
    "stored",
    "state",
    "field",
    "holds",
    "reason",
    "kind",
    "trash",
    "protection",
    "with",
    "gap",
    "slot",
    "unsupported",
    "unjudged",
    "scope",
    "integration_digest",
    "enabled",
];

/// Every key in `value` whose value is, or holds, a string.
fn string_keys(value: &serde_json::Value, key: &str, keys: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(_) => {
            keys.insert(key.to_owned());
        }
        serde_json::Value::Array(values) => {
            for one in values {
                string_keys(one, key, keys);
            }
        }
        serde_json::Value::Object(map) => {
            for (inner, one) in map {
                string_keys(one, inner, keys);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// Catches a stored record holding free text: an OS message, stderr, a command line or a
/// kept path's note (pinned 19; M3c pinned 19; 04 §8a). Every record kind and a profile
/// is decoded back and every string in it is under a key that holds a code, digest, name
/// or path.
#[test]
fn stored_records_hold_no_free_text() {
    let (item, target) = project(0, 100, &["a"]);
    let world = world_of(&[(item.clone(), target)]);
    let planned = super::plan_all(&scan_of(&[item]), &Config::new(), &world, PlanMode::Review);
    let header = encode_stored_plan(&planned.plan, &TestCodec).unwrap();
    let header = &header[..=header.iter().position(|byte| *byte == b'\n').unwrap()];
    let mut lines = vec![header.to_vec(), super::profile::stored_with_a_note()];
    for record in every_record() {
        let line = encode_line(&record, &TestCodec);
        assert_eq!(
            decode_line(&line[..line.len() - 1], &TestCodec).as_ref(),
            Ok(&record),
            "{record:?} reads back"
        );
        lines.push(line);
    }
    for line in lines {
        assert!(
            !line.windows(6).any(|window| window == b"SECRET"),
            "an OS message is never stored"
        );
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        let mut keys = BTreeSet::new();
        string_keys(&value, "", &mut keys);
        let free: Vec<_> = keys
            .iter()
            .filter(|key| !STRUCTURED.contains(&key.as_str()))
            .collect();
        assert_eq!(
            free,
            Vec::<&String>::new(),
            "keys holding text that isn't structured"
        );
    }
}

/// Catches a run stored without its plan claiming items, or a record a crash kept from
/// landing counted (pinned 6, 7): a run whose first write failed never started.
#[test]
fn a_run_whose_plan_record_never_landed_never_started() {
    let (item, target) = project(0, 100, &["a"]);
    let world = world_of(&[(item.clone(), target)]);
    let scan = scan_of(&[item]);
    world.state().crash = Some(Crash::Torn {
        write: 0,
        bytes: 10,
    });
    let crashed = catch_unwind(AssertUnwindSafe(|| {
        clean(
            &world,
            &Config::new(),
            &scan,
            &rescanned(&scan),
            &CancelToken::new(),
        )
    }));
    assert!(
        crashed.is_err(),
        "the run crashed while storing its plan record"
    );
    let run = the_run(&world).unwrap();
    assert_eq!(run.records(), &[] as &[Record]);
    assert_eq!(
        run.receipt().items[0].outcome,
        ItemOutcome::Skipped(Skip::NotStarted(Stop::Interrupted))
    );
    assert_eq!(world.state().ran, Vec::new());
}
