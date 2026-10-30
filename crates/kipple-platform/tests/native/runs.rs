//! The run store on the real file system (spec M3b pinned 6–10): what it stores reads
//! back byte for byte, a crash at any step leaves a run the core can read, a torn tail is
//! cut by the next append, and it refuses what it can't trust. Each test names what it
//! catches. The lock across processes and a full disk are in the library's own tests.
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kipple_core::{
    CancelToken, Clock, ErrorCode, HEADER_LIMIT, JOURNAL_LIMIT, LockState, RunId, RunStore as _,
    StoreRefusal, Unsupported,
};
use kipple_platform::FileRunStore;
use tempfile::TempDir;

use crate::executor::support::{link, present};

/// A clock stopped at 2026-10-30T14:25:01Z.
#[derive(Debug)]
struct Stopped;

impl Clock for Stopped {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_793_370_301)
    }
}

/// The ID the `n`th run created at [`Stopped`]'s time gets.
fn id(n: u16) -> RunId {
    RunId::at(Stopped.now(), n)
}

fn store(data: &Path) -> FileRunStore {
    FileRunStore::new(Some(data.to_owned()), Arc::new(Stopped))
}

/// A stored plan as the core writes one: a header line, then binary trees.
const PLAN: &[u8] = b"{\"header\":1}\n\x00\xfftrees\n\x01";

/// Where `run`'s directory is under the data directory `data`.
fn run_dir(data: &Path, run: &RunId) -> PathBuf {
    data.join("runs").join(run.as_str())
}

/// Cuts the file at `path` to `len` bytes, as a crash mid-write leaves it.
fn cut(path: &Path, len: u64) {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(len)
        .unwrap();
}

/// Catches a run that doesn't read back as stored (wrong names, a header cut short or
/// run on into the trees, a journal record lost), files others can read, or a lock
/// `history` can't see held. Creating and appending sync every directory involved, so
/// this also runs the directory syncs on Linux and macOS.
#[test]
fn a_run_reads_back_as_it_was_stored() {
    let temp = TempDir::new().unwrap();
    let data = temp.path().join("data");
    let store = store(&data);
    assert_eq!(store.runs(), Ok(Vec::new()), "no store yet is no runs");
    assert_eq!(store.lock_state(), Ok(LockState::Free));

    let lock = store.lock(&CancelToken::new()).unwrap();
    let run = store.create(&lock, PLAN, b"first\n").unwrap();
    let end = store.append(&lock, &run, 6, b"second\n").unwrap();

    assert_eq!(run, id(0));
    assert_eq!(end, 13);
    assert_eq!(store.lock_state(), Ok(LockState::Held));
    assert_eq!(store.runs(), Ok(vec![run.clone()]));
    assert_eq!(store.header(&run), Ok(Some(b"{\"header\":1}\n".to_vec())));
    assert_eq!(store.plan(&run), Ok(Some(PLAN.to_vec())));
    assert_eq!(store.journal(&run), Ok(b"first\nsecond\n".to_vec()));
    let dir = run_dir(&data, &run);
    assert_eq!(fs::read(dir.join("plan")).unwrap(), PLAN);
    assert_eq!(
        fs::read(dir.join("journal.jsonl")).unwrap(),
        b"first\nsecond\n"
    );
    #[cfg(unix)]
    readable_by_the_user_alone(&data, &dir);
    drop(lock);
    assert_eq!(store.lock_state(), Ok(LockState::Free));
}

/// Checks the store's directories are 0700 and its files 0600.
#[cfg(unix)]
fn readable_by_the_user_alone(data: &Path, run: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&data.join("runs")), 0o700);
    assert_eq!(mode(run), 0o700);
    for file in [
        run.join("plan"),
        run.join("journal.jsonl"),
        data.join("runs/.lock"),
    ] {
        assert_eq!(mode(&file), 0o600, "{}", file.display());
    }
}

/// Catches a crash inside `create` that leaves a run the core can't read or that blocks
/// the next one (pinned 6, 7): whatever step it stopped at, the run has no plan (never
/// started) or a plan and a journal with no sound record, and the next run takes the
/// next ID.
#[test]
fn a_crash_at_any_step_of_creating_a_run_leaves_one_that_never_started_or_has_no_records() {
    let temp = TempDir::new().unwrap();
    let store = store(temp.path());
    let lock = store.lock(&CancelToken::new()).unwrap();
    // The state each crash leaves: the directory made; the plan part-written; renamed;
    // the journal created; its first record part-written.
    let steps: [&[(&str, &[u8])]; 5] = [
        &[],
        &[("plan.partial", b"{\"hea")],
        &[("plan", PLAN)],
        &[("plan", PLAN), ("journal.jsonl", b"")],
        &[("plan", PLAN), ("journal.jsonl", b"{\"sch")],
    ];
    for (n, files) in (0..).zip(steps) {
        let dir = run_dir(temp.path(), &id(n));
        fs::create_dir(&dir).unwrap();
        for (name, bytes) in files {
            fs::write(dir.join(name), bytes).unwrap();
        }
    }

    let next = store.create(&lock, PLAN, b"first\n").unwrap();

    assert_eq!(next, id(5), "the next run takes the next free ID");
    assert_eq!(store.runs(), Ok((0..6).map(id).collect::<Vec<_>>()));
    for n in [0, 1] {
        assert_eq!(store.header(&id(n)), Ok(None), "run {n} never started");
    }
    for n in [2, 3] {
        assert_eq!(store.journal(&id(n)), Ok(Vec::new()), "run {n}");
    }
    assert_eq!(
        store.journal(&id(4)),
        Ok(b"{\"sch".to_vec()),
        "a torn tail as stored"
    );
}

/// Catches an append that leaves a gap or keeps a torn tail (pinned 8): one at a shorter
/// offset cuts what is past it first, and one past the journal's end fails and changes
/// nothing, never filling the gap with zeros.
#[test]
fn an_append_cuts_the_tail_it_replaces_and_never_leaves_a_gap() {
    let temp = TempDir::new().unwrap();
    let store = store(temp.path());
    let lock = store.lock(&CancelToken::new()).unwrap();
    let run = store.create(&lock, PLAN, b"first\n").unwrap();
    let tail = b"{\"a torn record, longer than the next";
    let end = store.append(&lock, &run, 6, tail).unwrap();

    let past = store.append(&lock, &run, end + 1, b"third\n");
    assert_eq!(past.map_err(|error| error.code), Err(ErrorCode::Journal));
    assert_eq!(store.journal(&run), Ok([&b"first\n"[..], tail].concat()));

    assert_eq!(store.append(&lock, &run, 6, b"second\n"), Ok(13));
    assert_eq!(store.journal(&run), Ok(b"first\nsecond\n".to_vec()));
}

/// Catches a torn last record that a later read takes as sound, or that stays in the
/// journal after the next append (pinned 8): cut at every byte of the last record, the
/// run reads with one record and a torn tail; reconciling appends at the end of the sound
/// records, and the run reads whole again. The baseline's names are a Unix machine's.
#[cfg(unix)]
#[test]
fn a_record_torn_at_any_byte_is_ignored_then_written_over() {
    use kipple_core::{Depth, RunRead, read_run};
    use kipple_platform::OsNames;

    const BASELINES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../kipple/tests/compat");
    let plan = fs::read(format!("{BASELINES}/stored-plan/v1/interrupted-trash.plan")).unwrap();
    let journal = fs::read(format!("{BASELINES}/journal/v1/interrupted-trash.journal")).unwrap();
    let lines: Vec<&[u8]> = journal.split_inclusive(|byte| *byte == b'\n').collect();
    let [first, last] = lines[..] else {
        panic!("the baseline journal holds a plan record and an intent");
    };
    let temp = TempDir::new().unwrap();
    let store = store(temp.path());
    let lock = store.lock(&CancelToken::new()).unwrap();
    let run = store.create(&lock, &plan, first).unwrap();
    let path = run_dir(temp.path(), &run).join("journal.jsonl");
    let sound = first.len() as u64;
    store.append(&lock, &run, sound, last).unwrap();
    let read = || match read_run(&store, &run, Depth::Trees, &OsNames) {
        RunRead::Read(stored) => (stored.records().len(), stored.torn()),
        other @ (RunRead::NeverStarted(_) | RunRead::Unreadable(..)) => panic!("{other:?}"),
    };

    for torn in 0..last.len() as u64 {
        cut(&path, sound + torn);

        assert_eq!(read(), (1, torn > 0), "cut {torn} bytes into the record");

        store.append(&lock, &run, sound, last).unwrap();
        assert_eq!(read(), (2, false), "written over at {torn}");
        assert_eq!(fs::read(&path).unwrap(), journal);
    }
}

/// Catches a stray name under `runs/` read as a run, or runs listed out of the order
/// they were created in, which would reconcile an older run after a newer one.
#[test]
fn only_run_ids_are_runs_and_they_list_in_order() {
    let temp = TempDir::new().unwrap();
    let store = store(temp.path());
    drop(store.lock(&CancelToken::new()).unwrap());
    let runs = temp.path().join("runs");
    for name in [
        id(2).as_str(),
        "notes",
        "20261030T142501Z-00000",
        id(0).as_str(),
    ] {
        fs::create_dir(runs.join(name)).unwrap();
    }

    assert_eq!(store.runs(), Ok(vec![id(0), id(2)]));
}

/// Catches a header or journal past the limits the core reads (`HEADER_LIMIT`,
/// `JOURNAL_LIMIT`) read whole into memory instead of refused.
#[test]
fn a_header_or_journal_past_its_limit_is_refused() {
    let temp = TempDir::new().unwrap();
    let store = store(temp.path());
    let lock = store.lock(&CancelToken::new()).unwrap();
    let run = store.create(&lock, PLAN, b"first\n").unwrap();
    let dir = run_dir(temp.path(), &run);
    // Zeros, so no newline ends the header within the limit.
    fs::write(dir.join("plan"), b"").unwrap();
    cut(&dir.join("plan"), HEADER_LIMIT + 1);
    cut(&dir.join("journal.jsonl"), JOURNAL_LIMIT + 1);

    let header = store.header(&run).map_err(|error| error.code);
    assert_eq!(header, Err(ErrorCode::TooLarge));
    let journal = store.journal(&run).map_err(|error| error.code);
    assert_eq!(journal, Err(ErrorCode::TooLarge));
}

/// Catches a store that follows a link put in place of `runs/`, its lock or a run
/// (pinned 10): it would lock, write and read wherever the link leads.
#[test]
fn a_link_in_place_of_the_runs_their_lock_or_a_run_is_refused() {
    let temp = TempDir::new().unwrap();
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join("file"), b"not the lock").unwrap();

    let linked = temp.path().join("linked");
    fs::create_dir(&linked).unwrap();
    link(&elsewhere, &linked.join("runs"));
    let store_through = store(&linked);
    let refused = store_through.lock(&CancelToken::new());
    assert!(
        matches!(&refused, Err(StoreRefusal::Write(error)) if error.code == ErrorCode::ThroughLink),
        "{refused:?}"
    );
    let listed = store_through.runs();
    assert_eq!(
        listed.map_err(|error| error.code),
        Err(ErrorCode::ThroughLink)
    );

    let real = temp.path().join("real");
    fs::create_dir_all(real.join("runs")).unwrap();
    link(&elsewhere.join("file"), &real.join("runs/.lock"));
    let store = store(&real);
    let lock = store.lock(&CancelToken::new());
    assert!(matches!(lock, Err(StoreRefusal::Lock(_))), "{lock:?}");

    link(&elsewhere, &run_dir(&real, &id(0)));
    let header = store.header(&id(0));
    assert_eq!(
        header.map_err(|error| error.code),
        Err(ErrorCode::ThroughLink)
    );
    assert_eq!(fs::read(elsewhere.join("file")).unwrap(), b"not the lock");
    assert_eq!(
        fs::read_dir(&elsewhere).unwrap().count(),
        1,
        "nothing was made there"
    );
}

/// Catches a store used when kipple's data directory is unknown: unknown is never an
/// empty history or a place to write.
#[test]
fn an_unknown_data_directory_refuses() {
    let store = FileRunStore::new(None, Arc::new(Stopped));

    let lock = store.lock(&CancelToken::new());
    assert!(matches!(lock, Err(StoreRefusal::NoDataDir)), "{lock:?}");
    assert_eq!(store.lock_state(), Err(StoreRefusal::NoDataDir));
    assert!(store.runs().is_err(), "no data directory isn't no runs");
}

/// Catches a store kept on FAT, whose syncs and locks a run can't rely on (pinned 7).
#[test]
fn a_store_on_fat_refuses() {
    let fat = std::env::var_os("KIPPLE_TEST_FAT").map(PathBuf::from);
    assert!(
        fat.is_some() || std::env::var_os("KIPPLE_TEST_MOUNTS_REQUIRED").is_none(),
        "KIPPLE_TEST_FAT must name a prepared mount"
    );
    let Some(fat) = fat else {
        return;
    };
    let temp = TempDir::new_in(&fat).unwrap();

    let refused = store(temp.path()).lock(&CancelToken::new());

    assert!(
        matches!(
            refused,
            Err(StoreRefusal::FileSystem(Unsupported::NoPosixDelete))
        ),
        "{refused:?}"
    );
    assert!(
        !present(&temp.path().join("runs/.lock")),
        "nothing is locked there"
    );
}
