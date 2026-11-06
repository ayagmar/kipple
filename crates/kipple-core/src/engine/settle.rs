//! Where a scan's findings that depend on each other are decided: after an integration's
//! findings are measured, and after every integration ran. Both entries change nothing yet,
//! so a release stays `Unsettled` and what they would read stays unknown, which holds.

use super::{Engine, Finding, FindingId, Owner, Shared};

impl Engine {
    /// Settles what one integration's findings decide among themselves, before any
    /// finding that isn't [`Plain`](crate::Subject::Plain) is announced as measured.
    #[expect(
        clippy::unused_self,
        reason = "the seam the release cards fill, reading this engine's ports"
    )]
    pub(super) const fn settle_integration(
        &self,
        _findings: &mut [Finding],
        _owner: Owner<'_>,
        _shared: Shared<'_>,
    ) {
    }

    /// Settles what reads other integrations' findings, after they all ran, and returns
    /// the findings it changed, which are announced again.
    #[expect(
        clippy::unused_self,
        reason = "the seam the worktree card fills, reading this engine's ports"
    )]
    pub(super) const fn settle_scan(
        &self,
        _findings: &mut [Finding],
        _shared: Shared<'_>,
    ) -> Vec<FindingId> {
        Vec::new()
    }
}
