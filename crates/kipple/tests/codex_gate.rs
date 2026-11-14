//! The gate of Codex's two removing rules: `builtin.codex.legacy-tui-log` and
//! `builtin.codex.plugin-catalog-cache` on the real probe and the real executor, over a store
//! whose `selection-history` was written from a real `lstat`. Everything is below a temp dir
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
    Inventory, Judge, Lineage, Method, Methods, MutationPorts, Observer, PlanMode, Platform,
    Policy, Ports, ProbeError, ProcessProbe, ProcessSnapshot, Protections, ReleaseFacts,
    ReleaseRole, ReleaseState, ResolvedRoot, RootSource, Roots, Rule, ScanReport, ScanRequest,
    Selection, Session, Subject, SymbolicRoot, Unavailable, event_channel, prepare_clean,
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

/// Nothing runs: what the host's own processes are is no business of a test.
#[derive(Debug)]
struct Idle;

impl ProcessProbe for Idle {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        Ok(ProcessSnapshot {
            processes: Vec::new(),
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
}

impl Gate {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let mut tree = FixtureTree::new();
        codex_demo::demo(&mut tree);
        tree.file(&format!("{}/{LOGIN_LOG}", codex_demo::HOME), b"login\n")
            .file(&format!("{}/{OTHER_CATALOG}", codex_demo::HOME), b"{}");
        tree.build_in(temp.path()).unwrap();
        codex_demo::selection_history(temp.path(), stamp).unwrap();
        Self { temp }
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
        let report = engine(fs, rules).scan(&self.request(), &tx, &cancel);
        drop(rx);
        report
    }

    /// Takes `finding` of `report` through a plan a person saw, the real executor and a run
    /// stored below the temp dir.
    fn delete(&self, report: &ScanReport, finding: &Finding) -> Finished {
        let rules = rules_of(PLATFORM);
        let engine = engine(Box::new(NoFollowFs), rules.clone());
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
        let selection = Selection(BTreeSet::from([finding.id]));
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

fn engine(fs: Box<dyn FsProbe>, rules: Vec<Rule>) -> Engine {
    let integrations = builtin::integrations()
        .into_iter()
        .filter(|integration| integration.descriptor().id == IntegrationId("codex"))
        .collect();
    let env = Environment::from_vars(Vec::new());
    let ports = Ports {
        fs,
        sizer: Box::new(Walker::new().unwrap()),
        processes: Box::new(Idle),
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
