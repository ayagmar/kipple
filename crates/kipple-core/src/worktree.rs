//! Git worktrees as an integration describes them (`docs/06-adapters-and-rules.md` §4).

use std::fmt;

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
