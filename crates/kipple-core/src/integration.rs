//! What an integration may do during a scan (`docs/05-architecture.md` §2).
//!
//! An integration gets read-only probes confined to the roots it declared, and reports
//! what it found through a [`FindingSink`]. It never sees anything that can mutate.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cancel::CancelToken;
use crate::capability::NativeTool;
use crate::discovery::{RepositoryDir, Walk, WalkPlan, WalkStop, Walks};
use crate::engine::{Finding, FindingId, Found, Omission, Reading};
use crate::event::{EventTx, ScanEvent, Stopped};
use crate::native::{ResolvedTool, ToolGap, ToolResolver};
use crate::policy::Consequence;
use crate::probe::{DirEntryMeta, EntryKind, EntryMeta, FsProbe, ProbeError};
use crate::protect::{OwnerStatus, StateDeclaration};
use crate::root::{ResolvedRoot, SymbolicRoot};
use crate::rule::{FactSpec, Lens, Operation, Platform, Rule, Selector};
use crate::supplied::{AdviceId, Claim, Supplied};
use crate::tracked::GitGap;
use crate::worktree::{AgentLabel, WorktreeQuery};

/// A built-in integration's ID, such as `cargo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IntegrationId(pub &'static str);

impl IntegrationId {
    /// The owner of third-party rules, which use only generic selectors and facts.
    pub const GENERIC: Self = Self("generic");
    /// Git, the one integration whose worktrees may give way on tracked files.
    pub const GIT: Self = Self("git");
}

impl fmt::Display for IntegrationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.0)
    }
}

/// What an integration declares about itself before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationDescriptor {
    /// Its ID.
    pub id: IntegrationId,
    /// Which home-screen lens its findings belong to.
    pub lens: Lens,
    /// The operating systems it supports.
    pub platforms: &'static [Platform],
    /// Roots it reads besides its rules' roots. It may read nothing else.
    pub roots: &'static [SymbolicRoot],
    /// Selectors it publishes, which only its own first-party rules may use.
    pub selectors: &'static [IntegrationSelector],
    /// Directory names that are always artifacts of its tool: no walk descends into them.
    pub artifacts: &'static [&'static str],
    /// File names that make the directory holding them its tool's, such as a virtual
    /// environment's `pyvenv.cfg`: no walk looks for anything inside that directory.
    pub owner_files: &'static [&'static str],
    /// The upstream versions whose layouts it was verified against, with a fixture each.
    pub verified: &'static [&'static str],
    /// Every consequence it can attach to one of its rules' findings
    /// ([`FindingSink::add`]). A profile freezes them with the rule (D-068), so a laxer
    /// recovery, loss or cost suspends the profile's entries for its rules.
    pub consequences: &'static [Consequence],
    /// The facts its findings can supply, which its own rules may test.
    pub facts: &'static [FactSpec],
    /// Its tool's state roots: what the host protects whether or not it runs here.
    pub state: &'static [StateDeclaration],
    /// What can show one of its findings in use. One that declares nothing gets
    /// [`ActivityEvidence::Everything`].
    pub evidences: &'static [ActivityEvidence],
    /// Its revision: bumped whenever its code changes what it finds, how it verifies a
    /// layout, or the consequence it attaches. A profile freezes it with the rule, so a
    /// change that lives in code no [`Rule`] shows suspends the profile's entries for its
    /// rules. `kipple-adapters/tests/golden/revision` keeps each revision's digest of the
    /// integration's output over its layout fixtures, and `integration_revision.rs` fails
    /// unless the current pair is its newest row. Changed output bumps this and appends a
    /// row, and review holds rows to append-only: the test sees only the rows as they are.
    pub revision: u32,
}

impl IntegrationDescriptor {
    /// The evidence its findings are judged by unless one chooses another.
    #[must_use]
    pub fn evidence(&self) -> ActivityEvidence {
        self.evidences
            .first()
            .copied()
            .unwrap_or(ActivityEvidence::Everything)
    }

    /// Whether a finding of this integration may be judged by `evidence`: one it lists, or
    /// [`Everything`](ActivityEvidence::Everything) when it lists none.
    #[must_use]
    pub fn declares(&self, evidence: ActivityEvidence) -> bool {
        self.evidences.contains(&evidence)
            || (self.evidences.is_empty() && evidence == ActivityEvidence::Everything)
    }
}

/// What can show one of an integration's findings in use (`docs/04-safety-model.md` §4,
/// D-050).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityEvidence {
    /// Every path a process shows counts, every process counts, and any path kipple
    /// couldn't read leaves activity unknown. The strictest, and what an integration that
    /// declares nothing gets.
    Everything,
    /// A project artifact that holds executables built in it, such as Cargo's `target/`:
    /// the executable a process runs. Root, `SYSTEM` and other users' processes are
    /// counted only as D-050 says.
    BuiltExecutables,
    /// A project artifact an interpreter loads, such as `.venv` or `node_modules`: every
    /// path a process shows (its executable, working directory, path arguments,
    /// environment paths and mapped files), and a process of one of these runtimes working
    /// in the project. Root, `SYSTEM` and other users' processes are counted only as
    /// D-050 says.
    Interpreted {
        /// The runtimes' program names, compared ignoring case. A trailing `*` matches
        /// any ending, as in `python*`.
        runtimes: &'static [&'static str],
    },
    /// Files a tool keeps outside any project, such as a worktree or a log: every path a
    /// process shows. Root, `SYSTEM` and other users' processes are counted only as D-050
    /// says, and outside the home directory every process counts.
    ScopedPaths,
    /// A store of a tool's own releases: the executable a process runs. Stricter than
    /// [`BuiltExecutables`](Self::BuiltExecutables): a process kipple can't inspect blocks
    /// when it goes by one of `names`, whoever owns it, and a current user's named so
    /// keeps no excuse. A name never holds a process kipple could inspect.
    ReleaseStore {
        /// What the tool's own processes are called.
        names: &'static [Program],
    },
    /// A tool whose runs leave no executable to match, such as npm under node: a process
    /// with a listed name, naming a listed script, or showing any path at or in the finding
    /// or a store, its environment's read lexically. A current user's process kipple can't
    /// inspect keeps D-052's excuse unless `programs` or `runtimes` list it. A name adds a
    /// hold and never proves idle.
    ToolRunning {
        /// What the tool's own processes are called.
        programs: &'static [Program],
        /// The file names of the scripts that run it.
        scripts: &'static [&'static str],
        /// The runtimes that run those scripts, as in [`Interpreted`](Self::Interpreted).
        runtimes: &'static [&'static str],
        /// The roots a run of the tool works in. One with no path leaves activity unknown.
        stores: &'static [SymbolicRoot],
    },
}

/// A program name, compared ignoring case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    /// Exactly this name.
    Exact(&'static str),
    /// Any name starting with this.
    Prefix(&'static str),
}

impl Program {
    /// Whether `name`, in lowercase, is this program.
    pub(crate) fn matches(self, name: &str) -> bool {
        match self {
            Self::Exact(exact) => name == exact.to_lowercase(),
            Self::Prefix(prefix) => name.starts_with(&prefix.to_lowercase()),
        }
    }
}

/// A selector an integration publishes, with what it selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegrationSelector {
    /// Its name, unique among all selectors.
    pub name: &'static str,
    /// What it selects, for the generated catalog.
    pub meaning: &'static str,
}

/// Why an integration thinks a finding is what its rule says. Shown to people; it never
/// authorizes anything.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Evidence {
    /// A file beside it marks a project, such as `Cargo.toml`.
    Marker(String),
    /// An entry inside it is one its tool writes, such as `CACHEDIR.TAG`.
    Signature(String),
    /// None of the entries its tool writes is inside, so its layout is unknown.
    NoSignature,
    /// It is the root itself, such as the trash.
    Root(SymbolicRoot),
    /// It is a git worktree, where this agent keeps them. The label is display only: the
    /// finding's facts decide, so this counts as a verified layout.
    Worktree {
        /// Which agent's location it is in.
        agent: AgentLabel,
    },
}

/// A native setting or command an integration suggests. Shown, never run by kipple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advice {
    /// A short title.
    pub title: String,
    /// What it does and what to watch out for.
    pub text: String,
    /// Exact commands or settings to copy.
    pub snippets: Vec<String>,
    /// Upstream documentation (HTTPS URLs).
    pub references: Vec<String>,
}

/// Why an integration stopped before it finished.
#[derive(Debug, Clone, thiserror::Error)]
pub enum IntegrationError {
    /// The scan was cancelled or its consumer went away. Not a failure of the integration.
    #[error("the scan stopped")]
    Stopped,
    /// A probe it depended on failed.
    #[error(transparent)]
    Probe(#[from] ProbeError),
}

/// A tool integration. It proposes; the core decides.
pub trait Integration: fmt::Debug + Send + Sync {
    /// Its ID and the roots it reads.
    fn descriptor(&self) -> IntegrationDescriptor;

    /// Finds candidates under its roots and adds each one to `sink`.
    ///
    /// # Errors
    /// [`IntegrationError::Stopped`] when `sink` refuses a finding, so the integration
    /// stops early, or the probe failure that kept it from finishing.
    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError>;

    /// Reads again what `finding` of an apply's scan rests on, right before it runs, and
    /// says what it supplies now. `None` means the scan's facts stand, which apply refuses
    /// for anything but a [`Plain`](crate::Subject::Plain) finding, so a forgotten override
    /// can't pass mid-run. A failure holds the item.
    ///
    /// # Errors
    /// When what it rests on can't be read.
    fn revisit(
        &self,
        cx: &ScanContext<'_>,
        finding: &Finding,
    ) -> Result<Option<Supplied>, IntegrationError> {
        let _ = (cx, finding);
        Ok(None)
    }
}

/// The read-only view an integration gets during a scan.
#[derive(Debug)]
pub struct ScanContext<'scan> {
    pub(crate) roots: &'scan [ResolvedRoot],
    pub(crate) rules: &'scan [Rule],
    pub(crate) fs: &'scan dyn FsProbe,
    pub(crate) tools: &'scan dyn ToolResolver,
    pub(crate) worktrees: &'scan dyn WorktreeQuery,
    pub(crate) cancel: &'scan CancelToken,
    pub(crate) plan: &'scan WalkPlan,
    pub(crate) walks: &'scan Walks,
    /// The walks this integration used, whose gaps are its omissions.
    pub(crate) walked: RefCell<BTreeMap<PathBuf, Arc<Walk>>>,
}

impl ScanContext<'_> {
    /// The integration's declared roots and its rules' roots that exist and are
    /// directories.
    #[must_use]
    pub const fn roots(&self) -> &[ResolvedRoot] {
        self.roots
    }

    /// The paths of `root` that opened. [`SymbolicRoot::ProjectRoots`] and
    /// [`SymbolicRoot::GitAgentLocations`] may have several.
    pub fn roots_of(&self, root: SymbolicRoot) -> impl Iterator<Item = &ResolvedRoot> {
        self.roots
            .iter()
            .filter(move |resolved| resolved.root == root)
    }

    /// The integration's own rules that apply on this platform.
    #[must_use]
    pub const fn rules(&self) -> &[Rule] {
        self.rules
    }

    /// Whether the scan was cancelled. Long loops should check it.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// The scan's cancel token, for a port that takes one, so a cancel reaches a query
    /// already running.
    #[must_use]
    pub const fn cancel_token(&self) -> &CancelToken {
        self.cancel
    }

    /// What `path` itself is, without following a link.
    ///
    /// # Errors
    /// When `path` is outside the integration's roots, or can't be read.
    pub fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        self.fs.metadata(&self.confined(path)?)
    }

    /// The entries of the directory at `dir`, without following links.
    ///
    /// # Errors
    /// When `dir` is outside the integration's roots, or can't be read.
    pub fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        self.fs.read_dir(&self.confined(dir)?)
    }

    /// The first `limit` bytes of the regular file at `path`, without following a link.
    ///
    /// # Errors
    /// When `path` is outside the integration's roots, is not a regular file or can't be
    /// read.
    pub fn read_file(&self, path: &Path, limit: u64) -> Result<Vec<u8>, ProbeError> {
        self.fs.read_file(&self.confined(path)?, limit)
    }

    /// Where the link at `path` points, exactly as it is stored.
    ///
    /// # Errors
    /// When `path` is outside the integration's roots, isn't a link or can't be read.
    pub fn read_link(&self, path: &Path) -> Result<PathBuf, ProbeError> {
        self.fs.read_link(&self.confined(path)?)
    }

    /// What the OS says of the link at `path` itself ([`FsProbe::link_stamp`]).
    ///
    /// # Errors
    /// When `path` is outside the integration's roots, isn't a link, can't be read, or the
    /// OS has no such stamp.
    pub fn link_stamp(&self, path: &Path) -> Result<String, ProbeError> {
        self.fs.link_stamp(&self.confined(path)?)
    }

    /// Asks git about a repository's worktrees.
    #[must_use]
    pub const fn worktrees(&self) -> &dyn WorktreeQuery {
        self.worktrees
    }

    /// `tool` as this machine has it now.
    ///
    /// # Errors
    /// When it can't be used, and why.
    pub fn tool(&self, tool: NativeTool) -> Result<ResolvedTool, ToolGap> {
        self.tools.resolve(tool)
    }

    /// Where the shared walk of each path of `root` stopped: at every candidate artifact
    /// of any rule, and at every directory a registered tool always owns. Directories it
    /// could not list become the integration's omissions.
    ///
    /// # Errors
    /// [`IntegrationError::Stopped`] when the scan was cancelled during the walk.
    pub fn walk_stops(&self, root: SymbolicRoot) -> Result<Vec<WalkStop>, IntegrationError> {
        self.walks_of(root, |stops, walk| keep_nearest(stops, &walk.stops))
            .map(BTreeMap::into_values)
            .map(Iterator::collect)
    }

    /// Every directory the shared walk of `root` found holding a `.git`, by what that is.
    ///
    /// # Errors
    /// [`IntegrationError::Stopped`] when the scan was cancelled during the walk.
    pub fn repositories(&self, root: SymbolicRoot) -> Result<Vec<RepositoryDir>, IntegrationError> {
        self.walks_of(root, |repositories: &mut BTreeMap<_, _>, walk| {
            for repository in &walk.repositories {
                repositories.insert(repository.dir.clone(), repository.clone());
            }
        })
        .map(BTreeMap::into_values)
        .map(Iterator::collect)
    }

    /// Walks each path of `root`, folding what each walk found into a map by path.
    fn walks_of<T>(
        &self,
        root: SymbolicRoot,
        fold: impl Fn(&mut BTreeMap<PathBuf, T>, &Walk),
    ) -> Result<BTreeMap<PathBuf, T>, IntegrationError> {
        // A directory under two paths of the root, given twice or one inside the other,
        // is one stop, as near its root as it is anywhere.
        let mut found = BTreeMap::new();
        for resolved in self.roots_of(root) {
            let walk = self
                .walks
                .get(self.plan, self.fs, &resolved.path, self.cancel);
            if walk.cancelled {
                return Err(IntegrationError::Stopped);
            }
            fold(&mut found, &walk);
            self.walked.borrow_mut().insert(resolved.path.clone(), walk);
        }
        Ok(found)
    }

    /// The candidates of a `project-artifact` rule under its root: directories named like
    /// its artifact, within its depth, with one of its markers beside them. Each one's
    /// markers are narrowed to the rule's. Empty for any other selector.
    ///
    /// # Errors
    /// [`IntegrationError::Stopped`] when the scan was cancelled during the walk.
    pub fn project_artifacts(&self, rule: &Rule) -> Result<Vec<WalkStop>, IntegrationError> {
        let Selector::ProjectArtifact {
            markers,
            artifact,
            max_depth,
        } = &rule.selector
        else {
            return Ok(Vec::new());
        };
        let candidates = self
            .walk_stops(rule.root)?
            .into_iter()
            .filter(|stop| stop.is_named(&artifact.0) && stop.depth <= *max_depth)
            .map(|stop| {
                let own = markers
                    .iter()
                    .filter(|marker| stop.markers.contains(&marker.0))
                    .map(|marker| marker.0.clone())
                    .collect();
                WalkStop {
                    markers: own,
                    ..stop
                }
            })
            .filter(|stop| !stop.markers.is_empty())
            .collect();
        Ok(candidates)
    }

    /// `path` without a trailing separator or `.` component, if it lies inside one of the
    /// roots. Those would make the OS follow a link in the last component. A path with a
    /// `..` component is refused rather than normalised, so it can't climb out of a root.
    /// So is a path through a link below the root, which the OS would follow too. Each
    /// directory below the root is checked from the top, so nothing beyond a link is
    /// touched.
    fn confined(&self, path: &Path) -> Result<PathBuf, ProbeError> {
        let outside = || ProbeError::OutsideRoots {
            path: path.to_owned(),
        };
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(outside());
        }
        let path: PathBuf = path.components().collect();
        let (root, below) = self
            .roots
            .iter()
            .find_map(|root| Some((root, path.strip_prefix(&root.path).ok()?)))
            .ok_or_else(outside)?;
        let mut dir = root.path.clone();
        let mut between = below.components();
        between.next_back();
        for part in between {
            dir.push(part);
            if self.fs.metadata(&dir)?.kind == EntryKind::Symlink {
                return Err(ProbeError::ThroughLink { link: dir });
            }
        }
        Ok(path)
    }
}

/// Adds `found` to `stops`, keeping each directory where it is nearest its root.
fn keep_nearest(stops: &mut BTreeMap<PathBuf, WalkStop>, found: &[WalkStop]) {
    for stop in found {
        let nearer = stops
            .get(&stop.path)
            .is_none_or(|seen| stop.depth < seen.depth);
        if nearer {
            stops.insert(stop.path.clone(), stop.clone());
        }
    }
}

/// Where an integration reports what it found. Every finding is streamed to the
/// frontend as it is added, waiting while the stream is full.
#[derive(Debug)]
pub struct FindingSink<'scan> {
    pub(crate) integration: IntegrationId,
    pub(crate) cx: &'scan ScanContext<'scan>,
    pub(crate) events: &'scan EventTx,
    pub(crate) next_id: &'scan AtomicU64,
    pub(crate) found: Vec<Found>,
    pub(crate) advice: Vec<Advice>,
    /// What it could not cover, past the walks the host reports for it.
    pub(crate) omissions: Vec<Omission>,
    /// What a finding is judged by unless it chooses another.
    pub(crate) evidence: ActivityEvidence,
    /// What it said about the layout it read.
    pub(crate) owner: OwnerStatus,
}

/// What is recorded with a finding, past its rule and path.
#[derive(Debug)]
struct Added {
    claim: Claim,
    supplied: Supplied,
    reading: Reading,
}

impl FindingSink<'_> {
    /// Adds a finding of `rule` at `path`, which must lie inside the integration's roots,
    /// with what removing it costs when the integration verified that. Report-only
    /// findings and unrecognised layouts have no consequence.
    ///
    /// # Errors
    /// [`IntegrationError::Stopped`] when the scan was cancelled or its consumer went
    /// away, and [`IntegrationError::Probe`] when `path` is outside the roots. The
    /// integration should return either.
    pub fn add(
        &mut self,
        rule: &Rule,
        path: &Path,
        evidence: Vec<Evidence>,
        consequence: Option<Consequence>,
    ) -> Result<(), IntegrationError> {
        let claim = Claim {
            evidence,
            consequence,
        };
        self.add_supplied(rule, path, claim, self.supplied())
    }

    /// Records that git couldn't list the worktrees of `repository`, so the report says
    /// they aren't known and never reads as "no worktrees".
    pub fn omit_worktrees(&mut self, repository: &Path, gap: GitGap) {
        self.omissions.push(Omission::GitInventory {
            repository: repository.to_owned(),
            gap,
        });
    }

    /// Says whether the integration recognised what it read, which is what lets its state
    /// roots open. It stands only if `discover` runs to its end.
    pub const fn set_owner_status(&mut self, status: OwnerStatus) {
        self.owner = status;
    }

    /// What a plain finding of this integration supplies, to build on.
    #[must_use]
    pub fn supplied(&self) -> Supplied {
        Supplied {
            activity: self.evidence,
            ..Supplied::default()
        }
    }

    /// Adds a finding like [`add`](Self::add), with what the integration supplies about
    /// it.
    ///
    /// # Errors
    /// As [`add`](Self::add).
    pub fn add_supplied(
        &mut self,
        rule: &Rule,
        path: &Path,
        claim: Claim,
        supplied: Supplied,
    ) -> Result<(), IntegrationError> {
        let path = self.cx.confined(path)?;
        let added = Added {
            claim,
            supplied,
            reading: Reading::Measured,
        };
        self.record(rule, path, added)
    }

    /// Adds a finding kipple neither reads nor measures, such as a registered worktree whose
    /// checkout is gone, or one outside every opened root: it is only reported, has no
    /// anchor, and everything measuring would say about it is unknown.
    ///
    /// # Errors
    /// [`IntegrationError::Stopped`] as for [`add`](Self::add), and
    /// [`IntegrationError::Probe`] when `rule` removes anything: its path wasn't confined
    /// to the roots, so it is outside what a removal may reach.
    pub fn add_unread(
        &mut self,
        rule: &Rule,
        path: &Path,
        evidence: Vec<Evidence>,
        supplied: Supplied,
    ) -> Result<(), IntegrationError> {
        if rule.operation != Operation::Report {
            let path = path.to_owned();
            return Err(ProbeError::OutsideRoots { path }.into());
        }
        let claim = Claim {
            evidence,
            consequence: None,
        };
        let added = Added {
            claim,
            supplied,
            reading: Reading::Unread,
        };
        self.record(rule, path.to_owned(), added)
    }

    fn record(&mut self, rule: &Rule, path: PathBuf, added: Added) -> Result<(), IntegrationError> {
        let Added {
            claim: Claim {
                evidence,
                consequence,
            },
            supplied,
            reading,
        } = added;
        let finding = Finding {
            id: FindingId(self.next_id.fetch_add(1, Ordering::Relaxed)),
            integration: self.integration,
            rule: rule.id.clone(),
            category: rule.category,
            path,
            evidence,
            size: None,
            facts: None,
        };
        let event = ScanEvent::FindingAdded {
            finding: finding.clone(),
        };
        self.events
            .send(event, self.cx.cancel)
            .map_err(|_: Stopped| IntegrationError::Stopped)?;
        self.found.push(Found {
            finding,
            consequence,
            supplied,
            reading,
        });
        Ok(())
    }

    /// Adds advice, which the report shows with the integration, and returns the ID a
    /// finding it applies to names in [`Supplied::advice`].
    pub fn advise(&mut self, advice: Advice) -> AdviceId {
        self.advice.push(advice);
        AdviceId(self.advice.len() - 1)
    }
}
