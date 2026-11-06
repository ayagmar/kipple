//! Git worktrees of any agent (`docs/06-adapters-and-rules.md` §4). Each registration of a
//! repository the walks reach gets exactly one finding, and so does each checkout no
//! repository lists. Every rule here reports, so this only supplies facts: a fact git
//! couldn't prove is unknown, and the host decides what that holds.
//!
//! A worktree's repository can lie outside every root it may read, so what git says comes
//! through [`WorktreeQuery`](kipple_core::WorktreeQuery). Only finding which directories are
//! linked worktrees at all reads `.git` files here.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Component, Path, PathBuf};

use kipple_core::{
    ActivityEvidence, Advice, AdviceId, AgentLabel, Checkout, Claim, ConfigGate, EntryKind,
    Evidence, FactKind, FactSpec, FactValue, FindingSink, GitGap, Integration,
    IntegrationDescriptor, IntegrationError, IntegrationId, IntegrationSelector, Lens, NativeTool,
    OwnerStatus, Platform, ProbeError, Registration, RegistrationState, ResolvedTool, Rule, RuleId,
    ScanContext, Subject, Supplied, SymbolicRoot, ToolGap, ToolVersion, WorktreeFacts,
    WorktreeState,
};

const SELECTORS: &[IntegrationSelector] = &[
    IntegrationSelector {
        name: "registered-linked-worktrees",
        meaning: "linked worktrees that are registered and present",
    },
    IntegrationSelector {
        name: "worktree-registrations",
        meaning: "registrations that are missing, unregistered or invalid",
    },
];

/// The one selector a checkout's own facts apply to.
const PRESENT: Option<&str> = Some("registered-linked-worktrees");

const fn fact(name: &'static str, kind: FactKind, meaning: &'static str) -> FactSpec {
    FactSpec {
        name,
        kind,
        selector: PRESENT,
        meaning,
    }
}

const FACTS: &[FactSpec] = &[
    fact(
        "worktree.linked",
        FactKind::Bool,
        "whether git places it apart from the main worktree",
    ),
    fact(
        "worktree.removal_git",
        FactKind::Enum(&["supported", "unsupported"]),
        "whether the git found is new enough to remove a worktree",
    ),
    fact(
        "repository_identity",
        FactKind::Enum(&["verified", "differs"]),
        "whether git places it in the repository that lists it",
    ),
    fact(
        "worktree.locked",
        FactKind::Bool,
        "whether it is locked, which kipple never undoes",
    ),
    FactSpec {
        name: "registration_state",
        kind: FactKind::Enum(&[
            "registered-present",
            "registered-missing",
            "present-unregistered",
            "registered-invalid",
        ]),
        selector: None,
        meaning: "how its registration and its checkout relate",
    },
    fact(
        "worktree.submodules",
        FactKind::Bool,
        "whether it has a submodule, populated or not",
    ),
    fact(
        "worktree.config_gate",
        FactKind::Enum(&["clear", "blocked"]),
        "whether its repository's config lets kipple ask git further",
    ),
    fact(
        "git.hidden_changes",
        FactKind::Count,
        "files git tracks that `status` doesn't look behind",
    ),
    fact(
        "worktree.unclassified_ignored",
        FactKind::Count,
        "ignored paths that aren't exactly a finding a rule would remove",
    ),
    fact(
        "worktree.operation_in_progress",
        FactKind::Bool,
        "whether a rebase, merge, bisect, cherry-pick or revert is under way",
    ),
    fact(
        "worktree.per_worktree_refs",
        FactKind::Count,
        "refs that live only in this worktree",
    ),
    fact(
        "worktree.index_locked",
        FactKind::Bool,
        "whether git holds its index or HEAD lock",
    ),
    fact(
        "worktree.head_reachable",
        FactKind::Bool,
        "whether a local branch holds its HEAD",
    ),
    FactSpec {
        name: "git_inventory",
        kind: FactKind::Enum(&["complete"]),
        selector: None,
        meaning: "whether git listed the repository's worktrees in full",
    },
    FactSpec {
        name: "storage_availability",
        kind: FactKind::Enum(&["available"]),
        selector: None,
        meaning: "whether what holds the checkout can be read",
    },
];

/// The oldest git the removal is proven on: the one `gate` runs on in CI.
const WORKTREE_REMOVE_OLDEST: [u64; 3] = [2, 39, 5];

/// The most read of a `.git` file: it holds one path.
const GITFILE_LIMIT: u64 = 64 * 1024;

/// Where each agent keeps worktrees outside any repository, as `git.agent-locations` holds
/// them. A location is known by its own name, because a setting can move what it is under.
/// Anything else in that root is Codex's: `<codex home>/worktrees`, wherever that home is.
const LOCATIONS: [(&str, AgentLabel); 4] = [
    (".cursor/worktrees", AgentLabel::Cursor),
    ("conductor/workspaces", AgentLabel::Conductor),
    (".herdr/worktrees", AgentLabel::Herdr),
    ("opencode/worktree", AgentLabel::OpenCode),
];

/// Where each agent keeps worktrees inside a repository.
const IN_REPOSITORY: [(&str, AgentLabel); 2] = [
    (".claude/worktrees", AgentLabel::ClaudeCode),
    (".gemini/worktrees", AgentLabel::GeminiCli),
];

/// The Git integration.
#[derive(Debug)]
pub(crate) struct Git;

impl Integration for Git {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId::GIT,
            lens: Lens::Agents,
            platforms: Platform::ALL,
            roots: &[SymbolicRoot::ProjectRoots, SymbolicRoot::GitAgentLocations],
            selectors: SELECTORS,
            artifacts: &[],
            owner_files: &[],
            verified: &["git 2.56.0"],
            consequences: &[],
            facts: FACTS,
            state: &[],
            evidences: &[ActivityEvidence::ScopedPaths],
            revision: 2,
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let mut checkouts = BTreeMap::new();
        for root in [SymbolicRoot::ProjectRoots, SymbolicRoot::GitAgentLocations] {
            for found in cx.repositories(root)? {
                checkouts.insert(found.dir, found.git);
            }
        }
        let mut scan = Discovery::new(cx, sink);
        for (dir, git) in checkouts {
            scan.visit(dir, git)?;
        }
        scan.finish()
    }
}

/// What a directory's `.git` file makes it.
#[derive(Debug, PartialEq, Eq)]
enum GitFile {
    /// A linked worktree, as far as its files show, with its repository's common directory.
    Linked { common: PathBuf },
    /// Its target has the shape of an admin directory and isn't there.
    Broken,
    /// A submodule's, a `--separate-git-dir` repository's, or none git would read.
    Other,
}

/// The rules each kind of finding goes to on this platform.
struct Rules<'rules> {
    /// Registered and present, inside an opened root: only where removal can ship.
    remove: Option<&'rules Rule>,
    inventory: Option<&'rules Rule>,
    anomalies: Option<&'rules Rule>,
}

impl<'rules> Rules<'rules> {
    fn of(cx: &'rules ScanContext<'_>) -> Self {
        let rule = |id: &str| {
            cx.rules()
                .iter()
                .find(|rule| rule.id == RuleId(id.to_owned()))
        };
        Self {
            remove: rule("builtin.git.worktree-remove"),
            inventory: rule("builtin.git.worktree-inventory"),
            anomalies: rule("builtin.git.worktree-anomalies"),
        }
    }
}

/// One scan's walk over the repositories its roots hold.
struct Discovery<'run, 'context, 'findings> {
    cx: &'run ScanContext<'context>,
    sink: &'run mut FindingSink<'findings>,
    rules: Rules<'run>,
    tool: Result<ResolvedTool, ToolGap>,
    /// Every path some repository lists.
    listed: BTreeSet<PathBuf>,
    /// Each main worktree already gone through, which another of its worktrees lists again.
    mains: BTreeSet<PathBuf>,
    /// The common directories git couldn't list, each reported once.
    failed: BTreeSet<PathBuf>,
    /// Linked checkouts to look for in what the repositories listed, once all have been.
    unconfirmed: Vec<PathBuf>,
    /// Checkouts whose `.git` leads nowhere.
    broken: Vec<PathBuf>,
    /// The advice each state shares.
    missing: Option<AdviceId>,
    invalid: Option<AdviceId>,
}

impl<'run, 'context, 'findings> Discovery<'run, 'context, 'findings> {
    fn new(cx: &'run ScanContext<'context>, sink: &'run mut FindingSink<'findings>) -> Self {
        Self {
            cx,
            sink,
            rules: Rules::of(cx),
            tool: cx.tool(NativeTool::Git),
            listed: BTreeSet::new(),
            mains: BTreeSet::new(),
            failed: BTreeSet::new(),
            unconfirmed: Vec::new(),
            broken: Vec::new(),
            missing: None,
            invalid: None,
        }
    }

    /// Goes through the repository of the directory `dir`, whose `.git` is `git`.
    fn visit(&mut self, dir: PathBuf, git: EntryKind) -> Result<(), IntegrationError> {
        match git {
            EntryKind::Dir => {
                let common = dir.join(".git");
                self.list(&dir, &common)
            }
            EntryKind::File => match git_file(self.cx, &dir) {
                GitFile::Linked { common } => {
                    self.list(&dir, &common)?;
                    if !self.failed.contains(&common) && !self.broken.contains(&dir) {
                        self.unconfirmed.push(dir);
                    }
                    Ok(())
                }
                GitFile::Broken => {
                    self.broken.push(dir);
                    Ok(())
                }
                GitFile::Other => Ok(()),
            },
            // A link isn't followed, and git writes neither of the others.
            EntryKind::Symlink | EntryKind::Other => Ok(()),
        }
    }

    /// Asks git for the registrations of the repository `dir` belongs to, unless they came
    /// with another of its worktrees. A list that fails says so, and never reads as a
    /// repository with no worktrees.
    fn list(&mut self, dir: &Path, common: &Path) -> Result<(), IntegrationError> {
        if self.listed.contains(dir) || self.failed.contains(common) {
            return Ok(());
        }
        let listed = self.cx.worktrees().list(dir, self.cx.cancel_token());
        match listed {
            Ok(registrations) => self.registrations(&registrations),
            Err(_) if self.cx.is_cancelled() => Err(IntegrationError::Stopped),
            // What its `.git` names is gone, which the platform can see past the roots.
            Err(GitGap::Unplaced) => {
                self.broken.push(dir.to_owned());
                Ok(())
            }
            Err(gap) => {
                self.failed.insert(common.to_owned());
                self.sink.omit_worktrees(dir, gap);
                Ok(())
            }
        }
    }

    /// Adds a finding for each linked worktree of a repository. The first record is its
    /// main worktree, which is never one.
    fn registrations(&mut self, registrations: &[Registration]) -> Result<(), IntegrationError> {
        let Some((main, linked)) = registrations.split_first() else {
            return Ok(());
        };
        self.listed
            .extend(registrations.iter().map(|r| r.path.clone()));
        if !self.mains.insert(main.path.clone()) {
            return Ok(());
        }
        for registration in linked {
            self.registered(main, registration)?;
        }
        Ok(())
    }

    fn registered(
        &mut self,
        main: &Registration,
        registration: &Registration,
    ) -> Result<(), IntegrationError> {
        if self.cx.is_cancelled() {
            return Err(IntegrationError::Stopped);
        }
        let state = self
            .cx
            .worktrees()
            .inspect(registration, self.cx.cancel_token());
        let agent = label(self.cx, &registration.path);
        let values = facts(main, registration, &state, &self.tool);
        let advice = self.advice(&state);
        let chosen = state.registration;
        let worktree = WorktreeFacts {
            registration: registration.clone(),
            // A row git couldn't prove never reads as present.
            state: chosen.unwrap_or(RegistrationState::RegisteredInvalid),
            repository: main.path.clone(),
            admin: state.place.as_ref().ok().map(|place| place.git_dir.clone()),
            witness: state.witness,
            ignored: state.ignored,
            tool: self.tool.clone(),
            agent,
        };
        let supplied = Supplied {
            values,
            subject: Subject::Worktree(Box::new(worktree)),
            activity: ActivityEvidence::ScopedPaths,
            advice,
        };
        self.add(&registration.path, chosen, agent, supplied)
    }

    /// Adds the one finding of a registration. What isn't a directory inside an opened root
    /// is only reported: its path wasn't confined, so nothing may reach it.
    fn add(
        &mut self,
        path: &Path,
        state: Result<RegistrationState, GitGap>,
        agent: AgentLabel,
        supplied: Supplied,
    ) -> Result<(), IntegrationError> {
        let inside = self
            .cx
            .metadata(path)
            .is_ok_and(|meta| meta.kind == EntryKind::Dir);
        let anomaly = matches!(
            state,
            Ok(RegistrationState::RegisteredMissing | RegistrationState::RegisteredInvalid)
        );
        let rule = match (anomaly, inside) {
            (true, _) => self.rules.anomalies,
            (false, true) => self.rules.remove.or(self.rules.inventory),
            (false, false) => self.rules.inventory,
        };
        let Some(rule) = rule else {
            return Ok(());
        };
        let evidence = vec![Evidence::Worktree { agent }];
        if inside {
            let claim = Claim {
                evidence,
                consequence: None,
            };
            self.sink.add_supplied(rule, path, claim, supplied)
        } else {
            self.sink.add_unread(rule, path, evidence, supplied)
        }
    }

    /// The advice of a registration in `state`, given once however many share it.
    fn advice(&mut self, state: &WorktreeState) -> Option<AdviceId> {
        let (slot, anomaly) = match state.registration {
            Ok(RegistrationState::RegisteredMissing) => (&mut self.missing, Anomaly::Missing),
            Ok(RegistrationState::RegisteredInvalid) => (&mut self.invalid, Anomaly::Invalid),
            _ => return None,
        };
        let sink = &mut *self.sink;
        Some(*slot.get_or_insert_with(|| sink.advise(registration_advice(anomaly))))
    }

    /// Reports each checkout no repository lists, now that all have been asked.
    fn finish(mut self) -> Result<(), IntegrationError> {
        // A repository that moved still lists its checkouts, whose links now lead nowhere:
        // those have their registration's finding.
        for dir in std::mem::take(&mut self.broken) {
            if !self.listed.contains(&dir) {
                self.unregistered(&dir, Unregistered::TargetGone)?;
            }
        }
        for dir in std::mem::take(&mut self.unconfirmed) {
            if !self.listed.contains(&dir) && !self.listed_elsewhere(&dir) {
                self.unregistered(&dir, Unregistered::Moved)?;
            }
        }
        // A layout is supported once git has read one, and one it couldn't read says so.
        if !self.mains.is_empty() && self.failed.is_empty() {
            self.sink.set_owner_status(OwnerStatus::Supported);
        }
        Ok(())
    }

    /// Whether the checkout at `dir` is one a repository lists under another spelling, as
    /// it does below a link in a root's parents: git prints the path it resolved.
    fn listed_elsewhere(&self, dir: &Path) -> bool {
        let asked = Registration {
            path: dir.to_owned(),
            head: None,
            checkout: Checkout::Unnamed,
            locked: None,
            prunable: false,
            bare: false,
        };
        let state = self.cx.worktrees().inspect(&asked, self.cx.cancel_token());
        state
            .place
            .is_ok_and(|place| self.listed.contains(&place.top))
    }

    /// Adds the finding of a checkout no repository lists. It is no [`WorktreeFacts`]: there
    /// is no registration, and its repository may be gone.
    fn unregistered(&mut self, dir: &Path, why: Unregistered) -> Result<(), IntegrationError> {
        let Some(rule) = self.rules.anomalies else {
            return Ok(());
        };
        let advice = self
            .sink
            .advise(registration_advice(Anomaly::Unregistered(dir, why)));
        let mut supplied = self.sink.supplied();
        supplied.advice = Some(advice);
        supplied.values.insert(
            "registration_state",
            FactValue::Enum(RegistrationState::PresentUnregistered.name()),
        );
        let claim = Claim {
            evidence: vec![Evidence::Worktree {
                agent: label(self.cx, dir),
            }],
            consequence: None,
        };
        self.sink.add_supplied(rule, dir, claim, supplied)
    }
}

/// What the `.git` file in `dir` makes it. It is linked when its target's parent is named
/// `worktrees` and holds `commondir`: a submodule's and a `--separate-git-dir` repository's
/// don't. A target this integration can't read may still be one, so git is asked.
fn git_file(cx: &ScanContext<'_>, dir: &Path) -> GitFile {
    let admin = match target(cx, dir) {
        Target::Path(admin) => admin,
        Target::Unread => {
            return GitFile::Linked {
                common: dir.join(".git"),
            };
        }
        Target::None => return GitFile::Other,
    };
    let Some(worktrees) = admin.parent().filter(|at| at.ends_with("worktrees")) else {
        return GitFile::Other;
    };
    let common = worktrees.parent().unwrap_or(worktrees).to_owned();
    match cx.metadata(&admin) {
        Ok(meta) if meta.kind != EntryKind::Dir => GitFile::Other,
        Ok(_) => match cx.metadata(&admin.join("commondir")) {
            Err(error) if is_absent(&error) => GitFile::Other,
            Ok(_) | Err(_) => GitFile::Linked { common },
        },
        Err(error) if is_absent(&error) => GitFile::Broken,
        Err(_) => GitFile::Linked { common },
    }
}

/// What a `.git` file says.
enum Target {
    /// This, resolved against its directory.
    Path(PathBuf),
    /// A link read as bytes alone can't be followed here, as one that isn't UTF-8 or that
    /// couldn't be read. git reads bytes, so it is the one to ask.
    Unread,
    /// No link git would read.
    None,
}

/// Where the `.git` file in `dir` points, with `.` and `..` folded by text since a confined
/// read refuses them.
fn target(cx: &ScanContext<'_>, dir: &Path) -> Target {
    let Ok(bytes) = cx.read_file(&dir.join(".git"), GITFILE_LIMIT) else {
        return Target::Unread;
    };
    let Some(link) = bytes.strip_prefix(b"gitdir: ") else {
        return Target::None;
    };
    let Ok(text) = std::str::from_utf8(link) else {
        return Target::Unread;
    };
    let path = text.trim_end_matches(['\r', '\n']);
    if path.is_empty() {
        return Target::None;
    }
    Target::Path(folded(&dir.join(path)))
}

fn folded(path: &Path) -> PathBuf {
    let mut parts = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            part @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                parts.push(part);
            }
        }
    }
    parts
}

/// Whether `error` says nothing is there. A path outside the roots or through a link says
/// nothing of the kind.
fn is_absent(error: &ProbeError) -> bool {
    matches!(
        error.io_kind(),
        Some(io::ErrorKind::NotFound | io::ErrorKind::NotADirectory)
    )
}

/// Whose location `path` is in, from where it is alone. A branch name proves nothing, and a
/// location moved by a setting reads as unknown, which is safe: the label grants nothing.
fn label(cx: &ScanContext<'_>, path: &Path) -> AgentLabel {
    let location = cx
        .roots_of(SymbolicRoot::GitAgentLocations)
        .find(|root| path.starts_with(&root.path));
    if let Some(root) = location {
        let known = LOCATIONS.iter().find(|(at, _)| root.path.ends_with(at));
        return known.map_or(AgentLabel::Codex, |(_, agent)| *agent);
    }
    let parent = path.parent();
    let inside = IN_REPOSITORY
        .iter()
        .find(|(at, _)| parent.is_some_and(|parent| parent.ends_with(at)));
    inside.map_or(AgentLabel::Unknown, |(_, agent)| *agent)
}

/// Every fact of a registration. An `Err` row is [`FactValue::Unknown`], which holds. That
/// ignored paths are classified is the scan's to settle, so it starts unknown.
fn facts(
    main: &Registration,
    registration: &Registration,
    state: &WorktreeState,
    tool: &Result<ResolvedTool, ToolGap>,
) -> BTreeMap<&'static str, FactValue> {
    let count = |row: &Result<u64, GitGap>| row.map_or(FactValue::Unknown, FactValue::Count);
    let flag = |row: &Result<bool, GitGap>| row.map_or(FactValue::Unknown, FactValue::Bool);
    let present = state.registration == Ok(RegistrationState::RegisteredPresent);
    let linked = state
        .place
        .as_ref()
        .map(|place| place.git_dir != place.common_dir);
    BTreeMap::from([
        (
            "worktree.linked",
            linked.map_or(FactValue::Unknown, FactValue::Bool),
        ),
        ("worktree.removal_git", removal_git(tool)),
        ("repository_identity", identity(main, state)),
        (
            "worktree.locked",
            FactValue::Bool(registration.locked.is_some()),
        ),
        (
            "registration_state",
            state
                .registration
                .map_or(FactValue::Unknown, |state| FactValue::Enum(state.name())),
        ),
        ("worktree.submodules", flag(&state.submodules)),
        (
            "worktree.config_gate",
            state
                .config_gate
                .map_or(FactValue::Unknown, |gate| match gate {
                    ConfigGate::Clear => FactValue::Enum("clear"),
                    ConfigGate::Blocked(_) => FactValue::Enum("blocked"),
                }),
        ),
        ("git.tracked_changes", count(&state.tracked_changes)),
        ("git.untracked_files", count(&state.untracked_files)),
        ("git.hidden_changes", count(&state.hidden_changes)),
        ("worktree.unclassified_ignored", FactValue::Unknown),
        (
            "worktree.operation_in_progress",
            flag(&state.operation_in_progress),
        ),
        (
            "worktree.per_worktree_refs",
            count(&state.per_worktree_refs),
        ),
        ("worktree.index_locked", flag(&state.index_locked)),
        ("worktree.head_reachable", flag(&state.head_reachable)),
        ("git_inventory", FactValue::Enum("complete")),
        (
            "storage_availability",
            if present {
                FactValue::Enum("available")
            } else {
                FactValue::Unknown
            },
        ),
    ])
}

/// Whether git places the worktree in the repository that lists it: `<main>/.git`, or a bare
/// main itself. Anything else differs, and a witness that couldn't be built is unknown.
fn identity(main: &Registration, state: &WorktreeState) -> FactValue {
    match (&state.place, &state.witness) {
        (Err(GitGap::OtherWorkTree), _) => FactValue::Enum("differs"),
        (Ok(place), Ok(_)) => {
            let common = if main.bare {
                main.path.clone()
            } else {
                main.path.join(".git")
            };
            FactValue::Enum(if place.common_dir == common {
                "verified"
            } else {
                "differs"
            })
        }
        _ => FactValue::Unknown,
    }
}

/// Whether the git a removal would run reaches [`WORKTREE_REMOVE_OLDEST`]. A version kipple
/// can't read is unknown.
fn removal_git(tool: &Result<ResolvedTool, ToolGap>) -> FactValue {
    let Some(version) = tool.as_ref().ok().and_then(|tool| version(&tool.version)) else {
        return FactValue::Unknown;
    };
    FactValue::Enum(if version < ToolVersion(WORKTREE_REMOVE_OLDEST.to_vec()) {
        "unsupported"
    } else {
        "supported"
    })
}

/// The leading numbers of a version as a tool printed it, such as `2.39.5 (Apple Git-154)`.
fn version(text: &str) -> Option<ToolVersion> {
    let number = text.split_whitespace().next()?;
    let parts: Option<Vec<u64>> = number.split('.').map(|part| part.parse().ok()).collect();
    parts.filter(|parts| !parts.is_empty()).map(ToolVersion)
}

/// What is wrong with a checkout no repository lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unregistered {
    /// Its `gitdir` still leads to an admin directory, so the checkout moved.
    Moved,
    /// Its `gitdir` leads nowhere.
    TargetGone,
}

/// A registration that needs a person, for [`registration_advice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anomaly<'path> {
    /// Registered, and its checkout isn't there.
    Missing,
    /// A checkout at this path that no repository lists.
    Unregistered(&'path Path, Unregistered),
    /// Registered, and what is there doesn't agree.
    Invalid,
}

/// What to do about `anomaly`.
///
/// `prune` drops a registration's admin directory, so only a missing checkout is offered it:
/// an invalid one may still be on disk. A directory's name can come from a repository or an
/// agent and a person pastes these commands, so it goes in the sentence and never in a
/// snippet.
#[must_use]
pub fn registration_advice(anomaly: Anomaly<'_>) -> Advice {
    let (title, text, snippets) = match anomaly {
        Anomaly::Missing => (
            "A worktree whose checkout is gone",
            "Git still lists this worktree, and its directory isn't there. It may be on a disk \
             that isn't mounted, or it may have moved, so check that first. The dry run shows \
             what git would drop. Without it, `git gc` prunes the registration after 3 months."
                .to_owned(),
            vec!["git worktree prune --dry-run --verbose".to_owned()],
        ),
        Anomaly::Unregistered(path, Unregistered::Moved) => (
            "A worktree that moved",
            format!(
                "The repository of the checkout at {} no longer lists it there, and its link to \
                 the repository still works. Run the command from inside that directory: repair \
                 points git at the new path. It may hold commits found nowhere else.",
                path.display()
            ),
            vec!["git worktree repair".to_owned()],
        ),
        Anomaly::Unregistered(_, Unregistered::TargetGone) => (
            "A checkout that lost its repository",
            "What this checkout's `.git` file points to is gone, so git can't say what it holds. \
             Review it by hand: it may hold work found nowhere else."
                .to_owned(),
            Vec::new(),
        ),
        Anomaly::Invalid => (
            "A worktree git can't read",
            "Git lists this worktree, and what is there doesn't match the registration. Review \
             it by hand, and don't prune: that drops the admin directory of a checkout still \
             on disk."
                .to_owned(),
            Vec::new(),
        ),
    };
    Advice {
        title: title.to_owned(),
        text,
        snippets,
        references: vec!["https://git-scm.com/docs/git-worktree".to_owned()],
    }
}
