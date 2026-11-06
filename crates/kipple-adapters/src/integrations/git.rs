//! Git worktrees of any agent (`docs/06-adapters-and-rules.md` §4): the descriptor, and what
//! each registration state advises. It lists no repository yet, so it finds nothing.

use std::path::Path;

use kipple_core::{
    Advice, FindingSink, Integration, IntegrationDescriptor, IntegrationError, IntegrationId,
    IntegrationSelector, Lens, Platform, ScanContext, SymbolicRoot,
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
            verified: &[],
            consequences: &[],
            facts: &[],
            state: &[],
            evidences: &[],
            revision: 1,
        }
    }

    fn discover(
        &self,
        _: &ScanContext<'_>,
        _: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        Ok(())
    }
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

/// What to do about `anomaly`. `prune` drops a registration's admin directory, so only a
/// missing checkout is offered it: an invalid one may still be on disk.
#[must_use]
pub fn registration_advice(anomaly: Anomaly<'_>) -> Advice {
    let (title, text, snippets) = match anomaly {
        Anomaly::Missing => (
            "A worktree whose checkout is gone",
            "Git still lists this worktree, and its directory isn't there. It may be on a disk \
             that isn't mounted, or it may have moved, so check that first. The dry run shows \
             what git would drop. Without it, `git gc` prunes the registration after 3 months.",
            vec!["git worktree prune --dry-run --verbose".to_owned()],
        ),
        Anomaly::Unregistered(path, Unregistered::Moved) => (
            "A worktree that moved",
            "This checkout's repository no longer lists it here, and its link to the repository \
             still works. Repair points git at the new path. It may hold commits found nowhere \
             else.",
            vec![format!("git worktree repair {}", path.display())],
        ),
        Anomaly::Unregistered(_, Unregistered::TargetGone) => (
            "A checkout that lost its repository",
            "What this checkout's `.git` file points to is gone, so git can't say what it holds. \
             Review it by hand: it may hold work found nowhere else.",
            Vec::new(),
        ),
        Anomaly::Invalid => (
            "A worktree git can't read",
            "Git lists this worktree, and what is there doesn't match the registration. Review \
             it by hand, and don't prune: that drops the admin directory of a checkout still \
             on disk.",
            Vec::new(),
        ),
    };
    Advice {
        title: title.to_owned(),
        text: text.to_owned(),
        snippets,
        references: vec!["https://git-scm.com/docs/git-worktree".to_owned()],
    }
}
