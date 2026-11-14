//! A worktree's removal end to end, with the built-in rules: the real probe,
//! the real `GitQuery`, the real runner and the real executor, over a repository the test
//! made, a plan a person reviewed, and a run stored below the temp dir. Nothing runs on
//! this machine's behalf: no process is looked at, and git reads none of the user's config.

#[path = "../../../kipple-adapters/tests/fixtures/git/rules.rs"]
mod pack;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kipple_adapters::builtin;
#[cfg(unix)]
use kipple_core::{Anchor, Freezer as _, RelPath, VerifiedRoot};
use kipple_core::{
    ApplyPorts, CancelToken, CleanRequest, Confirmed, Engine, EventChannel, Finding, Finished,
    FsProbe as _, HeldPath, ItemOutcome, Judge, Lineage, Methods, MutationPorts, NativeTool, Plan,
    PlanMode, Platform, Policy, Ports, Prepared, ResolvedRoot, ResolvedTool, RootSource, Roots,
    Rule, ScanRequest, Selection, Session, SymbolicRoot, ToolGap, ToolResolver, event_channel,
    prepare_clean, run_prepared,
};
use kipple_platform::{
    ConfinedFs, ConfinedTrash, Environment, FileLocks, FileRunStore, GitQuery, NativeCommands,
    NoFollowFs, OsNames, PLATFORM, PathTools, SystemClock, Walker,
};

use crate::repo::{NoProcesses, Repo, Scan};

/// The rule that removes a worktree.
pub(crate) const REMOVE: &str = "builtin.git.worktree-remove";

/// Whether this OS ships the removal: elsewhere the same worktree is only reported.
pub(crate) fn removes_here() -> bool {
    PLATFORM == Platform::Linux
}

/// Which git the removal runs.
#[derive(Debug, Clone)]
pub(crate) enum Tool {
    /// The one `PATH` finds, resolved as kipple resolves it.
    Found,
    /// A program the test wrote, standing in for git's executable.
    #[cfg(unix)]
    Program(PathBuf),
}

/// A program the test wrote, resolved as it is now, with the version of the git it stands
/// in for: the scan and the apply each resolve it, and must agree.
#[derive(Debug)]
#[cfg(unix)]
struct Written {
    path: PathBuf,
    version: String,
}

#[cfg(unix)]
impl ToolResolver for Written {
    fn resolve(&self, tool: NativeTool) -> Result<ResolvedTool, ToolGap> {
        if tool != NativeTool::Git {
            return Err(ToolGap::Missing);
        }
        let file = frozen(&self.path).ok_or(ToolGap::Unreadable)?;
        Ok(ResolvedTool {
            tool,
            path: self.path.clone(),
            identity: file.identity,
            fingerprint: file.fingerprint,
            version: self.version.clone(),
            runtime: None,
        })
    }
}

/// A change the test makes, owning what it changes.
pub(crate) type Change = Box<dyn FnOnce() + Send>;

/// A change armed to run at the `n`th resolution of git from now.
#[derive(Clone, Default)]
struct Trigger(Arc<Mutex<Option<(usize, Change)>>>);

impl fmt::Debug for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Trigger")
    }
}

impl Trigger {
    #[expect(clippy::unwrap_used, reason = "a poisoned lock fails the test")]
    fn arm(&self, at: usize, change: Change) {
        *self.0.lock().unwrap() = Some((at, change));
    }

    /// Counts one resolution, and runs the change when its turn comes.
    #[expect(clippy::unwrap_used, reason = "a poisoned lock fails the test")]
    fn tick(&self) {
        let mut armed = self.0.lock().unwrap();
        let due = match armed.as_mut() {
            Some((1, _)) => armed.take().map(|(_, change)| change),
            Some((left, _)) => {
                *left -= 1;
                None
            }
            None => None,
        };
        drop(armed);
        if let Some(change) = due {
            change();
        }
    }

    /// Whether the armed change ran.
    #[expect(clippy::unwrap_used, reason = "a poisoned lock fails the test")]
    fn fired(&self) -> bool {
        self.0.lock().unwrap().is_none()
    }
}

/// A resolver that counts its resolutions of git: the apply resolves git once when it
/// scans again, then once as `revisit` starts, before git is asked anything, so a change
/// armed for the second lands after the apply scan and before the item is read again.
#[derive(Debug)]
struct Counted {
    inner: Box<dyn ToolResolver>,
    trigger: Trigger,
}

impl ToolResolver for Counted {
    fn resolve(&self, tool: NativeTool) -> Result<ResolvedTool, ToolGap> {
        if tool == NativeTool::Git {
            self.trigger.tick();
        }
        self.inner.resolve(tool)
    }
}

/// The file at `path` as the executor reads it, frozen from the directory holding it.
#[cfg(unix)]
fn frozen(path: &Path) -> Option<kipple_core::EntrySnapshot> {
    let parent = path.parent()?;
    let identity = |at: &Path| NoFollowFs.metadata(at).ok()?.identity;
    let anchor = Anchor {
        root: VerifiedRoot {
            root: SymbolicRoot::Home,
            path: parent.to_owned(),
            identity: identity(parent)?,
        },
        relative: RelPath::of(Path::new(path.file_name()?)).ok()?,
        parent: identity(parent),
    };
    let frozen = ConfinedFs.freeze(&anchor, &CancelToken::new()).ok()?;
    frozen.tree.top().snapshot()
}

/// One repository's scans and removals.
pub(crate) struct Removal<'repo> {
    repo: &'repo Repo,
    env: Environment,
    rules: Vec<Rule>,
    projects: PathBuf,
    engine: Engine,
    trigger: Trigger,
}

impl Repo {
    /// Its removals, on this OS's rules, by the git `PATH` finds.
    pub(crate) fn removal(&self) -> Removal<'_> {
        self.removal_with(pack::on(PLATFORM), &Tool::Found)
    }

    /// Its removals by the rules of `platform`, wherever the test runs.
    pub(crate) fn removal_on(&self, platform: Platform) -> Removal<'_> {
        self.removal_with(pack::on(platform), &Tool::Found)
    }

    /// Its removals run by the program at `program`, on this OS's rules.
    #[cfg(unix)]
    pub(crate) fn removal_by(&self, program: &Path) -> Removal<'_> {
        self.removal_with(pack::on(PLATFORM), &Tool::Program(program.to_owned()))
    }

    /// Its removals run by the program at `program`, every question to git asked in `env`.
    #[cfg(unix)]
    pub(crate) fn removal_through(&self, env: Environment, program: &Path) -> Removal<'_> {
        Removal::new(
            self,
            env,
            pack::on(PLATFORM),
            &Tool::Program(program.to_owned()),
        )
    }

    fn removal_with(&self, rules: Vec<Rule>, tool: &Tool) -> Removal<'_> {
        Removal::new(self, self.env(), rules, tool)
    }

    /// The real git's executable, as kipple resolves it.
    #[expect(clippy::unwrap_used, reason = "these tests need a git")]
    pub(crate) fn real_git(&self) -> ResolvedTool {
        PathTools::new(&self.env())
            .resolve(NativeTool::Git)
            .unwrap()
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
impl<'repo> Removal<'repo> {
    fn new(repo: &'repo Repo, env: Environment, rules: Vec<Rule>, tool: &Tool) -> Self {
        let inner: Box<dyn ToolResolver> = match tool {
            Tool::Found => Box::new(PathTools::new(&env)),
            #[cfg(unix)]
            Tool::Program(path) => Box::new(Written {
                path: path.clone(),
                version: repo.real_git().version,
            }),
        };
        let trigger = Trigger::default();
        let tools = Box::new(Counted {
            inner,
            trigger: trigger.clone(),
        });
        let ports = Ports {
            fs: Box::new(NoFollowFs),
            sizer: Box::new(Walker::new().unwrap()),
            processes: Box::new(NoProcesses),
            clock: Box::new(SystemClock),
            tracked: Box::new(GitQuery::new(&env)),
            tools,
            worktrees: Box::new(GitQuery::new(&env)),
        };
        let engine = Engine::new(ports, builtin::integrations(), rules.clone()).unwrap();
        Self {
            repo,
            env,
            rules,
            projects: repo.projects(),
            engine,
            trigger,
        }
    }

    /// The project root opened at `root`, a spelling of the test's `projects`.
    #[cfg(unix)]
    pub(crate) fn projects_at(mut self, root: PathBuf) -> Self {
        self.projects = root;
        self
    }

    /// The test's temp dir as home, kipple's data in it, and the project root.
    fn roots(&self) -> Roots {
        let mut roots = Roots::default();
        let fixed =
            |root, path: &Path| Ok(ResolvedRoot::new(root, path.to_owned(), RootSource::Fixed));
        let home = SymbolicRoot::Home;
        roots.insert(home, fixed(home, self.repo.temp.path()));
        let projects = SymbolicRoot::ProjectRoots;
        roots.add(projects, fixed(projects, &self.projects));
        roots
    }

    fn data(&self) -> PathBuf {
        self.repo.temp.path().join("kipple")
    }

    /// What the host protects, as a run of kipple on this machine would.
    fn request(&self) -> ScanRequest {
        let roots = self.roots();
        let data = self.data();
        let protections =
            kipple_core::Protections::of_host(&roots, Some(&data), &builtin::descriptors(), &held);
        ScanRequest {
            roots,
            lens: None,
            exclude: Vec::new(),
            protections,
        }
    }

    /// What a scan finds now: what a person reviews.
    pub(crate) fn scan(&self) -> Scan {
        let cancel = CancelToken::new();
        let EventChannel { tx, rx } = event_channel(4096, &cancel);
        let report = self.engine.scan(&self.request(), &tx, &cancel);
        drop(rx);
        Scan(report)
    }

    /// Plans `findings` for `mode`, lets `between` change the machine, and runs the plan as
    /// `confirmed` gives it: the apply scans again, and every item is read again before it
    /// runs.
    pub(crate) fn run(
        &self,
        scan: &Scan,
        findings: &[&Finding],
        approval: Approval,
        between: Between,
    ) -> Ran {
        let request = self.request();
        let rescan = self.engine.rescan(&request);
        let git = GitQuery::new(&self.env);
        let mutation = MutationPorts {
            freezer: &ConfinedFs,
            executor: &ConfinedFs,
            trash: &ConfinedTrash::new(None, Arc::new(SystemClock)),
            tracked: &git,
            runner: &NativeCommands::new(&self.env, Some(self.data())),
            locks: &FileLocks,
            worktrees: &git,
        };
        let policy = Policy::default();
        let methods = Methods {
            command_line: None,
            rules: BTreeMap::new(),
            config: None,
        };
        let session = Session {
            judge: Judge {
                rules: &self.rules,
                policy: &policy,
                methods: &methods,
            },
            store: &FileRunStore::new(Some(self.data()), Arc::new(SystemClock)),
            ports: ApplyPorts {
                rescan: &rescan,
                mutation: &mutation,
            },
            codec: &OsNames,
        };
        let cancel = CancelToken::new();
        let selection = Selection(findings.iter().map(|finding| finding.id).collect());
        let clean = CleanRequest {
            scan: &scan.0,
            selection: &selection,
        };
        let prepared: Prepared = prepare_clean(&clean, &session, approval.mode, &cancel);
        let at_revisit = match between {
            Between::Review(change) => {
                change();
                None
            }
            Between::Revisit(change) => {
                self.trigger.arm(2, change);
                Some(&self.trigger)
            }
        };
        let finished = run_prepared(&prepared, approval.confirmed, &session, &mut (), &cancel);
        if let Some(trigger) = at_revisit {
            assert!(trigger.fired(), "the change ran as the item was read again");
        }
        Ran {
            plan: prepared.plan().clone(),
            finished,
        }
    }

    /// Takes `findings` through a plan a person reviewed in the TUI, `change` made once
    /// the apply has scanned again, as the item is about to be read again.
    pub(crate) fn remove_changed(&self, scan: &Scan, findings: &[&Finding], change: Change) -> Ran {
        self.run(scan, findings, Approval::PERSON, Between::Revisit(change))
    }

    /// Takes `findings` through a plan a person reviewed in the TUI, `change` made after the
    /// review and before the apply scans again.
    pub(crate) fn remove_after(&self, scan: &Scan, findings: &[&Finding], change: Change) -> Ran {
        self.run(scan, findings, Approval::PERSON, Between::Review(change))
    }

    /// Takes `findings` through a plan a person reviewed in the TUI.
    pub(crate) fn remove(&self, scan: &Scan, findings: &[&Finding]) -> Ran {
        self.run(
            scan,
            findings,
            Approval::PERSON,
            Between::Review(Box::new(|| {})),
        )
    }
}

/// Who a plan is for, and who confirms it.
#[derive(Debug, Clone)]
pub(crate) struct Approval {
    pub(crate) mode: PlanMode,
    pub(crate) confirmed: Confirmed,
}

impl Approval {
    /// A person reviewing item by item in the TUI.
    pub(crate) const PERSON: Self = Self {
        mode: PlanMode::Review,
        confirmed: Confirmed::Person,
    };
}

/// When a test changes the machine during a run.
pub(crate) enum Between {
    /// After the review, before the apply scans again.
    Review(Change),
    /// After the apply scanned again, as the item is read again.
    Revisit(Change),
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

/// A run: the plan the person saw, and how it ended.
#[derive(Debug)]
pub(crate) struct Ran {
    pub(crate) plan: Plan,
    pub(crate) finished: Finished,
}

impl Ran {
    /// How its one item ended.
    #[expect(clippy::expect_used, reason = "a run with no item fails the test")]
    pub(crate) fn outcome(&self) -> &ItemOutcome {
        let items = &self.finished.receipt.items;
        assert_eq!(items.len(), 1, "one item: {:#?}", self.finished);
        &items.first().expect("one item").outcome
    }
}
