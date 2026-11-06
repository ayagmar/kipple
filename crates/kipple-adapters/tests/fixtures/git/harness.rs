//! A scan of a fixture tree with git scripted: the tree's files are real, and what git says
//! about them comes from [`Script`]. `scan.rs` answers that no worktree can be asked about,
//! so classification needs its own ports.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::tree::FixtureTree;
use kipple_adapters::builtin;
use kipple_core::{
    Anchor, Apparent, CancelToken, Checkout, Clock, Completeness, ConfigGate, DirEntryMeta, Engine,
    EntryKind, EntryMeta, EventChannel, FactValue, Finding, FrozenTree, FsProbe, GitGap, Inventory,
    Measurement, NodeId, ObjectId, Observer, Ports, ProbeError, ProcessProbe, ProcessSnapshot,
    Registration, RegistrationState, ResolvedRoot, RootSource, Roots, ScanReport, ScanRequest,
    Sizer, SpaceEstimate, SymbolicRoot, TrackedQuery, Unavailable, WorktreePlace, WorktreeQuery,
    WorktreeState, event_channel,
};

/// The real file system, read without following links.
#[derive(Debug)]
struct TempFs;

fn kind(file_type: fs::FileType) -> EntryKind {
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

impl FsProbe for TempFs {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        let meta = fs::symlink_metadata(path).map_err(|e| ProbeError::io(path, e))?;
        Ok(EntryMeta {
            kind: kind(meta.file_type()),
            identity: None,
        })
    }

    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        let read = || -> io::Result<Vec<DirEntryMeta>> {
            fs::read_dir(dir)?
                .map(|entry| {
                    let entry = entry?;
                    Ok(DirEntryMeta {
                        name: entry.file_name(),
                        kind: kind(entry.file_type()?),
                    })
                })
                .collect()
        };
        read().map_err(|e| ProbeError::io(dir, e))
    }

    fn open_dir(&self, dir: &Path) -> Result<(), ProbeError> {
        fs::read_dir(dir)
            .map(drop)
            .map_err(|e| ProbeError::io(dir, e))
    }

    fn read_file(&self, path: &Path, limit: u64) -> Result<Vec<u8>, ProbeError> {
        let mut bytes = Vec::new();
        fs::File::open(path)
            .and_then(|file| file.take(limit).read_to_end(&mut bytes))
            .map_err(|e| ProbeError::io(path, e))?;
        Ok(bytes)
    }
}

/// The first directory at or below `item` holding a `.git`, as the platform's sizer reports
/// it, skipping `item` itself when asked for what is strictly below it.
fn holding_git(item: &Path, below: Below) -> Option<PathBuf> {
    if below == Below::AtOrBelow && fs::symlink_metadata(item.join(".git")).is_ok() {
        return Some(item.to_owned());
    }
    let mut dirs: Vec<_> = fs::read_dir(item)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|dir| !dir.ends_with(".git"))
        .collect();
    dirs.sort();
    dirs.iter()
        .find_map(|dir| holding_git(dir, Below::AtOrBelow))
}

/// Where [`holding_git`] starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Below {
    AtOrBelow,
    Strictly,
}

/// Sizes nothing, and finds each repository inside an item as the platform does.
#[derive(Debug)]
struct NoSizes;

impl Sizer for NoSizes {
    fn size(&self, path: &Path, _: &CancelToken) -> Result<Measurement, ProbeError> {
        let size = SpaceEstimate {
            apparent: Apparent(0),
            allocated: None,
            unique_reclaim: None,
            completeness: Completeness::Complete,
        };
        Ok(Measurement {
            size,
            modified: None,
            repository: holding_git(path, Below::AtOrBelow),
            nested_repository: holding_git(path, Below::Strictly),
        })
    }
}

#[derive(Debug)]
struct NoProcesses;

impl ProcessProbe for NoProcesses {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        Ok(ProcessSnapshot {
            processes: Vec::new(),
            inventory: Inventory::Complete,
            observer: Observer::PLAIN,
        })
    }
}

#[derive(Debug)]
struct Epoch;

impl Clock for Epoch {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
}

/// A repository in which git tracks nothing inside any item.
#[derive(Debug)]
struct NothingTracked;

impl TrackedQuery for NothingTracked {
    fn count(&self, _: &Path, _: &Path, _: &CancelToken) -> Result<u64, GitGap> {
        Ok(0)
    }

    fn entries(
        &self,
        _: &Path,
        _: &Anchor,
        _: &FrozenTree,
        _: &CancelToken,
    ) -> Result<Vec<NodeId>, GitGap> {
        Ok(Vec::new())
    }
}

/// A registration as `list` gives it: on a branch, unlocked.
pub(crate) fn registration(path: PathBuf) -> Registration {
    Registration {
        path,
        head: Some(ObjectId("a".repeat(40))),
        checkout: Checkout::Branch("wt".to_owned()),
        locked: None,
        prunable: false,
        bare: false,
    }
}

/// What `inspect` says of a clean worktree of the repository at `main`, nothing in the way.
pub(crate) fn clean(main: &Path, path: &Path) -> WorktreeState {
    let common_dir = main.join(".git");
    WorktreeState {
        place: Ok(WorktreePlace {
            git_dir: common_dir.join("worktrees/wt"),
            common_dir,
            top: path.to_owned(),
        }),
        registration: Ok(RegistrationState::RegisteredPresent),
        submodules: Ok(false),
        config_gate: Ok(ConfigGate::Clear),
        tracked_changes: Ok(0),
        untracked_files: Ok(0),
        ignored: Ok(Vec::new()),
        hidden_changes: Ok(0),
        operation_in_progress: Ok(false),
        per_worktree_refs: Ok(0),
        index_locked: Ok(false),
        head_reachable: Ok(true),
        witness: Err(GitGap::Failed),
    }
}

/// What git says: a repository's registrations, and each worktree as it is. A worktree with
/// no state of its own reads as [`clean`].
#[derive(Debug, Clone, Default)]
pub(crate) struct Script {
    repositories: Vec<Vec<Registration>>,
    states: BTreeMap<PathBuf, WorktreeState>,
    list_fails: Option<GitGap>,
}

impl Script {
    /// A script with one repository, its main worktree at `main`, listing `linked`.
    pub(crate) fn repository(main: &Path, linked: &[&Path]) -> Self {
        Self::default().with_repository(main, linked)
    }

    /// Adds a repository.
    pub(crate) fn with_repository(mut self, main: &Path, linked: &[&Path]) -> Self {
        let main = std::iter::once(main);
        let paths = main.chain(linked.iter().copied()).map(Path::to_owned);
        self.repositories.push(paths.map(registration).collect());
        self
    }

    /// Replaces the listed record of `path`.
    pub(crate) fn record(mut self, path: &Path, change: impl Fn(&mut Registration)) -> Self {
        self.repositories
            .iter_mut()
            .flatten()
            .filter(|record| record.path == path)
            .for_each(change);
        self
    }

    /// Says `state` of the worktree at `path`.
    pub(crate) fn state(mut self, path: &Path, state: WorktreeState) -> Self {
        self.states.insert(path.to_owned(), state);
        self
    }

    /// Makes every `list` fail with `gap`.
    pub(crate) const fn list_fails(mut self, gap: GitGap) -> Self {
        self.list_fails = Some(gap);
        self
    }
}

impl WorktreeQuery for Script {
    fn list(&self, repository: &Path, _: &CancelToken) -> Result<Vec<Registration>, GitGap> {
        if let Some(gap) = self.list_fails {
            return Err(gap);
        }
        self.repositories
            .iter()
            .find(|records| records.iter().any(|record| record.path == repository))
            .cloned()
            .ok_or(GitGap::Failed)
    }

    fn inspect(&self, registration: &Registration, _: &CancelToken) -> WorktreeState {
        let path = &registration.path;
        self.states.get(path).cloned().unwrap_or_else(|| {
            let main = self
                .repositories
                .iter()
                .find(|records| records.contains(registration))
                .and_then(|records| records.first());
            clean(main.map_or(path, |main| &main.path), path)
        })
    }
}

/// The directories an agent keeps worktrees in, below the tree.
const AGENT_LOCATIONS: [&str; 4] = [
    "home/.codex/worktrees",
    "home/.cursor/worktrees",
    "home/conductor/workspaces",
    "home/.herdr/worktrees",
];

/// Roots that only place other integrations' areas.
const OTHER_ROOTS: [(SymbolicRoot, &str); 3] = [
    (SymbolicRoot::Home, "home"),
    (SymbolicRoot::CodexHome, "home/.codex"),
    (SymbolicRoot::ClaudeConfig, "home/.claude"),
];

/// A tree built in a temp dir, ready to be scanned.
pub(crate) struct Built {
    temp: tempfile::TempDir,
    projects: &'static str,
}

/// Builds `tree` in a temp dir, its projects in `home/projects`.
pub(crate) fn built(tree: &FixtureTree) -> Built {
    built_from(|temp| {
        build(tree, temp);
    })
}

/// Like [`built`], for a tree whose links need the directory it is built in.
pub(crate) fn built_with(tree: impl FnOnce(&Path) -> FixtureTree) -> Built {
    built_from(|temp| build(&tree(temp), temp))
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn build(tree: &FixtureTree, temp: &Path) {
    tree.build_in(temp).unwrap();
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn built_from(fill: impl FnOnce(&Path)) -> Built {
    let temp = tempfile::tempdir().unwrap();
    fill(temp.path());
    Built {
        temp,
        projects: "home/projects",
    }
}

impl Built {
    /// Where the tree's `relative` path is.
    pub(crate) fn at(&self, relative: &str) -> PathBuf {
        self.temp.path().join(relative)
    }

    /// Makes `relative` the project root, for an area no default root reaches.
    pub(crate) const fn projects_at(mut self, relative: &'static str) -> Self {
        self.projects = relative;
        self
    }

    /// Each root, placed in the tree.
    pub(crate) fn roots(&self) -> Roots {
        let resolved = |root, relative: &str| {
            Ok(ResolvedRoot::new(
                root,
                self.at(relative),
                RootSource::Fixed,
            ))
        };
        let mut roots = Roots::default();
        for (root, relative) in OTHER_ROOTS {
            roots.insert(root, resolved(root, relative));
        }
        let projects = SymbolicRoot::ProjectRoots;
        roots.insert(projects, resolved(projects, self.projects));
        for relative in AGENT_LOCATIONS {
            let root = SymbolicRoot::GitAgentLocations;
            roots.add(root, resolved(root, relative));
        }
        roots
    }

    /// Scans the tree with every built-in integration and rule, git answering from `script`.
    #[expect(
        clippy::unwrap_used,
        reason = "a fixture that fails to build fails the test"
    )]
    pub(crate) fn scan(self, script: Script) -> Run {
        let packs = builtin::packs(&semver::Version::new(0, 0, 0)).unwrap();
        let rules = packs.into_iter().flat_map(|pack| pack.rules).collect();
        let ports = Ports {
            fs: Box::new(TempFs),
            sizer: Box::new(NoSizes),
            processes: Box::new(NoProcesses),
            clock: Box::new(Epoch),
            tracked: Box::new(NothingTracked),
            tools: Box::new(Unavailable),
            worktrees: Box::new(script),
        };
        let engine = Engine::new(ports, builtin::integrations(), rules).unwrap();
        let cancel = CancelToken::new();
        let EventChannel { tx: events, rx } = event_channel(4096, &cancel);
        let request = ScanRequest {
            roots: self.roots(),
            lens: None,
            exclude: Vec::new(),
        };
        let report = engine.scan(&request, &events, &cancel);
        drop(rx);
        Run {
            built: self,
            report,
        }
    }
}

/// A scan's report over the tree it read.
pub(crate) struct Run {
    /// The tree, which goes when this does.
    pub built: Built,
    /// What the scan found.
    pub report: ScanReport,
}

impl Run {
    /// Every finding of integration `id`.
    pub(crate) fn of(&self, id: &str) -> Vec<&Finding> {
        let findings = self.report.findings.iter();
        findings.filter(|f| f.integration.0 == id).collect()
    }

    /// Git's findings at `path`.
    pub(crate) fn at(&self, path: &Path) -> Vec<&Finding> {
        let found = self.of("git").into_iter();
        found.filter(|f| f.path == path).collect()
    }

    /// The one git finding at `path`.
    pub(crate) fn only(&self, path: &Path) -> &Finding {
        let found = self.at(path);
        assert_eq!(found.len(), 1, "one finding at {}", path.display());
        found[0]
    }

    /// The fact `name` of the git finding at `path`.
    #[expect(clippy::expect_used, reason = "a finding with no facts fails the test")]
    pub(crate) fn fact(&self, path: &Path, name: &str) -> FactValue {
        let facts = self.only(path).facts.as_ref().expect("it was measured");
        facts.supplied.values.get(name).copied().expect(name)
    }

    /// The advice git gave, its snippets joined.
    pub(crate) fn snippets(&self) -> Vec<String> {
        let integrations = self.report.integrations.iter();
        let git = integrations.filter(|i| i.id.0 == "git");
        git.flat_map(|i| &i.advice)
            .flat_map(|advice| advice.snippets.clone())
            .collect()
    }
}
