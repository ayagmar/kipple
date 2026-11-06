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

/// What an agent removes by itself from where it keeps worktrees, when it says so. Each one
/// sweeps by its own rules, so a person should know before keeping or removing one.
pub(crate) const fn sweep(label: AgentLabel) -> Option<&'static str> {
    match label {
        AgentLabel::ClaudeCode => Some(
            "Claude Code removes worktrees it made for subagents and background sessions past \
             cleanupPeriodDays, unless they have changed or untracked files or unpushed commits.",
        ),
        AgentLabel::Codex => Some(
            "Codex keeps your 15 most recent worktrees it manages, and takes a snapshot before \
             deleting one.",
        ),
        AgentLabel::Cursor => Some(
            "Cursor keeps 25 worktrees per machine by default, and that cap covers a plain `git \
             worktree add` under its root, so one you made yourself can be deleted too.",
        ),
        AgentLabel::Conductor
        | AgentLabel::Herdr
        | AgentLabel::GeminiCli
        | AgentLabel::OpenCode
        | AgentLabel::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use kipple_core::AgentLabel;

    use super::sweep;

    /// Catches Cursor's cap losing its warning: it deletes worktrees it didn't make, which is
    /// the one thing a person can't guess from the location.
    #[test]
    fn cursors_sweep_warns_that_it_reaches_a_plain_worktree() {
        let words = sweep(AgentLabel::Cursor).unwrap_or_default();

        assert!(
            words.contains("git worktree add"),
            "expected the warning, found {words:?}"
        );
    }
}
