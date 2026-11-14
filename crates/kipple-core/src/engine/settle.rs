//! Where a scan's findings that depend on each other are decided: after an integration's
//! findings are measured, and after every integration ran.
//!
//! The first settles releases (spec M4 pinned 12 to 14). A release runs when a process's
//! executable lies under it, which its activity already shows, or when a process runs a
//! file with the identity of one of its files from anywhere, as a hard link does; that is
//! recorded as its activity too. Then [`settle`] decides each release among the others of
//! its group. Apply settles a release again the same way, from what its integration reads
//! again and a new process snapshot. A group with a member left unmeasured stays
//! `Unsettled`: which release may go depends on all of them, so the others can't be
//! decided without it.
//!
//! The second settles worktrees. `git worktree remove` deletes ignored files without a
//! word, so a worktree's ignored path counts as classified only when it is exactly a
//! finding some other rule would remove on its own evidence. Until this runs the fact is
//! unknown, which holds the worktree.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{Engine, Finding, FindingId, Found, Owner, Shared};
use crate::activity::{Processes, Scope};
use crate::facts::{Evaluated, unmet};
use crate::integration::IntegrationId;
use crate::policy::{Activity, ActivityGap, Facts, HeldPath, Proposal, Seen, Tracked};
use crate::protect::Unresolved;
use crate::retention::{ReleaseFacts, ReleaseView, settle};
use crate::supplied::{FactValue, Subject};

/// How many of a worktree's ignored paths aren't exactly a classified finding.
const UNCLASSIFIED: &str = "worktree.unclassified_ignored";

impl Engine {
    /// Settles what one integration's findings decide among themselves, before any
    /// finding that isn't [`Plain`](crate::Subject::Plain) is announced as measured.
    /// `unmeasured` are the findings a cancel stopped short of, each with what its
    /// integration supplied, so it can be placed in its group: they have no facts in
    /// `findings`.
    pub(super) fn settle_integration(
        &self,
        findings: &mut [Finding],
        unmeasured: &[Found],
        _owner: Owner<'_>,
        shared: Shared<'_>,
    ) {
        let scope = Scope {
            fs: self.ports.fs.as_ref(),
            identities: shared.identities,
            home: shared.home,
            roots: shared.roots,
        };
        let unmeasured: Vec<&ReleaseFacts> = unmeasured
            .iter()
            .filter_map(|found| match &found.supplied.subject {
                Subject::Release(release) => Some(release),
                Subject::Plain | Subject::Worktree(_) | Subject::Semantic(_) => None,
            })
            .collect();
        self.settle_releases(findings, &unmeasured, shared.processes, scope);
    }

    /// Settles every release among `findings`, with `unmeasured` in their groups too, from
    /// `processes`: what the scan does once an integration's findings are measured, and
    /// what apply does again for one release's component.
    pub(super) fn settle_releases(
        &self,
        findings: &mut [Finding],
        unmeasured: &[&ReleaseFacts],
        processes: &Processes,
        scope: Scope<'_>,
    ) {
        for finding in findings.iter_mut() {
            self.mark_running(finding, processes, scope);
        }
        let unknown = Activity::Unknown(ActivityGap::Unsupported);
        let mut views = Vec::new();
        let mut settled = Vec::new();
        for (at, finding) in findings.iter().enumerate() {
            let Some(facts) = &finding.facts else {
                continue;
            };
            let Subject::Release(release) = &facts.supplied.subject else {
                continue;
            };
            views.push(ReleaseView {
                facts: release,
                measured: finding.size.map(|size| size.completeness),
                activity: &facts.activity,
            });
            settled.push(at);
        }
        views.extend(unmeasured.iter().map(|release| ReleaseView {
            facts: release,
            measured: None,
            activity: &unknown,
        }));
        let decided: Vec<_> = settled.into_iter().zip(settle(&views)).collect();
        for (at, settlement) in decided {
            if let Some(facts) = findings.get_mut(at).and_then(|f| f.facts.as_mut()) {
                facts.settlement = settlement;
            }
        }
    }

    /// Records a process running a file of the release `finding` from anywhere as its
    /// activity. A file whose identity can't be read could be the one run, so its activity
    /// is then unknown.
    fn mark_running(&self, finding: &mut Finding, processes: &Processes, scope: Scope<'_>) {
        let Some(facts) = finding.facts.as_mut() else {
            return;
        };
        let Subject::Release(release) = &facts.supplied.subject else {
            return;
        };
        if !matches!(
            facts.activity,
            Activity::Idle | Activity::IdleExceptSystem { .. }
        ) {
            return;
        }
        let fs = self.ports.fs.as_ref();
        let identities: Option<Vec<_>> = release
            .files
            .iter()
            .map(|file| fs.metadata(file).ok()?.identity)
            .collect();
        let Some(identities) = identities else {
            facts.activity = Activity::Unknown(ActivityGap::Unsupported);
            return;
        };
        let probe = self.ports.processes.as_ref();
        if let Some(pid) = processes.running(probe, scope, &identities) {
            let seen = Seen::Executable(finding.path.clone());
            facts.activity = Activity::InUse { pid, seen };
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Method;
    use crate::policy::{Activity, Lineage, Modified};
    use crate::retention::Settlement;
    use crate::supplied::Supplied;

    fn verified(proposal: Proposal) -> Facts {
        Facts {
            proposal,
            layout_verified: true,
            consequence: None,
            tracked: Tracked::NoRepository,
            activity: Activity::Idle,
            modified: Modified::Ago(std::time::Duration::ZERO),
            recent_days: None,
            lineage: Lineage::default(),
            supplied: Supplied::default(),
            unmet: Vec::new(),
            settlement: Settlement::Unsettled,
            integration: IntegrationId("test"),
            host: None,
        }
    }

    /// Catches a report standing for proof. No built-in report rule reaches a project with a
    /// verified layout and nothing tracked today, so a scan can't show this yet: the first
    /// that does would let a worktree's ignored path go on a finding that only reports it.
    #[test]
    fn only_a_rule_that_removes_stands_on_its_own() {
        for reports in [Proposal::Report, Proposal::Advice] {
            let facts = verified(reports);
            assert!(
                !stands_on_its_own(&facts),
                "{:?} only reports",
                facts.proposal
            );
        }
        let removes = verified(Proposal::Remove(vec![Method::Delete]));
        assert!(stands_on_its_own(&removes), "a rule that removes stands");
    }
}
