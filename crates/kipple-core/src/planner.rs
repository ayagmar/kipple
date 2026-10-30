//! Planning: the user's selection judged now, checked against the host's protections and
//! against the other items, then frozen, before review is shown
//! (`docs/04-safety-model.md` §2a, §3.2, §3.5, §3.9, §3.10).
//!
//! Each selected finding is judged as [`Scene::judge`] says. Findings selected at one
//! directory through two rules or paths are aliases: they become one item when they have
//! the same method and scope, and conflict otherwise. An item at or above another
//! conflicts with it. A conflict refuses both items until the review resolves it by
//! leaving one out. Only then is each item frozen through the [`Freezer`]: a target on a
//! file system kipple doesn't change, or whose method is trash where no trash can take
//! it, is refused, never planned with another method.

mod judging;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;

pub(crate) use judging::{Judged, Scene};

use crate::cancel::CancelToken;
use crate::engine::{FindingId, ScanReport};
use crate::frozen::FrozenTree;
use crate::mutation::{Executor, FileSystemClass, Freezer, LocalFileSystem};
use crate::outcome::{ApplyError, ErrorCode, Skip, Stop};
use crate::plan::{ItemId, MethodChoice, Plan, PlanEntry, Selection, Standing};
use crate::policy::{Policy, Verdict};
use crate::protect::Protections;
use crate::rule::{Method, Rule, RuleId};
use crate::trash::TrashBackend;

/// What decides how a finding is judged and removed now.
///
/// The rules, the policy, the methods chosen and the host's protections. Planning and
/// apply each take the current one, so a change between them is seen.
#[derive(Debug, Clone, Copy)]
pub struct Judge<'config> {
    /// Every rule loaded.
    pub rules: &'config [Rule],
    /// The configured policy.
    pub policy: &'config Policy,
    /// The methods chosen.
    pub methods: &'config Methods,
    /// The host's protected paths.
    pub protections: &'config Protections,
}

/// The removal methods chosen, from the command line and the config
/// (`docs/03-functional-spec.md` §6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Methods {
    /// `--method`.
    pub command_line: Option<Method>,
    /// Each `rules.<id>.method`.
    pub rules: BTreeMap<RuleId, Method>,
    /// `clean.method`.
    pub config: Option<Method>,
}

impl Methods {
    /// The choice for `rule`'s findings.
    #[must_use]
    pub fn choice(&self, rule: &RuleId) -> MethodChoice {
        MethodChoice {
            command_line: self.command_line,
            rule: self.rules.get(rule).copied(),
            config: self.config,
        }
    }
}

named! {
    /// Who a plan is for.
    PlanMode {
        /// A person, who reviews it: Safe and Review items.
        Review = "review",
        /// Automation (`--only`, a profile, with `--yes`): Safe items removed by a
        /// delete, nothing else (pinned 1).
        SafeOnly = "safe-only",
    }
}

/// The ports that freeze, delete and trash. Only the platform implements them.
#[derive(Debug, Clone, Copy)]
pub struct MutationPorts<'ports> {
    /// Reads targets from their handles.
    pub freezer: &'ports dyn Freezer,
    /// Deletes what an operation authorizes.
    pub executor: &'ports dyn Executor,
    /// Moves items whole to the trash and back.
    pub trash: &'ports dyn TrashBackend,
}

/// What to plan.
#[derive(Debug, Clone, Copy)]
pub struct PlanRequest<'scan> {
    /// The scan the user selected from.
    pub scan: &'scan ScanReport,
    /// The findings selected, each by its ID.
    pub selection: &'scan Selection,
    /// Who the plan is for.
    pub mode: PlanMode,
}

named! {
    /// How two selected items conflict.
    ConflictKind {
        /// One is at or above the other, so removing it would remove the other too.
        Overlap = "overlap",
        /// The same directory, selected through two rules or paths, by two methods.
        Method = "method",
        /// The same directory, selected once as a root's contents and once whole.
        Scope = "scope",
    }
}

/// A conflict with another selected item (04 §3.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conflict {
    /// The other item's finding.
    pub with: FindingId,
    /// How they conflict.
    pub kind: ConflictKind,
}

named! {
    /// Why a finding can't be judged now.
    Unjudged {
        /// The scan doesn't have it.
        NotFound = "not-found",
        /// It wasn't measured, so its facts aren't known.
        Unmeasured = "unmeasured",
        /// Its root, its own identity or its directory's isn't known.
        Unanchored = "unanchored",
    }
}

/// A selected finding that became part of another's item: the same directory, through
/// another rule or path, with the same method and scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Merged {
    /// The finding.
    pub finding: FindingId,
    /// The item it is part of.
    pub into: ItemId,
}

/// A selected finding that isn't planned, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The finding.
    pub finding: FindingId,
    /// Why.
    pub skip: Skip,
}

/// A plan with what happened to every selected finding: each is an item, part of one,
/// or refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// The plan, frozen.
    pub plan: Plan,
    /// Findings merged into another's item, by finding.
    pub merged: Vec<Merged>,
    /// Findings refused, by finding.
    pub refused: Vec<Refused>,
}

/// An item on its way into the plan, with the aliases merged into it.
#[derive(Debug)]
struct Candidate<'scan> {
    judged: Judged<'scan>,
    aliases: Vec<FindingId>,
}

/// Plans `request`'s selection under `judge`, freezing each item through `ports`. Stops
/// freezing once `cancel` is set, refusing the rest as not started.
#[must_use]
pub fn plan(
    request: &PlanRequest<'_>,
    judge: &Judge<'_>,
    ports: &MutationPorts<'_>,
    cancel: &CancelToken,
) -> Planned {
    let scene = Scene::new(request.scan, judge);
    let mut refused = Vec::new();
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, finding) in request.scan.findings.iter().enumerate() {
        if !request.selection.0.contains(&finding.id) {
            continue;
        }
        seen.insert(finding.id);
        match scene.judge(index, judge) {
            Ok(judged) => candidates.push(Candidate {
                judged,
                aliases: Vec::new(),
            }),
            Err(skip) => refused.push(Refused {
                finding: finding.id,
                skip,
            }),
        }
    }
    refused.extend(
        request
            .selection
            .0
            .iter()
            .filter(|id| !seen.contains(id))
            .map(|id| Refused {
                finding: *id,
                skip: Skip::Unjudged(Unjudged::NotFound),
            }),
    );
    let candidates = without_overlaps(candidates, &mut refused);
    let candidates = merge(candidates, &mut refused);
    let candidates = suited(candidates, request.mode, &mut refused);
    let planned = freeze_all(candidates, ports, cancel, &mut refused);
    finish(planned, refused)
}

/// The candidates whose standing and method may be planned for `mode`. The others are
/// refused with their aliases.
fn suited<'scan>(
    candidates: Vec<Candidate<'scan>>,
    mode: PlanMode,
    refused: &mut Vec<Refused>,
) -> Vec<Candidate<'scan>> {
    let mut kept = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        match unsuited(&candidate.judged, mode) {
            None => kept.push(candidate),
            Some(skip) => refuse_all(&candidate, &skip, refused),
        }
    }
    kept
}

/// Why an item of `judged`'s standing and method can't be planned for `mode`, if it can't.
fn unsuited(judged: &Judged<'_>, mode: PlanMode) -> Option<Skip> {
    match (mode, &judged.verdict, judged.method()) {
        (PlanMode::Review, _, _) | (PlanMode::SafeOnly, Verdict::Safe, Method::Delete) => None,
        (PlanMode::SafeOnly, Verdict::Review(cautions), _) => Some(Skip::Riskier(cautions.clone())),
        (PlanMode::SafeOnly, Verdict::Protected(holds), _) => Some(Skip::Protected(holds.clone())),
        (PlanMode::SafeOnly, Verdict::Safe, Method::Trash) => Some(Skip::WholeNotReviewed),
    }
}

/// A directory's selected aliases while they merge: one candidate, or a conflict.
#[derive(Debug)]
enum Group<'scan> {
    /// The first alias, with the others merged into it.
    Open(Box<Candidate<'scan>>),
    /// Two aliases differ: every alias is refused for this.
    Conflicted(Conflict),
}

/// One candidate per directory: aliases with the same method and scope merge into the
/// first; aliases that differ conflict, and all of them are refused.
fn merge<'scan>(
    candidates: Vec<Candidate<'scan>>,
    refused: &mut Vec<Refused>,
) -> Vec<Candidate<'scan>> {
    let mut first: HashMap<_, usize> = HashMap::new();
    let mut groups: Vec<Group<'scan>> = Vec::new();
    for candidate in candidates {
        let id = candidate.judged.finding.id;
        let Some(group) = first
            .get(&candidate.judged.own)
            .and_then(|at| groups.get_mut(*at))
        else {
            first.insert(candidate.judged.own, groups.len());
            groups.push(Group::Open(Box::new(candidate)));
            continue;
        };
        let head = match group {
            Group::Conflicted(conflict) => {
                refused.push(Refused {
                    finding: id,
                    skip: Skip::Conflict(*conflict),
                });
                continue;
            }
            Group::Open(head) => head,
        };
        let Some(kind) = differs(&head.judged, &candidate.judged) else {
            head.aliases.push(id);
            continue;
        };
        let with_head = Conflict {
            with: head.judged.finding.id,
            kind,
        };
        refuse_all(head, &Skip::Conflict(Conflict { with: id, kind }), refused);
        refused.push(Refused {
            finding: id,
            skip: Skip::Conflict(with_head),
        });
        *group = Group::Conflicted(with_head);
    }
    groups
        .into_iter()
        .filter_map(|group| match group {
            Group::Open(candidate) => Some(*candidate),
            Group::Conflicted(_) => None,
        })
        .collect()
}

/// How two aliases differ, if they do.
fn differs(a: &Judged<'_>, b: &Judged<'_>) -> Option<ConflictKind> {
    if a.anchor.scope() != b.anchor.scope() {
        Some(ConflictKind::Scope)
    } else if a.method() != b.method() {
        Some(ConflictKind::Method)
    } else {
        None
    }
}

/// The candidates at or above none of the others, before aliases merge, so a selected
/// finding refused later still conflicts with what is above it. Each pair that overlaps
/// is refused.
fn without_overlaps<'scan>(
    candidates: Vec<Candidate<'scan>>,
    refused: &mut Vec<Refused>,
) -> Vec<Candidate<'scan>> {
    let mut at: HashMap<_, Vec<usize>> = HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        at.entry(candidate.judged.own).or_default().push(index);
    }
    let mut overlaps: BTreeMap<usize, FindingId> = BTreeMap::new();
    for (below, candidate) in candidates.iter().enumerate() {
        let ancestors = candidate.judged.facts.lineage.0.iter().skip(1).flatten();
        for above in ancestors.filter_map(|identity| at.get(identity)).flatten() {
            let Some(other) = candidates.get(*above) else {
                continue;
            };
            overlaps.entry(below).or_insert(other.judged.finding.id);
            overlaps
                .entry(*above)
                .or_insert(candidate.judged.finding.id);
        }
    }
    let mut kept = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.into_iter().enumerate() {
        match overlaps.get(&index) {
            None => kept.push(candidate),
            Some(with) => refuse_all(
                &candidate,
                &Skip::Conflict(Conflict {
                    with: *with,
                    kind: ConflictKind::Overlap,
                }),
                refused,
            ),
        }
    }
    kept
}

/// Refuses `candidate` and its aliases for `skip`.
fn refuse_all(candidate: &Candidate<'_>, skip: &Skip, refused: &mut Vec<Refused>) {
    let findings =
        std::iter::once(candidate.judged.finding.id).chain(candidate.aliases.iter().copied());
    refused.extend(findings.map(|finding| Refused {
        finding,
        skip: skip.clone(),
    }));
}

/// An item frozen for the plan, with the aliases merged into it.
#[derive(Debug)]
struct Frozen {
    entry: PlanEntry,
    aliases: Vec<FindingId>,
}

/// Freezes each candidate in turn. Once `cancel` is set the rest are refused as not
/// started.
fn freeze_all(
    candidates: Vec<Candidate<'_>>,
    ports: &MutationPorts<'_>,
    cancel: &CancelToken,
    refused: &mut Vec<Refused>,
) -> Vec<Frozen> {
    let mut frozen = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let entry = if cancel.is_cancelled() {
            Err(Skip::NotStarted(Stop::Cancelled))
        } else {
            freeze(&candidate.judged, ports, cancel)
        };
        match entry {
            Ok(entry) => frozen.push(Frozen {
                entry,
                aliases: candidate.aliases,
            }),
            Err(skip) => refuse_all(&candidate, &skip, refused),
        }
    }
    frozen
}

/// The plan entry for `judged`, frozen now.
fn freeze(
    judged: &Judged<'_>,
    ports: &MutationPorts<'_>,
    cancel: &CancelToken,
) -> Result<PlanEntry, Skip> {
    let anchor = &judged.anchor;
    if judged.method() == Method::Trash {
        ports
            .trash
            .available(anchor)
            .map_err(Skip::TrashUnavailable)?;
    }
    let freeze = ports.freezer.freeze(anchor, cancel).map_err(|error| {
        if error.code == ErrorCode::Cancelled {
            Skip::NotStarted(Stop::Cancelled)
        } else {
            Skip::Freeze(error)
        }
    })?;
    let file_system = match freeze.file_system {
        FileSystemClass::Supported(file_system) => file_system,
        FileSystemClass::Unsupported(why) => return Err(Skip::FileSystem(why)),
    };
    let top = freeze.tree.top().snapshot().map(|top| top.identity);
    let changed = if freeze.parent != anchor.parent {
        Some(ErrorCode::ParentChanged)
    } else if top != Some(judged.own) || freeze.tree.scope() != anchor.scope() {
        Some(ErrorCode::TargetChanged)
    } else {
        None
    };
    if let Some(code) = changed {
        return Err(Skip::Freeze(ApplyError::new(code)));
    }
    Ok(entry(judged, file_system, Arc::new(freeze.tree)))
}

/// The plan entry for `judged`, with its frozen `tree`. Its item is numbered by the plan.
fn entry(judged: &Judged<'_>, file_system: LocalFileSystem, tree: Arc<FrozenTree>) -> PlanEntry {
    let standing =
        Standing::of(&judged.verdict).unwrap_or_else(|| Standing::Review(BTreeSet::new()));
    PlanEntry {
        item: ItemId(0),
        finding: judged.finding.id,
        path: judged.finding.path.clone(),
        anchor: judged.anchor.clone(),
        method: judged.method(),
        standing,
        judgement: judged.judgement.clone(),
        estimate: judged.finding.size,
        file_system,
        tree,
    }
}

/// The plan of the frozen items, with each alias placed and every refusal in finding
/// order.
fn finish(frozen: Vec<Frozen>, mut refused: Vec<Refused>) -> Planned {
    let mut aliases = Vec::with_capacity(frozen.len());
    let mut entries = Vec::with_capacity(frozen.len());
    for one in frozen {
        aliases.push(one.aliases);
        entries.push(one.entry);
    }
    let plan = Plan::new(entries);
    let mut merged: Vec<Merged> = plan
        .entries()
        .iter()
        .zip(aliases)
        .flat_map(|(entry, aliases)| {
            aliases.into_iter().map(|finding| Merged {
                finding,
                into: entry.item(),
            })
        })
        .collect();
    merged.sort_by_key(|merged| merged.finding);
    refused.sort_by_key(|refused| refused.finding);
    Planned {
        plan,
        merged,
        refused,
    }
}
