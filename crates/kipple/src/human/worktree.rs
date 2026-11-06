//! The words for git worktrees and the agents that leave them.

use kipple_core::AgentLabel;

/// Whose location a worktree is in. The location is all this says: it grants nothing.
const fn agent(label: AgentLabel) -> &'static str {
    match label {
        AgentLabel::ClaudeCode => "Claude Code's",
        AgentLabel::Codex => "Codex's",
        AgentLabel::Cursor => "Cursor's",
        AgentLabel::Conductor => "Conductor's",
        AgentLabel::Herdr => "Herdr's",
        AgentLabel::GeminiCli => "Gemini CLI's",
        AgentLabel::OpenCode => "OpenCode's",
        AgentLabel::Unknown => "no known agent's",
    }
}

/// The evidence a worktree finding rests on.
pub(crate) fn evidence(label: AgentLabel) -> String {
    format!("a git worktree, in {} location", agent(label))
}
