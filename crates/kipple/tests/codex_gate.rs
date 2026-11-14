//! The gate of Codex's removing rules: `builtin.codex.legacy-tui-log`,
//! `builtin.codex.plugin-catalog-cache` and `builtin.codex.stale-releases` on the real
//! probe, the real locks and the real executor, over a store whose `selection-history` was
//! written from a real `lstat`. Everything is below a temp dir
//! and no process is looked at, so it runs on every OS with the rest of the tests: the
//! adapters' own tests script the stamp and never remove anything.
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]

#[path = "../../kipple-adapters/tests/fixtures/tree.rs"]
mod tree;

#[path = "../../kipple-adapters/tests/fixtures/codex/demo.rs"]
mod codex_demo;
#[path = "../../kipple-adapters/tests/fixtures/codex/v0_165_0.rs"]
mod codex_v0_165_0;
#[path = "../../kipple-adapters/tests/fixtures/codex/v0_166_0.rs"]
mod codex_v0_166_0;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use kipple_adapters::builtin;
use kipple_core::{
    ApplyPorts, CancelToken, CleanRequest, Clock, Confirmed, DirEntryMeta, Ending, Engine,
    EntryMeta, EventChannel, Finding, Finished, FsProbe, GroupGap, HeldPath, IntegrationId,
    Inventory, ItemOutcome, Judge, Lineage, LockGap, Method, Methods, MutationPorts, Observer,
    PlanMode, Platform, Policy, Ports, ProbeError, ProcessOutcome, ProcessOwner, ProcessProbe,
    ProcessRecord, ProcessSnapshot, Protections, ReleaseFacts, ReleaseRole, ReleaseState,
    ResolvedRoot, RootSource, Roots, Rule, ScanReport, ScanRequest, Selection, Session, Settlement,
    Skip, StoreLocks as _, Subject, SymbolicRoot, Unavailable, event_channel, prepare_clean,
    run_prepared,
};
use kipple_platform::{
    ConfinedFs, ConfinedTrash, Environment, FileLocks, FileRunStore, GitQuery, NativeCommands,
    NoFollowFs, OsNames, PLATFORM, Walker,
};
use tempfile::TempDir;
use tree::{FixtureTree, Holds};

// The demo's fixtures hold what other tests use too.
const _: () = {
    let _ = codex_v0_166_0::windows_release;
    let _ = FixtureTree::dir;
    let _ = FixtureTree::modified;
};

const LOG_RULE: &str = "builtin.codex.legacy-tui-log";
const CATALOG_RULE: &str = "builtin.codex.plugin-catalog-cache";
const REPORTED_ONLY: &str = "builtin.codex.store-inventory";

const LOG: &str = "log/codex-tui.log";
const LOGIN_LOG: &str = "log/codex-login.log";
const CATALOG_DIR: &str = "cache/remote_plugin_catalog";
const CATALOG: &str = "cache/remote_plugin_catalog/0123456789abcdef.json";
const OTHER_CATALOG: &str = "cache/remote_plugin_catalog/fedcba9876543210.json";

/// The store's component the demo fills, relative to the temp dir.
const STORE: &str = "home/.codex/packages/standalone";

/// 2100-01-01: every file here is old by then, as a real one must be to be anything but
/// recent, and it takes no change to what the walker reads of them.
#[derive(Debug)]
struct Long;

impl Clock for Long {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_hours(1_139_568)
    }
}

/// What runs: a Codex of the user from each program, and nothing else, since what the
/// host's own processes are is no business of a test.
#[derive(Debug)]
struct Runs(Vec<PathBuf>);

impl ProcessProbe for Runs {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        let mut processes = Vec::new();
        for (pid, program) in (3000..).zip(&self.0) {
            let identity = NoFollowFs.metadata(program)?.identity.unwrap();
            let outcome = ProcessOutcome::Identified {
                path: program.clone(),
                identity,
                deleted: false,
            };
            processes.push(ProcessRecord {
                owner: ProcessOwner::CurrentUser,
                argv: Ok(vec!["codex".into()]),
                env_paths: Ok(Vec::new()),
                mapped_files: Ok(Vec::new()),
                ..ProcessRecord::new(pid, outcome)
            });
        }
        Ok(ProcessSnapshot {
            processes,
            inventory: Inventory::Complete,
            observer: Observer::PLAIN,
        })
    }
}

/// The real probe on an OS with no stamp for a link, as Windows is.
#[derive(Debug)]
struct NoStamp;

impl FsProbe for NoStamp {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        NoFollowFs.metadata(path)
    }

    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        NoFollowFs.read_dir(dir)
    }

    fn open_dir(&self, dir: &Path) -> Result<(), ProbeError> {
        NoFollowFs.open_dir(dir)
    }

    fn read_file(&self, path: &Path, limit: u64) -> Result<Vec<u8>, ProbeError> {
        NoFollowFs.read_file(path, limit)
    }

    fn read_link(&self, path: &Path) -> Result<PathBuf, ProbeError> {
        NoFollowFs.read_link(path)
    }

    fn link_stamp(&self, path: &Path) -> Result<String, ProbeError> {
        Err(ProbeError::io(path, io::ErrorKind::Unsupported.into()))
    }
}

/// What the OS records of `link`, and a line that can't match on an OS that records nothing,
/// where upstream writes a USN `std` can't read.
fn stamp(link: &Path) -> io::Result<String> {
    match FixtureTree::link_stamp(link) {
        Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok("no stamp".to_owned()),
        stamp => stamp,
    }
}

/// The demo's Codex home with a neighbour for each file, in a temp dir.
struct Gate {
    temp: TempDir,
    /// The programs, relative to the home, a Codex of the user runs from.
    running: Vec<String>,
}

impl Gate {
    fn new() -> Self {
        Self::with(|_| {})
    }

    /// The demo's home with what `add` adds to it.
    fn with(add: impl FnOnce(&mut FixtureTree)) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let mut tree = FixtureTree::new();
        codex_demo::demo(&mut tree);
        tree.file(&format!("{}/{LOGIN_LOG}", codex_demo::HOME), b"login\n")
            .file(&format!("{}/{OTHER_CATALOG}", codex_demo::HOME), b"{}");
        add(&mut tree);
        tree.build_in(temp.path()).unwrap();
        codex_demo::selection_history(temp.path(), stamp).unwrap();
        Self {
            temp,
            running: Vec::new(),
        }
    }

    /// The store of `docs/11-delivery-plan.md` M4's Verify list: five standalone releases,
    /// 0.166.0 selected after 0.165.0, and a Codex running from 0.163.0.
    fn five_releases() -> Self {
        let mut gate = Self::with(|tree| {
            for version in ["0.162.1", "0.163.0"] {
                let dir = format!("{}/{}", codex_demo::HOME, release_of(version));
                codex_v0_165_0::release(tree, &dir, version, TARGET);
            }
        });
        gate.running
            .push(format!("{}/bin/codex", release_of("0.163.0")));
        gate
    }

    /// An engine of `rules` over `fs`, seeing what runs.
    fn engine(&self, fs: Box<dyn FsProbe>, rules: Vec<Rule>) -> Engine {
        let running = self.running.iter().map(|path| self.home().join(path));
        engine(fs, rules, Box::new(Runs(running.collect())))
    }

    /// The names of the store's releases left.
    fn releases_left(&self) -> BTreeSet<String> {
        let releases = self.home().join("packages/standalone/releases");
        fs::read_dir(releases)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn home(&self) -> PathBuf {
        self.temp.path().join(codex_demo::HOME)
    }

    fn roots(&self) -> Roots {
        let mut roots = Roots::default();
        let home = self.temp.path().join("home");
        let store = self.home().join("packages");
        for (root, path) in [
            (SymbolicRoot::Home, home),
            (SymbolicRoot::CodexHome, self.home()),
            (SymbolicRoot::CodexDefaultHome, self.home()),
            (SymbolicRoot::CodexReleaseStore, store),
        ] {
            roots.insert(root, Ok(ResolvedRoot::new(root, path, RootSource::Fixed)));
        }
        roots
    }

    fn request(&self) -> ScanRequest {
        ScanRequest {
            roots: self.roots(),
            lens: None,
            exclude: Vec::new(),
        }
    }

    /// What a scan of `rules` through `fs` finds now.
    fn scan(&self, fs: Box<dyn FsProbe>, rules: Vec<Rule>) -> ScanReport {
        let cancel = CancelToken::new();
        let EventChannel { tx, rx } = event_channel(4096, &cancel);
        let report = self.engine(fs, rules).scan(&self.request(), &tx, &cancel);
        drop(rx);
        report
    }

    /// Takes `finding` of `report` through a plan a person saw, the real executor and a run
    /// stored below the temp dir.
    fn delete(&self, report: &ScanReport, finding: &Finding) -> Finished {
        self.delete_all(report, &[finding])
    }

    /// Takes `findings` of `report` through one run, as [`delete`](Self::delete) does one.
    fn delete_all(&self, report: &ScanReport, findings: &[&Finding]) -> Finished {
        let rules = rules_of(PLATFORM);
        let engine = self.engine(Box::new(NoFollowFs), rules.clone());
        let request = self.request();
        let rescan = engine.rescan(&request);
        let env = Environment::from_vars(Vec::new());
        let data = self.temp.path().join("kipple");
        let git = GitQuery::new(&env);
        let mutation = MutationPorts {
            freezer: &ConfinedFs,
            executor: &ConfinedFs,
            trash: &ConfinedTrash::new(None, Arc::new(Long)),
            tracked: &git,
            runner: &NativeCommands::new(&env, Some(data.clone())),
            locks: &FileLocks,
            worktrees: &git,
        };
        let policy = Policy::default();
        let methods = Methods {
            command_line: Some(Method::Delete),
            rules: BTreeMap::new(),
            config: None,
        };
        let protections =
            Protections::of_host(&request.roots, Some(&data), &builtin::descriptors(), &held);
        let session = Session {
            judge: Judge {
                rules: &rules,
                policy: &policy,
                methods: &methods,
                protections: &protections,
            },
            store: &FileRunStore::new(Some(data), Arc::new(Long)),
            ports: ApplyPorts {
                rescan: &rescan,
                mutation: &mutation,
            },
            codec: &OsNames,
        };
        let cancel = CancelToken::new();
        let selection = Selection(findings.iter().map(|finding| finding.id).collect());
        let clean = CleanRequest {
            scan: report,
            selection: &selection,
        };
        let prepared = prepare_clean(&clean, &session, PlanMode::Review, &cancel);
        run_prepared(&prepared, Confirmed::Person, &session, &mut (), &cancel)
    }

    /// Replaces `current` with a new link to the same release, as an update that selects
    /// it again does: a temporary link renamed over the old one.
    #[expect(
        clippy::disallowed_methods,
        reason = "the test plays Codex's installer"
    )]
    fn select_again(&self) {
        let root = self.temp.path();
        let selected = fs::read_link(root.join(STORE).join("current")).unwrap();
        let fresh = format!("{STORE}/.current.fresh");
        let selected = selected.strip_prefix(root).unwrap().to_str().unwrap();
        let mut tree = FixtureTree::new();
        tree.link(&fresh, selected, Holds::AbsoluteInTree);
        tree.build_in(root).unwrap();
        fs::rename(root.join(fresh), root.join(STORE).join("current")).unwrap();
    }
}

/// `path` held as the config holds a path: by its name and what is there and above it.
fn held(path: &Path) -> HeldPath {
    let identity = |at| NoFollowFs.metadata(at).ok()?.identity;
    HeldPath {
        forms: vec![path.to_owned()],
        lineage: Lineage(path.ancestors().map(identity).collect()),
        note: None,
    }
}

/// Codex's rules for `platform`.
fn rules_of(platform: Platform) -> Vec<Rule> {
    builtin::packs(&semver::Version::new(0, 0, 0))
        .unwrap()
        .into_iter()
        .flat_map(|pack| pack.rules)
        .filter(|rule| rule.integration == IntegrationId("codex"))
        .filter(|rule| rule.platforms.contains(&platform))
        .collect()
}

fn engine(fs: Box<dyn FsProbe>, rules: Vec<Rule>, processes: Box<dyn ProcessProbe>) -> Engine {
    let integrations = builtin::integrations()
        .into_iter()
        .filter(|integration| integration.descriptor().id == IntegrationId("codex"))
        .collect();
    let env = Environment::from_vars(Vec::new());
    let ports = Ports {
        fs,
        sizer: Box::new(Walker::new().unwrap()),
        processes,
        clock: Box::new(Long),
        tracked: Box::new(GitQuery::new(&env)),
        tools: Box::new(Unavailable),
        worktrees: Box::new(Unavailable),
    };
    Engine::new(ports, integrations, rules).unwrap()
}

/// The finding at `path`, relative to the home of `gate`.
fn at<'report>(gate: &Gate, report: &'report ScanReport, path: &str) -> &'report Finding {
    let path = gate.home().join(path);
    let found = report.findings.iter().find(|finding| finding.path == path);
    found.unwrap()
}

/// What each release of `report` says of itself, by the release's version.
fn releases(report: &ScanReport) -> BTreeMap<String, ReleaseFacts> {
    let facts = |finding: &Finding| {
        let Subject::Release(release) = &finding.facts.as_ref()?.supplied.subject else {
            return None;
        };
        Some((release.version.0.clone(), release.clone()))
    };
    report.findings.iter().filter_map(facts).collect()
}

/// Whether `path` still names something.
fn present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Catches a removing rule that a real probe doesn't reach or a real executor can't finish:
/// the scan must hand over the file itself, and the removal take it and nothing beside it.
/// The rules are Linux's, so nothing runs on the others.
#[test]
fn each_rule_removes_its_file_and_nothing_beside_it() {
    if PLATFORM != Platform::Linux {
        return;
    }
    for (rule, file, sibling) in [
        (LOG_RULE, LOG, LOGIN_LOG),
        (CATALOG_RULE, CATALOG, OTHER_CATALOG),
    ] {
        let gate = Gate::new();
        let report = gate.scan(Box::new(NoFollowFs), rules_of(PLATFORM));
        let finding = at(&gate, &report, file);
        assert_eq!(finding.rule.0, rule, "{file}");

        let finished = gate.delete(&report, finding);

        assert_eq!(finished.ending, Ending::Done, "{file}: {finished:#?}");
        assert!(!present(&gate.home().join(file)), "{file} is gone");
        assert!(present(&gate.home().join(sibling)), "{sibling} stays");
    }
}

/// Catches either rule reaching an OS nobody verified it on: there the same entries are
/// reported and nothing proposes to remove them.
#[test]
fn the_other_oses_only_report_them() {
    for platform in [Platform::Macos, Platform::Windows] {
        let gate = Gate::new();

        let report = gate.scan(Box::new(NoFollowFs), rules_of(platform));

        for path in [LOG, CATALOG_DIR] {
            assert_eq!(at(&gate, &report, path).rule.0, REPORTED_ONLY, "{path}");
        }
        let rules: Vec<&str> = report.findings.iter().map(|f| f.rule.0.as_str()).collect();
        assert!(
            !rules.contains(&LOG_RULE) && !rules.contains(&CATALOG_RULE),
            "expected neither rule on {platform:?}, got {rules:?}"
        );
    }
}

/// Catches kipple's stamp and upstream's parting ways: `selection-history` written from a
/// real `lstat` reads as fresh through the real probe, and `current` made again at the same
/// target doesn't, though its target is the same, since upstream compares the link itself.
#[test]
fn a_current_made_again_at_the_same_target_is_stale() {
    let gate = Gate::new();
    let scan = || releases(&gate.scan(Box::new(NoFollowFs), rules_of(Platform::Linux)));
    let selected = codex_v0_166_0::VERSION;

    let before = scan();
    if FixtureTree::link_stamp(&gate.temp.path().join(STORE).join("current")).is_err() {
        assert_eq!(
            before[selected].state,
            ReleaseState::Blocked(GroupGap::Unreadable)
        );
        return;
    }
    assert_eq!(before[selected].state, ReleaseState::Sound);
    assert_eq!(before[selected].role, ReleaseRole::Active);

    gate.select_again();

    assert_eq!(
        scan()[selected].state,
        ReleaseState::Blocked(GroupGap::HistoryStale)
    );
}

/// Catches an OS with no stamp for a link passing a history it can't check: nothing of the
/// store can be settled there, whichever release it is.
#[test]
fn a_stamp_the_os_cant_give_blocks_every_group() {
    let gate = Gate::new();

    let report = gate.scan(Box::new(NoStamp), rules_of(Platform::Linux));

    let releases = releases(&report);
    assert_eq!(releases.len(), 3, "{report:#?}");
    for (version, release) in releases {
        assert_eq!(
            release.state,
            ReleaseState::Blocked(GroupGap::Unreadable),
            "{version}"
        );
    }
}

/// The release the demo's store holds beside its selected and rollback ones, relative to
/// the home.
const STALE_RELEASE: &str = "packages/standalone/releases/0.164.0-x86_64-unknown-linux-musl";

/// Catches the release rule failing where only real files show it: the release retention
/// lets go is removed under Codex's own install lock by the real executor, the selected
/// and rollback releases stay, and while another holder has the lock nothing is removed.
#[test]
fn the_stale_release_goes_under_codexs_lock() {
    if PLATFORM != Platform::Linux {
        return;
    }
    let gate = Gate::new();
    let report = gate.scan(Box::new(NoFollowFs), rules_of(PLATFORM));
    let finding = at(&gate, &report, STALE_RELEASE);
    assert_eq!(finding.rule.0, "builtin.codex.stale-releases");
    let lock = gate.temp.path().join(STORE).join("install.lock");
    let installer = FileLocks.try_hold(std::slice::from_ref(&lock)).unwrap();

    let busy = gate.delete(&report, finding);

    assert_eq!(
        busy.receipt.items[0].outcome,
        ItemOutcome::Skipped(Skip::InstallerBusy(LockGap::Held))
    );
    assert!(present(&gate.home().join(STALE_RELEASE)), "kept while held");
    drop(installer);

    let finished = gate.delete(&report, finding);

    assert_eq!(finished.ending, Ending::Done, "{finished:#?}");
    assert!(
        !present(&gate.home().join(STALE_RELEASE)),
        "the stale release is gone"
    );
    assert_eq!(
        gate.releases_left(),
        BTreeSet::from([name("0.165.0"), name("0.166.0")])
    );
}

/// Catches a run removing at most one release of a component: the release a run reaches
/// second must settle among what is there once the first is gone, so the 5+3 store's two
/// stale standalone releases both go in one run, and the running, rollback and selected
/// ones stay.
#[test]
fn two_stale_releases_of_one_component_go_in_one_run() {
    if PLATFORM != Platform::Linux {
        return;
    }
    let gate = Gate::five_releases();
    let report = gate.scan(Box::new(NoFollowFs), rules_of(PLATFORM));
    let stale = [release_of("0.162.1"), release_of("0.164.0")];
    let findings: Vec<&Finding> = stale.iter().map(|path| at(&gate, &report, path)).collect();
    let candidates: Vec<&PathBuf> = report
        .findings
        .iter()
        .filter(|finding| {
            let settlement = finding.facts.as_ref().map(|facts| facts.settlement);
            settlement == Some(Settlement::Candidate)
        })
        .map(|finding| &finding.path)
        .collect();
    let expected: Vec<&PathBuf> = findings.iter().map(|finding| &finding.path).collect();
    assert_eq!(
        candidates, expected,
        "the scan offers the two stale releases only"
    );

    let finished = gate.delete_all(&report, &findings);

    assert_eq!(finished.ending, Ending::Done, "{finished:#?}");
    let outcomes = finished.receipt.items.iter().map(|item| &item.outcome);
    for outcome in outcomes {
        assert!(
            matches!(outcome, ItemOutcome::Done(_)),
            "expected each removed: {outcome:?}"
        );
    }
    assert_eq!(
        gate.releases_left(),
        BTreeSet::from([name("0.163.0"), name("0.165.0"), name("0.166.0")])
    );
}

const TARGET: &str = "x86_64-unknown-linux-musl";

/// The release `version` of the store's own target.
fn name(version: &str) -> String {
    format!("{version}-{TARGET}")
}

/// The release `version` of the standalone store, relative to the home.
fn release_of(version: &str) -> String {
    format!("packages/standalone/releases/{}", name(version))
}
