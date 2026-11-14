//! Git worktrees as an integration describes them (`docs/06-adapters-and-rules.md` §4).

use std::fmt;
use std::path::{Path, PathBuf};

use crate::cancel::CancelToken;
use crate::native::{ResolvedTool, ToolGap};
use crate::probe::FileIdentity;
use crate::tracked::GitGap;

named! {
    /// Which agent's location a worktree is in, from its location alone. Display only: a
    /// branch name proves nothing, and the label never feeds eligibility.
    AgentLabel {
        /// Claude Code.
        ClaudeCode = "claude-code",
        /// Codex.
        Codex = "codex",
        /// Cursor.
        Cursor = "cursor",
        /// Conductor.
        Conductor = "conductor",
        /// Herdr.
        Herdr = "herdr",
        /// Gemini CLI.
        GeminiCli = "gemini-cli",
        /// `OpenCode`.
        OpenCode = "opencode",
        /// No agent's known location, such as a root an agent's setting moved.
        Unknown = "unknown",
    }
}

named! {
    /// How a worktree's registration and its checkout relate.
    RegistrationState {
        /// Registered, and its checkout is there.
        RegisteredPresent = "registered-present",
        /// Registered, and its checkout isn't there.
        RegisteredMissing = "registered-missing",
        /// A checkout no repository lists.
        PresentUnregistered = "present-unregistered",
        /// Registered, and what is there doesn't agree with the registration.
        RegisteredInvalid = "registered-invalid",
    }
}

/// A git object name, as git prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectId(pub String);

/// What a worktree has checked out, past its commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checkout {
    /// This branch, by its short name.
    Branch(String),
    /// A commit, with no branch.
    Detached,
    /// Git printed neither, as for a bare repository.
    Unnamed,
}

/// One record of `git worktree list --porcelain -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// Its path, exactly as printed.
    pub path: PathBuf,
    /// Its commit. `None` for a bare repository, or one with no commit yet.
    pub head: Option<ObjectId>,
    /// What it has checked out.
    pub checkout: Checkout,
    /// Why it is locked, when it is. The reason may be empty.
    pub locked: Option<String>,
    /// Whether git would prune it. Read by presence: git's reason is translated.
    pub prunable: bool,
    /// Whether this is a bare repository.
    pub bare: bool,
}

/// What ties a worktree to its repository, which apply compares with one built fresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeWitness {
    /// The repository's common directory.
    pub common: FileIdentity,
    /// This worktree's admin directory in it.
    pub admin: FileIdentity,
    /// The checkout's `.git` file.
    pub gitfile: FileIdentity,
    /// The admin directory's `gitdir` file.
    pub gitdir_file: FileIdentity,
    /// The commit it has checked out.
    pub head: ObjectId,
    /// Its branch, when it has one.
    pub branch: Option<String>,
}

/// What git's integration read about one worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeFacts {
    /// Its registration, as listed.
    pub registration: Registration,
    /// How that relates to what is there.
    pub state: RegistrationState,
    /// The repository's main worktree.
    pub repository: PathBuf,
    /// Its admin directory, when it has one.
    pub admin: Option<PathBuf>,
    /// What apply compares, or why it couldn't be built.
    pub witness: Result<WorktreeWitness, GitGap>,
    /// The ignored paths `status` prints, relative to the checkout, which a scan settles
    /// against its findings, or why git didn't say.
    pub ignored: Result<Vec<PathBuf>, GitGap>,
    /// The `git` a removal would run, or why there is none.
    pub tool: Result<ResolvedTool, ToolGap>,
    /// Whose location it is in.
    pub agent: AgentLabel,
}

/// Where git places a worktree, as `rev-parse` printed it from there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreePlace {
    /// Its git directory: its admin directory when it is linked.
    pub git_dir: PathBuf,
    /// Its repository's common directory.
    pub common_dir: PathBuf,
    /// The top of its work tree.
    pub top: PathBuf,
}

named! {
    /// Which keys of a repository's config stopped kipple from asking git more. Only the
    /// class is ever shown, because a value can hold a credential.
    ConfigClass {
        /// A clean, smudge or process filter, which `status` would run.
        Filter = "filter",
        /// A partial clone, whose rename detection would fetch.
        PartialClone = "partial-clone",
        /// `core.worktree`, which moves `status` onto another tree.
        WorkTree = "work-tree",
        /// A ref storage that moves per-worktree state out of the admin directory.
        RefStorage = "ref-storage",
    }
}

/// Whether a repository's config lets kipple ask git about a checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigGate {
    /// Nothing in it would run a program or move what is asked about.
    Clear,
    /// It holds a key of this class.
    Blocked(ConfigClass),
}

/// What a worktree is, row by row, each proven on its own.
///
/// `git worktree remove` exits 0 and deletes what its own refusal doesn't cover, so its
/// refusal proves nothing. Every row is a fact, and an `Err` is that fact unknown, which
/// holds the worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeState {
    /// Where git places it. [`GitGap::Unplaced`] says the registration its `.git` leads to
    /// names a checkout that isn't there, so this one moved rather than being a copy.
    pub place: Result<WorktreePlace, GitGap>,
    /// How its registration and its checkout relate.
    pub registration: Result<RegistrationState, GitGap>,
    /// Whether it has a submodule, populated or not.
    pub submodules: Result<bool, GitGap>,
    /// Whether its config lets kipple ask further.
    pub config_gate: Result<ConfigGate, GitGap>,
    /// How many paths `status` lists as changed.
    pub tracked_changes: Result<u64, GitGap>,
    /// How many paths `status` lists as untracked.
    pub untracked_files: Result<u64, GitGap>,
    /// The ignored paths `status` prints, relative to the checkout. A scan decides whether
    /// each is exactly a classified finding.
    pub ignored: Result<Vec<PathBuf>, GitGap>,
    /// How many paths `ls-files -v` tags as anything but plainly tracked: edits `status`
    /// doesn't show.
    pub hidden_changes: Result<u64, GitGap>,
    /// Whether a rebase, bisect, cherry-pick, revert or merge is in progress.
    pub operation_in_progress: Result<bool, GitGap>,
    /// How many refs its admin directory holds.
    pub per_worktree_refs: Result<u64, GitGap>,
    /// Whether `index.lock` or `HEAD.lock` is in its admin directory.
    pub index_locked: Result<bool, GitGap>,
    /// Whether a local branch holds its HEAD.
    pub head_reachable: Result<bool, GitGap>,
    /// What apply compares.
    pub witness: Result<WorktreeWitness, GitGap>,
}

impl WorktreeState {
    /// A worktree of which nothing could be established, for `gap`.
    #[must_use]
    pub const fn unknown(gap: GitGap) -> Self {
        Self {
            place: Err(gap),
            registration: Err(gap),
            submodules: Err(gap),
            config_gate: Err(gap),
            tracked_changes: Err(gap),
            untracked_files: Err(gap),
            ignored: Err(gap),
            hidden_changes: Err(gap),
            operation_in_progress: Err(gap),
            per_worktree_refs: Err(gap),
            index_locked: Err(gap),
            head_reachable: Err(gap),
            witness: Err(gap),
        }
    }
}

/// Asks git about a repository's worktrees. Read-only: it runs git through the platform's
/// runner, isolated as [`TrackedQuery`](crate::TrackedQuery) is, and never runs a program a
/// repository's config names.
pub trait WorktreeQuery: fmt::Debug + Send + Sync {
    /// Every registration of the repository whose main worktree is `repository`, the main
    /// one first.
    ///
    /// # Errors
    /// When git couldn't answer in full: never "no worktrees".
    /// [`GitGap::Unplaced`] says `<repository>/.git` is a file naming nothing that is there,
    /// so there is no repository to ask.
    fn list(&self, repository: &Path, cancel: &CancelToken) -> Result<Vec<Registration>, GitGap>;

    /// What the worktree `registration` names is now.
    fn inspect(&self, registration: &Registration, cancel: &CancelToken) -> WorktreeState;
}
