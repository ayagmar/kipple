//! Where a scan's findings that depend on each other are decided: after an integration's
//! findings are measured, and after every integration ran. The first changes nothing yet,
//! so a release stays `Unsettled`, which holds.
//!
//! A group with a member left unmeasured stays `Unsettled`: which release may go depends on
//! all of them, so the others can't be decided without it.
//!
//! The second settles worktrees. `git worktree remove` deletes ignored files without a
//! word, so a worktree's ignored path counts as classified only when it is exactly a
//! finding some other rule would remove on its own evidence. Until this runs the fact is
//! unknown, which holds the worktree.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{Engine, Finding, FindingId, Found, Owner, Shared};
use crate::facts::{Evaluated, unmet};
use crate::integration::IntegrationId;
use crate::policy::{Facts, HeldPath, Proposal, Tracked};
use crate::protect::Unresolved;
use crate::supplied::{FactValue, Subject};

/// How many of a worktree's ignored paths aren't exactly a classified finding.
const UNCLASSIFIED: &str = "worktree.unclassified_ignored";

impl Engine {
    /// Settles what one integration's findings decide among themselves, before any
    /// finding that isn't [`Plain`](crate::Subject::Plain) is announced as measured.
    /// `unmeasured` are the findings a cancel stopped short of, each with what its
    /// integration supplied, so it can be placed in its group: they have no facts in
    /// `findings`.
    #[expect(
        clippy::unused_self,
        reason = "the seam the release cards fill, reading this engine's ports"
    )]
    pub(super) const fn settle_integration(
        &self,
        _findings: &mut [Finding],
        _unmeasured: &[Found],
        _owner: Owner<'_>,
        _shared: Shared<'_>,
    ) {
    }

    /// Settles what reads other integrations' findings, after they all ran, and returns
    /// the findings it changed, which are announced again.
    pub(super) fn settle_scan(
        &self,
        findings: &mut [Finding],
        shared: Shared<'_>,
    ) -> Vec<FindingId> {
        let classified = self.classified(findings, shared);
        let mut changed = Vec::new();
        for finding in findings {
            let Some(unclassified) = unclassified(finding, &classified) else {
                continue;
            };
            self.supply_unclassified(finding, unclassified);
            changed.push(finding.id);
        }
        changed
    }

    /// The paths a worktree's ignored file may be, because a rule would remove what is
    /// there on its own evidence.
    fn classified(&self, findings: &[Finding], shared: Shared<'_>) -> BTreeSet<PathBuf> {
        let ignored: BTreeSet<PathBuf> = findings.iter().flat_map(ignored_paths).collect();
        findings
            .iter()
            .filter(|finding| ignored.contains(&finding.path))
            .filter(|finding| self.classifies(finding, shared))
            .map(|finding| finding.path.clone())
            .collect()
    }

    /// Whether `finding` stands for the ignored path at its own path.
    fn classifies(&self, finding: &Finding, shared: Shared<'_>) -> bool {
        finding.integration != IntegrationId::GENERIC
            && finding.facts.as_ref().is_some_and(|facts| {
                stands_on_its_own(facts) && !self.in_state_root(finding, facts, shared)
            })
    }

    /// Whether a state root reaches `finding`, or can't be shown not to. Artifact rules
    /// don't reach inside an agent's state: such a finding stays held, so it answers for
    /// nothing, whichever owner is supported.
    fn in_state_root(&self, finding: &Finding, facts: &Facts, shared: Shared<'_>) -> bool {
        let fs = self.ports.fs.as_ref();
        let holds = |root: &Path| {
            let held = HeldPath {
                forms: vec![root.to_owned()],
                lineage: shared.identities.lineage(fs, root),
                note: None,
            };
            held.holds(&finding.path, &facts.lineage)
        };
        let declared = self.integrations.iter().flat_map(|registered| {
            let state = registered.descriptor.state.iter();
            state.map(|declaration| (registered.descriptor.id, declaration))
        });
        declared
            .flat_map(|(owner, declaration)| {
                let paths = shared.roots.all(declaration.root).iter();
                paths.map(move |root| (owner, declaration, root))
            })
            .any(|(owner, declaration, root)| {
                root.as_ref().map_or_else(
                    |_| {
                        declaration.unresolved == Unresolved::HoldAll
                            || owner == finding.integration
                    },
                    |root| holds(&root.path),
                )
            })
    }

    /// Records `count` on the worktree `finding`, and evaluates its rule's predicates
    /// again, since the fact was unknown when they first ran.
    fn supply_unclassified(&self, finding: &mut Finding, count: u64) {
        let Some(mut facts) = finding.facts.take() else {
            return;
        };
        let values = &mut facts.supplied.values;
        values.insert(UNCLASSIFIED, FactValue::Count(count));
        let registered = self
            .integrations
            .iter()
            .find(|registered| registered.descriptor.id == finding.integration);
        let rule = registered.and_then(|registered| {
            let rule = registered.rules.iter().find(|r| r.id == finding.rule)?;
            Some((rule, registered.descriptor.facts))
        });
        if let Some((rule, specs)) = rule {
            let evaluated = Evaluated {
                evidence: &finding.evidence,
                size: finding.size,
                facts: &facts,
            };
            facts.unmet = unmet(rule, specs, evaluated);
        }
        finding.facts = Some(facts);
    }
}

/// The ignored paths of the worktree `finding`, if it is one and git named them.
fn ignored_paths(finding: &Finding) -> Vec<PathBuf> {
    let Some(Subject::Worktree(worktree)) = subject(finding) else {
        return Vec::new();
    };
    let ignored = worktree.ignored.as_deref().unwrap_or_default();
    ignored.iter().map(|path| finding.path.join(path)).collect()
}

fn subject(finding: &Finding) -> Option<&Subject> {
    Some(&finding.facts.as_ref()?.supplied.subject)
}

/// How many ignored paths of the worktree `finding` aren't in `classified`. `None` when it
/// isn't a worktree or git didn't name them, which leaves the fact unknown.
fn unclassified(finding: &Finding, classified: &BTreeSet<PathBuf>) -> Option<u64> {
    let Subject::Worktree(worktree) = subject(finding)? else {
        return None;
    };
    let ignored = worktree.ignored.as_ref().ok()?;
    let left = ignored
        .iter()
        .filter(|path| !classified.contains(&finding.path.join(path)));
    Some(left.map(|_| 1).sum())
}

/// Whether what a finding's rule would remove stands on its own: a rule that removes, a
/// layout it verified, no worktree of its own, and nothing inside that is a repository or
/// that git tracks. A walk or a question that didn't finish can't show that, so it doesn't.
const fn stands_on_its_own(facts: &Facts) -> bool {
    let removes = !matches!(facts.proposal, Proposal::Report | Proposal::Advice);
    let repository_free = matches!(
        facts.tracked,
        Tracked::NoRepository | Tracked::Asked { tracked: 0, .. }
    );
    let worktree = matches!(facts.supplied.subject, Subject::Worktree(_));
    removes && facts.layout_verified && repository_free && !worktree
}
