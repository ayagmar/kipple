//! Git worktrees as an integration describes them (`docs/06-adapters-and-rules.md` §4).

use std::fmt;
use std::path::PathBuf;

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
    /// The `git` a removal would run, or why there is none.
    pub tool: Result<ResolvedTool, ToolGap>,
    /// Whose location it is in.
    pub agent: AgentLabel,
}
