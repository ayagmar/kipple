//! Releases through the apply loop (spec M4 pinned 11 to 14): the store's own locks taken
//! before anything of a release is read again, and a release judged by what is settled
//! afresh at apply. The locks themselves, on real files, are the platform's
//! (`crates/kipple-platform/tests/store_lock.rs`), and a release settled afresh from a real
//! store is Codex's (`crates/kipple-adapters/tests/codex.rs`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use kipple_core::{
    ApplyPorts, ApplyRequest, Authorization, CancelToken, Finding, HeldLocks, ItemOutcome, LockGap,
    Method, MutationPorts, Now, PlanMode, Proposal, Recorders, ReleaseFacts, ReleaseGroup,
    ReleaseRole, ReleaseState, ReleaseVersion, Rescan, Retained, ReviewedPlan, ScanReport,
    Settlement, Skip, SpaceEstimate, Status, StoreLocks, Subject, apply,
};

use super::world::{Log, Rescanned, World};
use super::{Config, TARGET, home, plan_all, ports, project, rescanned, scan_of, world_of};

/// A release retention let go, naming `locks`, and the world holding it.
fn release(locks: &[&str]) -> (ScanReport, World) {
    let (item, target) = project(0, 100, TARGET);
    let world = world_of(&[(item.clone(), target)]);
    let mut scan = scan_of(&[item]);
    let facts = scan.findings[0].facts.as_mut().unwrap();
    facts.proposal = Proposal::RemoveRelease(vec![Method::Delete]);
    facts.settlement = Settlement::Candidate;
    facts.supplied.subject = Subject::Release(ReleaseFacts {
        group: ReleaseGroup {
            component: "standalone".to_owned(),
            target: "x86_64-unknown-linux-musl".to_owned(),
        },
        version: ReleaseVersion("0.164.0".to_owned()),
        role: ReleaseRole::Installed,
        files: Vec::new(),
        locks: locks.iter().map(|lock| home(lock)).collect(),
        state: ReleaseState::Sound,
    });
    (scan, world)
}

const INSTALL_LOCK: &str = ".codex/packages/standalone/install.lock";
const DAEMON_LOCK: &str = ".codex/packages/app-server-daemon/daemon.lock";

/// A store's locks, each held elsewhere or missing as the test says, keeping what they were
/// asked for and saying when they are taken and let go.
#[derive(Debug)]
struct StoreLocksAt {
    gaps: Vec<(PathBuf, LockGap)>,
    asked: Mutex<Vec<PathBuf>>,
    log: Log,
}

impl StoreLocksAt {
    fn new(gaps: &[(&str, LockGap)], log: &Log) -> Self {
        Self {
            gaps: gaps.iter().map(|(lock, gap)| (home(lock), *gap)).collect(),
            asked: Mutex::default(),
            log: Arc::clone(log),
        }
    }

    fn asked(&self) -> Vec<PathBuf> {
        self.asked.lock().unwrap().clone()
    }
}

/// Says `released` when it is dropped.
#[derive(Debug)]
struct Release(Log);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("released");
    }
}

impl StoreLocks for StoreLocksAt {
    fn try_hold(&self, paths: &[PathBuf]) -> Result<HeldLocks, LockGap> {
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(paths);
        if paths.is_empty() {
            return Err(LockGap::Missing);
        }
        let gap = |path: &PathBuf| {
            let held = self.gaps.iter().find(|(lock, _)| lock == path);
            held.map(|(_, gap)| *gap)
        };
        if let Some(gap) = paths.iter().find_map(gap) {
            return Err(gap);
        }
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("locks");
        Ok(HeldLocks::new(Release(Arc::clone(&self.log))))
    }
}

/// Apply's rescan, settling the release as `settled` says it is now.
#[derive(Debug)]
struct SettledNow {
    inner: Rescanned,
    settled: Option<Settlement>,
}

impl Rescan for SettledNow {
    fn scan(&self, cancel: &CancelToken) -> ScanReport {
        self.inner.scan(cancel)
    }

    fn now(&self, scan: &ScanReport, finding: &Finding, cancel: &CancelToken) -> Result<Now, Skip> {
        let mut now = self.inner.now(scan, finding, cancel)?;
        if let Some(settled) = self.settled {
            now.settlement = settled;
        }
        Ok(now)
    }

    fn size(&self, path: &Path, cancel: &CancelToken) -> Option<SpaceEstimate> {
        self.inner.size(path, cancel)
    }
}

/// How a run of `scan`'s release ended, planned as `mode` and run under `locks`, with apply
/// settling it as `settled`.
struct Run<'test> {
    scan: &'test ScanReport,
    world: &'test World,
    locks: &'test StoreLocksAt,
    settled: Option<Settlement>,
    mode: PlanMode,
}

impl Run<'_> {
    fn outcome(&self) -> ItemOutcome {
        let planned = plan_all(self.scan, &Config::new(), self.world, self.mode);
        let plan = planned.plan;
        assert_eq!(plan.entries().len(), 1, "{:?}", planned.refused);
        let authorization = match self.mode {
            PlanMode::SafeOnly => Authorization::SafeOnly,
            PlanMode::Review | PlanMode::Safe | PlanMode::SafeFiles => {
                Authorization::Reviewed(ReviewedPlan::for_core_tests(&plan))
            }
        };
        self.world.state().log = Some(Arc::clone(&self.locks.log));
        let rescan = SettledNow {
            inner: Rescanned {
                log: Some(Arc::clone(&self.locks.log)),
                ..rescanned(self.scan)
            },
            settled: self.settled,
        };
        let mutation = MutationPorts {
            locks: self.locks,
            ..ports(self.world)
        };
        let ports = ApplyPorts {
            rescan: &rescan,
            mutation: &mutation,
        };
        let mut journal = self.world.recorder();
        let to = Recorders {
            journal: &mut journal,
            events: &mut (),
        };
        let request = ApplyRequest {
            plan: &plan,
            authorization,
        };
        let config = Config::new();
        let receipt = apply(&request, &config.judge(), &ports, to, &CancelToken::new());
        receipt.items[0].outcome.clone()
    }
}

const fn run<'test>(
    scan: &'test ScanReport,
    world: &'test World,
    locks: &'test StoreLocksAt,
) -> Run<'test> {
    Run {
        scan,
        world,
        locks,
        settled: None,
        mode: PlanMode::Review,
    }
}

/// Catches a release removed while Codex's installer holds its store: nothing of it is
/// read again or touched (pinned 13).
#[test]
fn held_install_lock_skips() {
    let (scan, world) = release(&[INSTALL_LOCK]);
    let log = Log::default();
    let locks = StoreLocksAt::new(&[(INSTALL_LOCK, LockGap::Held)], &log);

    let outcome = run(&scan, &world, &locks).outcome();

    assert_eq!(
        outcome,
        ItemOutcome::Skipped(Skip::InstallerBusy(LockGap::Held))
    );
    assert_eq!(*log.lock().unwrap(), [] as [&str; 0], "nothing went on");
}

/// Catches a lock file that is gone read as a free store: it is never created, so its
/// store isn't the one the scan read.
#[test]
fn missing_lock_file_skips() {
    let (scan, world) = release(&[INSTALL_LOCK]);
    let log = Log::default();
    let locks = StoreLocksAt::new(&[(INSTALL_LOCK, LockGap::Missing)], &log);

    let outcome = run(&scan, &world, &locks).outcome();

    assert_eq!(
        outcome,
        ItemOutcome::Skipped(Skip::InstallerBusy(LockGap::Missing))
    );
}

/// Catches the daemon's store removed under its install lock alone, when its updater takes
/// `daemon.lock` too.
#[test]
fn daemon_store_needs_both_locks() {
    let (scan, world) = release(&[INSTALL_LOCK, DAEMON_LOCK]);
    let log = Log::default();
    let locks = StoreLocksAt::new(&[], &log);

    let outcome = run(&scan, &world, &locks).outcome();

    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    assert_eq!(locks.asked(), [home(INSTALL_LOCK), home(DAEMON_LOCK)]);
    assert_eq!(
        *log.lock().unwrap(),
        ["locks", "freeze", "now", "ran", "released"],
        "both taken first, held through the removal"
    );
}

/// Catches a daemon store removed while its updater holds `daemon.lock`, though the
/// install lock is free.
#[test]
fn held_daemon_lock_skips() {
    let (scan, world) = release(&[INSTALL_LOCK, DAEMON_LOCK]);
    let log = Log::default();
    let locks = StoreLocksAt::new(&[(DAEMON_LOCK, LockGap::Held)], &log);

    let outcome = run(&scan, &world, &locks).outcome();

    assert_eq!(
        outcome,
        ItemOutcome::Skipped(Skip::InstallerBusy(LockGap::Held))
    );
    assert_eq!(*log.lock().unwrap(), [] as [&str; 0], "nothing went on");
}

/// Catches `--only builtin.codex.stale-releases --yes` taking a path around the locks or
/// the settle at apply that a reviewed run takes (pinned 11, `docs/04-safety-model.md`
/// §2a).
#[test]
fn only_yes_runs_a_release_through_the_same_locks() {
    let (scan, world) = release(&[INSTALL_LOCK]);
    let log = Log::default();
    let locks = StoreLocksAt::new(&[], &log);
    let safe_only = Run {
        mode: PlanMode::SafeOnly,
        ..run(&scan, &world, &locks)
    };

    let outcome = safe_only.outcome();

    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    assert_eq!(locks.asked(), [home(INSTALL_LOCK)]);
    assert_eq!(
        *log.lock().unwrap(),
        ["locks", "freeze", "now", "ran", "released"]
    );

    let (busy_scan, busy_world) = release(&[INSTALL_LOCK]);
    let busy_log = Log::default();
    let held = StoreLocksAt::new(&[(INSTALL_LOCK, LockGap::Held)], &busy_log);
    let busy = Run {
        mode: PlanMode::SafeOnly,
        ..run(&busy_scan, &busy_world, &held)
    };
    assert_eq!(
        busy.outcome(),
        ItemOutcome::Skipped(Skip::InstallerBusy(LockGap::Held))
    );
}

/// Catches apply removing what a settle at apply keeps: a release the scan let go that is
/// now active, rolled back to or unsettled stays, whatever the plan said.
#[test]
fn a_release_kept_at_apply_stays() {
    for settled in [
        Settlement::Retained(Retained::Active),
        Settlement::Retained(Retained::Previous),
        Settlement::Unsettled,
    ] {
        let (scan, world) = release(&[INSTALL_LOCK]);
        let log = Log::default();
        let locks = StoreLocksAt::new(&[], &log);
        let changed = Run {
            settled: Some(settled),
            ..run(&scan, &world, &locks)
        };

        let outcome = changed.outcome();

        assert!(
            matches!(outcome, ItemOutcome::Skipped(Skip::Protected(_))),
            "expected {settled:?} to keep it, got {outcome:?}"
        );
        assert_eq!(world.state().ran, Vec::new(), "{settled:?}: nothing ran");
    }
}
