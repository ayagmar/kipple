//! The closed set of native operations (`docs/04-safety-model.md` §6).

use std::fmt;
use std::path::PathBuf;

use crate::capability::NativeTool;
use crate::probe::FileIdentity;

named! {
    /// A first-party native operation: a tool's own command, reviewed as a semantic plan.
    NativeKind {
        /// `git worktree remove`.
        GitWorktreeRemove = "git-worktree-remove",
        /// `npm cache clean`.
        NpmCacheClean = "native.npm-cache-clean",
    }
}

named! {
    /// Why a native tool can't be used.
    ToolGap {
        /// It wasn't found.
        Missing = "tool-missing",
        /// It was found, and can't be read or identified.
        Unreadable = "tool-unreadable",
        /// Its version couldn't be read.
        UnknownVersion = "tool-unknown-version",
        /// Its version isn't one kipple verified.
        UnverifiedVersion = "tool-unverified-version",
    }
}

/// A file a native tool runs from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFile {
    /// Where it is.
    pub path: PathBuf,
    /// Its identity, which a run compares before it starts.
    pub identity: FileIdentity,
}

/// A native tool as found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTool {
    /// Which tool.
    pub tool: NativeTool,
    /// Its executable.
    pub path: PathBuf,
    /// Its executable's identity.
    pub identity: FileIdentity,
    /// Its version, as it reported it.
    pub version: String,
    /// What runs it, when it is a script.
    pub runtime: Option<ToolFile>,
}

/// What an integration read about the target of a semantic operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticFacts {
    /// The operation it is for.
    pub kind: NativeKind,
    /// The tool that would run it, or why there is none.
    pub tool: Result<ResolvedTool, ToolGap>,
}
