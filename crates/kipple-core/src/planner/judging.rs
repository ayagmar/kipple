//! Judging one finding of a scan for removal, the same way at planning and at apply: the
//! host's protections, its bucket and those of the findings it would take with it, its
//! rule and method, and where it is.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use super::{Judge, Unjudged};
use crate::engine::{Finding, ScanReport};
use crate::frozen::RelPath;
use crate::integration::IntegrationId;
use crate::native::{NativeCommand, NativeKind, NativeSpec};
use crate::outcome::Skip;
use crate::plan::{
    Anchor, Execution, ItemMethod, Judgement, JudgementField, Removal, VerifiedRoot,
};
use crate::policy::{Facts, Hold, Lineage, Proposal, Verdict, starts_with};
use crate::probe::FileIdentity;
use crate::protect::Protection;
use crate::rule::RuleId;
use crate::supplied::Subject;
use crate::tracked::OwnGit;

/// A scan with every finding's verdict, indexed for the checks that look past one
/// finding.
#[derive(Debug)]
pub(crate) struct Scene<'scan> {
    scan: &'scan ScanReport,
    /// Each finding's verdict under the current policy, `None` when it wasn't measured.
    verdicts: Vec<Option<Verdict>>,
    /// What of the host's protects each finding, if anything does.
    hosts: Vec<Option<Protection>>,
    /// Findings the host protects, that are Protected or that weren't measured.
    sheltered: Placed,
    /// Findings in Review, whose cautions every item at or above them takes.
    cautioned: Placed,
    /// Every finding, by its path and rule.
    by_place: HashMap<Place<'scan>, usize>,
}

/// Where a finding is and which rule found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Place<'scan> {
    path: &'scan Path,
    rule: &'scan RuleId,
}

/// Some of a scan's findings, by where they are.
#[derive(Debug, Default)]
struct Placed {
    /// By the identity of every directory they are at or inside.
    by_identity: HashMap<FileIdentity, Vec<usize>>,
    /// Those whose ancestry isn't all known, matched by path instead.
    unplaced: Vec<usize>,
}

impl Placed {
    fn add(&mut self, index: usize, lineage: Option<&Lineage>) {
        for identity in lineage
            .into_iter()
            .flat_map(|lineage| lineage.0.iter().flatten())
        {
            self.by_identity.entry(*identity).or_default().push(index);
        }
        if !lineage.is_some_and(Lineage::is_known) {
            self.unplaced.push(index);
        }
    }

    /// Those at `finding`'s directory or inside it, by identity or by path.
    fn within<'placed>(
        &'placed self,
        scan: &'placed ScanReport,
        finding: &'placed Finding,
        facts: &Facts,
    ) -> impl Iterator<Item = usize> + 'placed {
        let by_identity = facts
            .lineage
            .own()
            .and_then(|own| self.by_identity.get(&own))
            .into_iter()
            .flatten();
        let by_path = self.unplaced.iter().filter(move |other| {
            scan.findings
                .get(**other)
                .is_some_and(|other| starts_with(&other.path, &finding.path))
        });
        by_identity.chain(by_path).copied()
    }
}

/// A finding judged eligible now, with everything planning or revalidation needs.
#[derive(Debug)]
pub(crate) struct Judged<'scan> {
    /// The finding.
    pub(crate) finding: &'scan Finding,
    /// Its facts.
    pub(crate) facts: &'scan Facts,
    /// Its verdict with every caution of a finding at the same directory or inside it:
    /// never Protected.
    pub(crate) verdict: Verdict,
    /// What its rule proposes.
    pub(crate) proposal: Proposal,
    /// What it is judged under, with its method.
    pub(crate) judgement: Judgement,
    /// How it is to run.
    pub(crate) execution: Execution,
    /// Where it is.
    pub(crate) anchor: Anchor,
    /// Its own identity.
    pub(crate) own: FileIdentity,
}

impl Judged<'_> {
    /// How it is to run.
    pub(crate) const fn method(&self) -> ItemMethod {
        self.judgement.method
    }

    /// Whether its own top-level `.git` stands for a repository inside it.
    pub(crate) fn own_git(&self) -> OwnGit {
        OwnGit::of(
            &self.proposal,
            &self.facts.supplied.subject,
            self.facts.integration,
        )
    }
}

impl<'scan> Scene<'scan> {
    /// `scan`, judged under `judge`'s policy.
    pub(crate) fn new(scan: &'scan ScanReport, judge: &Judge<'_>) -> Self {
        let mut scene = Self {
            scan,
            verdicts: Vec::with_capacity(scan.findings.len()),
            hosts: Vec::with_capacity(scan.findings.len()),
            sheltered: Placed::default(),
            cautioned: Placed::default(),
            by_place: HashMap::new(),
        };
        let owners = scan.owners();
        for (index, finding) in scan.findings.iter().enumerate() {
            let verdict = finding.verdict(judge.policy);
            let lineage = finding.facts.as_ref().map(|facts| &facts.lineage);
            let host = lineage.and_then(|lineage| {
                let integration = finding.integration;
                judge
                    .protections
                    .check(&finding.path, lineage, integration, &owners)
            });
            match (&host, &verdict) {
                (None, Some(Verdict::Safe)) => {}
                (None, Some(Verdict::Review(_))) => scene.cautioned.add(index, lineage),
                (Some(_), _) | (None, Some(Verdict::Protected(_)) | None) => {
                    scene.sheltered.add(index, lineage);
                }
            }
            scene.verdicts.push(verdict);
            scene.hosts.push(host);
            let place = Place {
                path: &finding.path,
                rule: &finding.rule,
            };
            scene.by_place.entry(place).or_insert(index);
        }
        scene
    }

    /// The scan.
    pub(crate) const fn scan(&self) -> &'scan ScanReport {
        self.scan
    }

    /// The finding at `path` found by the rule `rule`, if the scan has it.
    pub(crate) fn find(&self, path: &Path, rule: &RuleId) -> Option<usize> {
        self.by_place.get(&Place { path, rule }).copied()
    }

    /// Judges the finding at `index` for removal now, or says why it can't be removed.
    ///
    /// In order: it must have been measured; the host must not protect it; it must not
    /// be Protected, nor any other finding at it or inside it; its rule must still exist
    /// and be first-party; the method chosen must be one its rule offers; its root, its
    /// own identity and its directory's must be known; and a native item's command must
    /// be one its subject can build.
    pub(crate) fn judge(&self, index: usize, judge: &Judge<'_>) -> Result<Judged<'scan>, Skip> {
        let finding = self
            .scan
            .findings
            .get(index)
            .ok_or(Skip::Unjudged(Unjudged::NotFound))?;
        let facts = finding
            .facts
            .as_ref()
            .ok_or(Skip::Unjudged(Unjudged::Unmeasured))?;
        if let Some(Some(why)) = self.hosts.get(index) {
            return Err(Skip::HostProtected(*why));
        }
        let own = match self.verdicts.get(index) {
            Some(Some(Verdict::Protected(holds))) => return Err(Skip::Protected(holds.clone())),
            Some(Some(own)) => own,
            Some(None) | None => return Err(Skip::Unjudged(Unjudged::Unmeasured)),
        };
        self.unsheltered(index, finding, facts)?;
        let rule = judge
            .rules
            .iter()
            .find(|rule| rule.id == finding.rule)
            .ok_or(Skip::Rejudged(JudgementField::Rule))?;
        if rule.integration == IntegrationId::GENERIC {
            return Err(Skip::NotGranted);
        }
        let choice = judge.methods.choice(&rule.id);
        let removal = choice
            .for_proposal(&facts.proposal)
            .map_err(|_| Skip::MethodNotOffered(choice.resolve()))?;
        let (anchor, own_identity) = self.anchor(finding, facts)?;
        let execution = match removal {
            Removal::Files(method) => Execution::Files(method),
            Removal::Native(kind) => {
                let subject = &facts.supplied.subject;
                Execution::Native(Box::new(native_spec(kind, subject, &anchor)?))
            }
        };
        Ok(Judged {
            finding,
            facts,
            verdict: self.standing(own, finding, facts),
            proposal: facts.proposal.clone(),
            judgement: Judgement::new(judge.policy, rule, facts, execution.method()),
            execution,
            anchor,
            own: own_identity,
        })
    }

    /// Fails when a finding other than `index`, at the same directory or inside it, is
    /// protected by the host, is Protected or wasn't measured.
    fn unsheltered(&self, index: usize, finding: &Finding, facts: &Facts) -> Result<(), Skip> {
        let Some(other) = self
            .sheltered
            .within(self.scan, finding, facts)
            .find(|other| *other != index)
        else {
            return Ok(());
        };
        if let Some(Some(why)) = self.hosts.get(other) {
            return Err(Skip::HostProtected(*why));
        }
        let path = self
            .scan
            .findings
            .get(other)
            .map(|other| other.path.clone())
            .unwrap_or_default();
        let holds = match self.verdicts.get(other) {
            Some(Some(Verdict::Protected(holds))) => holds.clone(),
            Some(Some(Verdict::Safe | Verdict::Review(_)) | None) | None => Vec::new(),
        };
        Err(Skip::HoldsProtected { path, holds })
    }

    /// `own` with every caution of the findings at `finding`'s directory or inside it,
    /// since removing it removes them too.
    fn standing(&self, own: &Verdict, finding: &Finding, facts: &Facts) -> Verdict {
        let mut cautions = BTreeSet::new();
        let verdicts = self
            .cautioned
            .within(self.scan, finding, facts)
            .filter_map(|index| self.verdicts.get(index).and_then(Option::as_ref))
            .chain([own]);
        for verdict in verdicts {
            if let Verdict::Review(found) = verdict {
                cautions.extend(found.iter().copied());
            }
        }
        if cautions.is_empty() {
            Verdict::Safe
        } else {
            Verdict::Review(cautions.into_iter().collect())
        }
    }

    /// Where `finding` is: below the deepest root its integration opened that holds it,
    /// with the identities that root, the finding and its directory had in the scan.
    fn anchor(&self, finding: &Finding, facts: &Facts) -> Result<(Anchor, FileIdentity), Skip> {
        let unanchored = Skip::Unjudged(Unjudged::Unanchored);
        let lineage = &facts.lineage;
        let own = lineage
            .own()
            .filter(|_| lineage.is_known())
            .ok_or_else(|| unanchored.clone())?;
        let root = self
            .scan
            .anchor_root(finding)
            .ok_or_else(|| unanchored.clone())?;
        let identity = root.identity.ok_or_else(|| unanchored.clone())?;
        let relative = finding
            .path
            .strip_prefix(&root.path)
            .ok()
            .and_then(|below| RelPath::of(below).ok())
            .ok_or_else(|| unanchored.clone())?;
        let parent = if relative.is_empty() {
            None
        } else {
            Some(lineage.0.get(1).copied().flatten().ok_or(unanchored)?)
        };
        let anchor = Anchor {
            root: VerifiedRoot {
                root: root.root,
                path: root.path.clone(),
                identity,
            },
            relative,
            parent,
        };
        Ok((anchor, own))
    }
}

/// The command for `kind` on the item at `anchor`, from what its integration read. Only
/// here is one built, and only from its subject: an `Err` in its tool or witness leaves
/// nothing to build, which holds the item.
fn native_spec(kind: NativeKind, subject: &Subject, anchor: &Anchor) -> Result<NativeSpec, Skip> {
    let (command, tool) = match (kind, subject) {
        (NativeKind::NpmCacheClean, Subject::Semantic(facts)) if facts.kind == kind => {
            let tool = facts.tool.clone().map_err(Skip::ToolChanged)?;
            let cache = cache_root(anchor)?;
            (NativeCommand::NpmCacheClean { cache }, tool)
        }
        (NativeKind::GitWorktreeRemove, Subject::Worktree(facts)) => {
            let tool = facts.tool.clone().map_err(Skip::ToolChanged)?;
            let witness = facts.witness.clone().map_err(Skip::Git)?;
            let checkout = anchor.clone();
            (NativeCommand::GitWorktreeRemove { checkout, witness }, tool)
        }
        (
            NativeKind::NpmCacheClean | NativeKind::GitWorktreeRemove,
            Subject::Plain | Subject::Release(_) | Subject::Semantic(_) | Subject::Worktree(_),
        ) => return Err(Skip::Protected(vec![Hold::SubjectMismatch])),
    };
    Ok(NativeSpec {
        command,
        tool,
        scope: anchor.clone(),
    })
}

/// The cache root npm is pointed at: the root holding the store at `item`, directly
/// below it. Anywhere else, the command would clean something the item isn't.
fn cache_root(item: &Anchor) -> Result<Anchor, Skip> {
    match item.relative.components() {
        [_] => Ok(Anchor {
            root: item.root.clone(),
            relative: RelPath::new(Vec::new()),
            parent: None,
        }),
        _ => Err(Skip::Unjudged(Unjudged::Unanchored)),
    }
}
