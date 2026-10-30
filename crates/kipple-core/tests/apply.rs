//! Planning, revalidation, the apply loop and reconciliation through their public API,
//! against a machine in memory (`docs/04-safety-model.md` §3, §7, §10;
//! `docs/11-delivery-plan.md` M3; spec v2 pinned 1, 2, 7, 8, 12; v2.1 C, F, G, H).
#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a helper given a bad fixture fails the test"
)]
#![expect(
    clippy::significant_drop_tightening,
    reason = "a test reads the machine's state while it checks it"
)]

#[path = "support/codec.rs"]
mod codec;
#[path = "support/world.rs"]
mod world;

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Duration;

use codec::TestCodec;
use kipple_core::{
    Activity, ActivityGap, Allocated, Apparent, ApplyRequest, Authorization, CancelToken, Category,
    Completeness, Conflict, ConflictKind, Consequence, Cost, EntryKind, EntrySnapshot, ErrorCode,
    Evidence, Facts, FileIdentity, FileSystemClass, Finding, FindingId, Fingerprint, Found,
    HeldPath, Hold, IntegrationId, IntegrationReport, ItemOutcome, Judge, JudgementField,
    LeftReason, Level, Lineage, Loss, Method, Methods, Modified, MutationPorts, OpenedRoot,
    Operation, Plan, PlanEntry, PlanMode, PlanRequest, Planned, Platform, Policy, Proposal,
    Protection, Protections, Receipt, Recovery, RelPath, Rule, RuleId, RuleSettings, RuleText,
    ScanOutcome, ScanReport, Scope, Selection, Selector, Skip, SpaceEstimate, Stamp, Standing,
    Status, Stop, SymbolicRoot, Tracked, Unjudged, Unsupported, apply, plan, reconcile,
};
use kipple_core::{ApplyPorts, Depth, Reconciled, Recorders, RunRead, StoredRun, read_run};
use world::{Crash, Crashed, Record, Rescanned, Target, World};

/// The home directory the scenarios run in: absolute on the OS the tests run on, as a
/// stored plan's root path must be.
#[cfg(unix)]
const HOME: &str = "/home/u";
#[cfg(windows)]
const HOME: &str = "C:/home/u";
const FS_ROOT: FileIdentity = id(1);
const HOME_PARENT: FileIdentity = id(2);
const HOME_ID: FileIdentity = id(3);
const CARGO: IntegrationId = IntegrationId("cargo");
const OTHER: IntegrationId = IntegrationId("other");

const fn id(file: u128) -> FileIdentity {
    FileIdentity { volume: 7, file }
}

fn rule_id(text: &str) -> RuleId {
    RuleId(text.to_owned())
}

fn rule(id: &str, integration: IntegrationId) -> Rule {
    Rule {
        id: rule_id(id),
        integration,
        text: RuleText {
            title: id.to_owned(),
            description: String::new(),
            why: String::new(),
            loss: String::new(),
            references: Vec::new(),
        },
        category: Category::BuildArtifact,
        platforms: Platform::ALL.to_vec(),
        root: SymbolicRoot::ProjectRoots,
        selector: Selector::Integration("cargo.target"),
        requirements: Vec::new(),
        eligibility: Vec::new(),
        operation: Operation::RemoveFiles(vec![Method::Delete, Method::Trash]),
        recent_days: None,
    }
}

fn snapshot(kind: EntryKind, file: u128, changed: i64, links: u64) -> EntrySnapshot {
    let stamp = |secs| Stamp::new(secs, 0).unwrap();
    EntrySnapshot {
        kind,
        identity: id(file),
        fingerprint: Fingerprint {
            changed: stamp(changed),
            modified: stamp(50),
            born: Some(stamp(10)),
            size: 4096,
        },
        links,
    }
}

/// `rest` below the home directory.
fn home(rest: &str) -> PathBuf {
    Path::new(HOME).join(rest)
}

fn rel(text: &str) -> RelPath {
    RelPath::of(Path::new(text)).unwrap()
}

/// One item of a scenario: a directory below the home root, found by `rule`.
#[derive(Debug, Clone)]
struct Item {
    path: PathBuf,
    rule: RuleId,
    integration: IntegrationId,
    own: FileIdentity,
    /// The identities of the directories from the item's up to the home directory.
    above: Vec<FileIdentity>,
}

impl Item {
    fn lineage(&self) -> Lineage {
        let mut lineage = vec![Some(self.own)];
        lineage.extend(self.above.iter().copied().map(Some));
        lineage.extend([Some(HOME_ID), Some(HOME_PARENT), Some(FS_ROOT)]);
        Lineage(lineage)
    }
}

/// A project's `target/` at `/home/u/p<n>/target`, holding `entries` (`/` ends a
/// directory) with identities from `base`.
fn project(n: u128, base: u128, entries: &[&str]) -> (Item, Target) {
    let item = Item {
        path: home(&format!("p{n}/target")),
        rule: rule_id("builtin.cargo.target"),
        integration: CARGO,
        own: id(base),
        above: vec![id(base - 1)],
    };
    let mut tree = BTreeMap::new();
    tree.insert(RelPath::default(), snapshot(EntryKind::Dir, base, 100, 1));
    for (offset, entry) in (1..).zip(entries) {
        let kind = if entry.ends_with('/') {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let path = rel(entry.trim_end_matches('/'));
        tree.insert(path, snapshot(kind, base + offset, 100, 1));
    }
    (item, Target::new(Scope::Whole, Some(id(base - 1)), tree))
}

const SIZE: SpaceEstimate = SpaceEstimate {
    apparent: Apparent(8192),
    allocated: Some(Allocated(8192)),
    unique_reclaim: Some(Allocated(8192)),
    completeness: Completeness::Complete,
};

/// Facts that make a build directory Safe at Balanced.
fn safe(lineage: Lineage) -> Facts {
    Facts {
        proposal: Proposal::Remove(vec![Method::Delete, Method::Trash]),
        layout_verified: true,
        consequence: Some(Consequence {
            recovery: Recovery::RebuildLocal,
            loss: &[Loss::CacheWarmth],
            cost: Cost::Low,
        }),
        tracked: Tracked::NoRepository,
        activity: Activity::Idle,
        modified: Modified::Ago(Duration::from_hours(24 * 40)),
        recent_days: None,
        lineage,
    }
}

fn finding(index: u64, item: &Item) -> Finding {
    Finding {
        id: FindingId(index),
        integration: item.integration,
        rule: item.rule.clone(),
        category: Category::BuildArtifact,
        path: item.path.clone(),
        evidence: vec![Evidence::Signature("CACHEDIR.TAG".to_owned())],
        size: Some(SIZE),
        facts: Some(safe(item.lineage())),
    }
}

fn home_root() -> OpenedRoot {
    OpenedRoot {
        root: SymbolicRoot::ProjectRoots,
        path: PathBuf::from(HOME),
        identity: Some(HOME_ID),
    }
}

/// The scan that found `items`, each through its integration under the home root.
fn scan_of(items: &[Item]) -> ScanReport {
    let integrations: BTreeSet<IntegrationId> = items.iter().map(|item| item.integration).collect();
    ScanReport {
        outcome: ScanOutcome::Completed,
        integrations: integrations
            .into_iter()
            .map(|integration| IntegrationReport {
                id: integration,
                omissions: Vec::new(),
                advice: Vec::new(),
                opened: vec![home_root()],
            })
            .collect(),
        findings: (0..).zip(items).map(|(n, item)| finding(n, item)).collect(),
        unchecked_system: 0,
        by_command_line: 0,
    }
}

/// The home directory as the host protects it.
fn protections() -> Protections {
    Protections {
        home: Some(HeldPath {
            forms: vec![PathBuf::from(HOME)],
            lineage: Lineage(vec![Some(HOME_ID), Some(HOME_PARENT), Some(FS_ROOT)]),
            note: None,
        }),
        subresources: Vec::new(),
    }
}

/// The config a scenario plans and applies under.
#[derive(Debug, Clone)]
struct Config {
    rules: Vec<Rule>,
    policy: Policy,
    methods: Methods,
    protections: Protections,
}

impl Config {
    fn new() -> Self {
        Self {
            rules: vec![
                rule("builtin.cargo.target", CARGO),
                rule("builtin.other.build", OTHER),
            ],
            policy: Policy::default(),
            methods: Methods::default(),
            protections: protections(),
        }
    }

    fn judge(&self) -> Judge<'_> {
        Judge {
            rules: &self.rules,
            policy: &self.policy,
            methods: &self.methods,
            protections: &self.protections,
        }
    }
}

const fn ports(world: &World) -> MutationPorts<'_> {
    MutationPorts {
        freezer: world,
        executor: world,
        trash: world,
        tracked: world,
    }
}

/// Applies `plan` with `authorization`, rescanning as `rescan`, journalling to `world`.
fn apply_as(
    plan: &Plan,
    authorization: Authorization,
    rescan: &Rescanned,
    config: &Config,
    world: &World,
) -> Receipt {
    let mutation = ports(world);
    let ports = ApplyPorts {
        rescan,
        mutation: &mutation,
    };
    let mut journal = world.recorder();
    let to = Recorders {
        journal: &mut journal,
        events: &mut (),
    };
    apply(
        &ApplyRequest {
            plan,
            authorization,
        },
        &config.judge(),
        &ports,
        to,
        &CancelToken::new(),
    )
}

/// `scan` as apply's rescan, with each finding's own activity.
fn rescanned(scan: &ScanReport) -> Rescanned {
    Rescanned {
        scan: scan.clone(),
        activity: None,
    }
}

/// A machine holding `targets`, each at its item's path.
fn world_of(targets: &[(Item, Target)]) -> World {
    let world = World::default();
    world.state().targets = targets
        .iter()
        .map(|(item, target)| (item.path.clone(), target.clone()))
        .collect();
    world
}

fn everything(scan: &ScanReport) -> Selection {
    Selection(scan.findings.iter().map(|finding| finding.id).collect())
}

fn plan_all(scan: &ScanReport, config: &Config, world: &World, mode: PlanMode) -> Planned {
    let request = PlanRequest {
        scan,
        selection: &everything(scan),
        mode,
    };
    plan(
        &request,
        &config.judge(),
        &ports(world),
        &CancelToken::new(),
    )
}

/// Plans the findings `ids` of `scan`.
fn plan_some(scan: &ScanReport, ids: &[u64], config: &Config, world: &World) -> Planned {
    let request = PlanRequest {
        scan,
        selection: &Selection(ids.iter().copied().map(FindingId).collect()),
        mode: PlanMode::Review,
    };
    plan(
        &request,
        &config.judge(),
        &ports(world),
        &CancelToken::new(),
    )
}

const fn reviewed(plan: &Plan) -> Authorization {
    Authorization::Reviewed {
        plan: plan.digest(),
    }
}

fn run(plan: &Plan, rescan: &ScanReport, config: &Config, world: &World) -> Receipt {
    run_with(plan, rescan, config, world, &CancelToken::new())
}

fn run_with(
    plan: &Plan,
    rescan: &ScanReport,
    config: &Config,
    world: &World,
    cancel: &CancelToken,
) -> Receipt {
    let request = ApplyRequest {
        plan,
        authorization: reviewed(plan),
    };
    let rescan = rescanned(rescan);
    let mutation = ports(world);
    let ports = ApplyPorts {
        rescan: &rescan,
        mutation: &mutation,
    };
    let mut journal = world.recorder();
    let to = Recorders {
        journal: &mut journal,
        events: &mut (),
    };
    apply(&request, &config.judge(), &ports, to, cancel)
}

fn statuses(receipt: &Receipt) -> Vec<Status> {
    receipt
        .items
        .iter()
        .map(|item| item.outcome.status())
        .collect()
}

const fn skip_of(outcome: &ItemOutcome) -> Option<&Skip> {
    match outcome {
        ItemOutcome::Skipped(skip) => Some(skip),
        ItemOutcome::Done(_)
        | ItemOutcome::Partial(_)
        | ItemOutcome::Failed { .. }
        | ItemOutcome::Cancelled(_)
        | ItemOutcome::Indeterminate(_) => None,
    }
}

fn left(outcome: &ItemOutcome, reason: LeftReason) -> u64 {
    outcome
        .effect()
        .and_then(|effect| effect.left.0.get(&reason))
        .map_or(0, |left| left.count)
}

const TARGET: &[&str] = &[
    "a",
    "b",
    "debug/",
    "debug/app",
    "debug/deps/",
    "debug/deps/c",
];

/// One project `target/`, planned for review, with the scan that found it.
fn one() -> (World, ScanReport, Config, Planned) {
    let (item, target) = project(0, 100, TARGET);
    let world = world_of(&[(item.clone(), target)]);
    let scan = scan_of(&[item]);
    let config = Config::new();
    let planned = plan_all(&scan, &config, &world, PlanMode::Review);
    assert_eq!(planned.refused, Vec::new());
    (world, scan, config, planned)
}

/// Catches an item run before its intent is durable, or recorded out of order: a delete
/// runs between its intent and its outcome, removes every reviewed entry, and its totals
/// count each removed name and its estimated bytes once (04 §7, §8; pinned 7, 8).
#[test]
fn an_item_runs_between_its_durable_intent_and_its_outcome() {
    let (world, scan, config, planned) = one();
    let receipt = run(&planned.plan, &scan, &config, &world);
    assert_eq!(statuses(&receipt), [Status::Done]);
    let state = world.state();
    assert_eq!(state.unjournalled, Vec::new());
    assert_eq!(state.order_problem(), None);
    let kinds: Vec<_> = state
        .records
        .iter()
        .map(|record| match record {
            Record::Plan { .. } => "plan",
            Record::Intent(_) => "intent",
            Record::Outcome { .. } => "outcome",
            Record::RestoreIntent { .. }
            | Record::RestoreOutcome { .. }
            | Record::TrashRecordRemoved { .. } => "restore",
        })
        .collect();
    assert_eq!(kinds, ["plan", "intent", "outcome"]);
    let Record::Intent(intent) = &state.records[1] else {
        panic!("the second record is the intent");
    };
    assert_eq!(
        intent.entries, 7,
        "the intent names every entry it may remove"
    );
    assert_eq!(state.removals.len(), 7);
    assert!(state.targets.is_empty(), "the target is gone");
    let totals = receipt.totals();
    assert_eq!(totals.removed.names, 7);
    assert_eq!(totals.removed.estimated, Allocated(7 * 4096));
}

/// Catches apply trusting a root that was unmounted or swapped since the scan (v2.1 G;
/// 04 §10 "a project that is missing or unmounted"): nothing is frozen or removed.
#[test]
fn a_root_that_changed_or_didnt_open_runs_nothing() {
    let (world, scan, config, planned) = one();
    let freezes = world.state().freezes;
    let mut swapped = scan.clone();
    swapped.integrations[0].opened[0].identity = Some(id(77));
    let mut unmounted = scan;
    unmounted.integrations[0].opened.clear();
    for rescan in [swapped, unmounted] {
        let receipt = run(&planned.plan, &rescan, &config, &world);
        assert_eq!(skip_of(&receipt.items[0].outcome), Some(&Skip::RootChanged));
    }
    let state = world.state();
    assert_eq!(
        state.freezes, freezes,
        "nothing is frozen past a changed root"
    );
    assert_eq!(state.removals, Vec::new());
}

/// Catches layout or activity taken from the planning scan instead of judged again
/// (v2.1 H; 04 §3.3, §4; 04 §10 "an owning tool that is busy, or whose state is
/// unknown"): each item is judged from the scan run for the apply.
#[test]
fn layout_and_activity_are_judged_again_at_apply() {
    let cases: [(Edit<Finding>, Skip); 5] = [
        (
            |finding| finding.evidence = vec![Evidence::NoSignature],
            Skip::Protected(vec![Hold::UnknownLayout]),
        ),
        (
            |finding| finding.facts.as_mut().unwrap().activity = in_use(),
            Skip::Protected(vec![Hold::InUse]),
        ),
        (
            |finding| {
                finding.facts.as_mut().unwrap().activity =
                    Activity::Unknown(ActivityGap::Uninspected(1));
            },
            Skip::Protected(vec![Hold::UnknownActivity]),
        ),
        (
            |finding| finding.facts = None,
            Skip::Unjudged(Unjudged::Unmeasured),
        ),
        (
            |finding| finding.path = home("p0/elsewhere"),
            Skip::Unjudged(Unjudged::NotFound),
        ),
    ];
    for (change, skip) in cases {
        let (world, scan, config, planned) = one();
        let mut rescan = scan.clone();
        change(&mut rescan.findings[0]);
        refresh_layout(&mut rescan);
        let receipt = run(&planned.plan, &rescan, &config, &world);
        assert_eq!(skip_of(&receipt.items[0].outcome), Some(&skip));
        assert_eq!(world.state().removals, Vec::new());
    }
}

fn in_use() -> Activity {
    Activity::InUse {
        pid: 42,
        seen: kipple_core::Seen::Executable(home("p0/target/debug/app")),
    }
}

/// The fact the engine derives from evidence: a signature verifies the layout.
fn refresh_layout(scan: &mut ScanReport) {
    for finding in &mut scan.findings {
        let verified = finding
            .evidence
            .iter()
            .any(|evidence| matches!(evidence, Evidence::Signature(_)));
        if let Some(facts) = finding.facts.as_mut() {
            facts.layout_verified = verified;
        }
    }
}

/// Catches a plan applied after the config changed what it was judged under (v2.1 H;
/// 11 M3's profile regressions): a level, a rule's level or recent window, a rule turned
/// back on, a rule with a wider pattern or gone, another method, a new kept path.
#[test]
fn a_changed_config_drops_the_item() {
    for (change, field) in config_changes() {
        let (item, target) = project(0, 100, TARGET);
        let world = world_of(&[(item.clone(), target)]);
        let scan = scan_of(&[item]);
        let mut config = Config::new();
        let disabled = RuleSettings {
            disabled: true,
            ..RuleSettings::default()
        };
        config
            .policy
            .rules
            .insert(rule_id("builtin.other.build"), disabled);
        let planned = plan_all(&scan, &config, &world, PlanMode::Review);
        change(&mut config);
        let receipt = run(&planned.plan, &scan, &config, &world);
        assert_eq!(
            skip_of(&receipt.items[0].outcome),
            Some(&Skip::Rejudged(field))
        );
        assert_eq!(world.state().removals, Vec::new());
    }
}

/// A change to a test's input.
type Edit<T> = fn(&mut T);

/// Each config change, with what it changes of an item's judgement.
fn config_changes() -> [(Edit<Config>, JudgementField); 8] {
    [
        (
            |config| config.policy.level = Level::Careful,
            JudgementField::Level,
        ),
        (
            |config| {
                let thorough = RuleSettings {
                    level: Some(Level::Thorough),
                    ..RuleSettings::default()
                };
                config
                    .policy
                    .rules
                    .insert(rule_id("builtin.cargo.target"), thorough);
            },
            JudgementField::Level,
        ),
        (
            |config| {
                let longer = RuleSettings {
                    recent_days: Some(30),
                    ..RuleSettings::default()
                };
                config
                    .policy
                    .rules
                    .insert(rule_id("builtin.cargo.target"), longer);
            },
            JudgementField::RecentWindow,
        ),
        (
            |config| {
                config.policy.rules.remove(&rule_id("builtin.other.build"));
            },
            JudgementField::Policy,
        ),
        (
            |config| config.rules[0].selector = Selector::Integration("cargo.any"),
            JudgementField::Rule,
        ),
        (
            |config| config.rules.remove(0).id.0.clear(),
            JudgementField::Rule,
        ),
        (
            |config| config.methods.config = Some(Method::Trash),
            JudgementField::Method,
        ),
        (
            |config| {
                config.policy.keep.push(HeldPath {
                    forms: vec![home("p9")],
                    lineage: Lineage(vec![Some(id(9000))]),
                    note: None,
                });
            },
            JudgementField::Held,
        ),
    ]
}

/// Catches a once-Safe item that became Review or unknown still running unattended
/// (11 M3's profile regressions; pinned 1).
#[test]
fn an_item_that_became_riskier_or_unknown_is_skipped() {
    let (world, scan, config, planned) = one();
    let mut recent = scan.clone();
    recent.findings[0].facts.as_mut().unwrap().modified = Modified::Ago(Duration::from_hours(1));
    let mut unknown = scan;
    unknown.findings[0].facts.as_mut().unwrap().modified =
        Modified::Unknown(kipple_core::WalkGap::Unread);
    for authorization in [reviewed(&planned.plan), Authorization::SafeOnly] {
        for (rescan, skip) in [
            (&recent, Skip::Riskier(vec![kipple_core::Caution::Recent])),
            (&unknown, Skip::Protected(vec![Hold::UnknownModified])),
        ] {
            let receipt = apply_as(
                &planned.plan,
                authorization,
                &rescanned(rescan),
                &config,
                &world,
            );
            assert_eq!(skip_of(&receipt.items[0].outcome), Some(&skip));
        }
    }
    assert_eq!(world.state().removals, Vec::new());
}

/// Catches an item replaced between scan and apply being removed as if it were the one
/// reviewed (04 §10): nothing of it is touched.
#[test]
fn an_item_replaced_since_review_is_not_touched() {
    let (world, scan, config, planned) = one();
    let mut rescan = scan;
    let replaced = id(555);
    {
        let mut state = world.state();
        let target = state.targets.values_mut().next().unwrap();
        target
            .entries
            .insert(RelPath::default(), snapshot(EntryKind::Dir, 555, 100, 1));
    }
    rescan.findings[0].facts.as_mut().unwrap().lineage.0[0] = Some(replaced);
    let receipt = run(&planned.plan, &rescan, &config, &world);
    let Some(Skip::AllDropped(left)) = skip_of(&receipt.items[0].outcome) else {
        panic!("the item is dropped whole: {:?}", receipt.items[0].outcome);
    };
    assert_eq!(left.0[&LeftReason::Replaced].count, 7);
    assert_eq!(world.state().removals, Vec::new());
}

/// Catches a child created after review being removed because its directory was
/// selected (04 §3.5, §10; v2.1 B, C): the changed directory is left with everything in
/// it, and the item is partial, measured against what was reviewed.
#[test]
fn a_child_created_after_review_is_never_removed() {
    let (world, scan, config, planned) = one();
    {
        let mut state = world.state();
        let target = state.targets.values_mut().next().unwrap();
        target.entries.insert(
            rel("debug/deps/new"),
            snapshot(EntryKind::File, 999, 200, 1),
        );
        target
            .entries
            .insert(rel("debug/deps"), snapshot(EntryKind::Dir, 105, 200, 1));
    }
    let receipt = run(&planned.plan, &scan, &config, &world);
    let outcome = &receipt.items[0].outcome;
    assert_eq!(outcome.status(), Status::Partial);
    assert_eq!(left(outcome, LeftReason::Changed), 2);
    assert_eq!(left(outcome, LeftReason::NotEmpty), 2);
    let state = world.state();
    let target = state.targets.values().next().unwrap();
    assert!(
        target.entries.contains_key(&rel("debug/deps/new")),
        "the new child is still there"
    );
    assert_eq!(receipt.totals().removed.names, 3);
}

/// Catches a permission failure on one entry failing the item or hiding the rest
/// (04 §10 "partial permission failure").
#[test]
fn a_refused_entry_leaves_the_item_partial_and_the_rest_removed() {
    let (world, scan, config, planned) = one();
    world
        .state()
        .targets
        .values_mut()
        .next()
        .unwrap()
        .denied
        .insert(rel("b"));
    let receipt = run(&planned.plan, &scan, &config, &world);
    let outcome = &receipt.items[0].outcome;
    assert_eq!(outcome.status(), Status::Partial);
    assert_eq!(left(outcome, LeftReason::Denied), 1);
    assert_eq!(left(outcome, LeftReason::NotEmpty), 1);
    assert_eq!(receipt.totals().removed.names, 5);
}

/// Two projects' `target/` directories.
fn two() -> (World, ScanReport, Config, Planned) {
    let first = project(0, 100, TARGET);
    let second = project(1, 200, TARGET);
    let world = world_of(&[first.clone(), second.clone()]);
    let scan = scan_of(&[first.0, second.0]);
    let config = Config::new();
    let planned = plan_all(&scan, &config, &world, PlanMode::Review);
    assert_eq!(planned.plan.entries().len(), 2);
    (world, scan, config, planned)
}

/// Catches a cancel that loses what was already removed or lets the next item start
/// (04 §7, §10 "cancellation after the first success").
#[test]
fn cancelling_after_the_first_removal_stops_this_item_and_the_rest() {
    let (world, scan, config, planned) = two();
    world.state().cancel_after = Some(1);
    let receipt = run(&planned.plan, &scan, &config, &world);
    assert_eq!(statuses(&receipt), [Status::Cancelled, Status::Skipped]);
    assert_eq!(
        skip_of(&receipt.items[1].outcome),
        Some(&Skip::NotStarted(Stop::Cancelled))
    );
    assert_eq!(left(&receipt.items[0].outcome, LeftReason::NotReached), 6);
    assert_eq!(receipt.totals().removed.names, 1);
    let state = world.state();
    assert_eq!(state.ran, [planned.plan.entries()[0].item()]);
    assert_eq!(state.order_problem(), None);
}

/// Catches a journal failure that lets an item run without a durable intent, or lets
/// the apply go on (pinned 8): the plan, an intent and an outcome each failing.
#[test]
fn a_journal_write_that_fails_stops_the_apply() {
    let journal_failed = Skip::NotStarted(Stop::JournalFailed);
    for (fails_at, ran, first) in [
        (0, 0, Status::Skipped),
        (1, 0, Status::Skipped),
        (2, 1, Status::Done),
    ] {
        let (world, scan, config, planned) = two();
        world.state().journal_fails_at = Some(fails_at);
        let receipt = run(&planned.plan, &scan, &config, &world);
        assert_eq!(statuses(&receipt), [first, Status::Skipped]);
        assert_eq!(skip_of(&receipt.items[1].outcome), Some(&journal_failed));
        let state = world.state();
        assert_eq!(state.ran.len(), ran);
        assert_eq!(state.unjournalled, Vec::new());
    }
}

/// Catches a refusal before anything was touched reported as a failure (pinned 7): the
/// executor or the trash refusing the item, as when its parent was replaced after the
/// freeze, skips it, by either method, and removes nothing.
#[test]
fn an_item_refused_before_anything_was_touched_is_skipped() {
    for method in [Method::Delete, Method::Trash] {
        let (item, target) = project(0, 100, TARGET);
        let world = world_of(&[(item.clone(), target)]);
        let scan = scan_of(&[item]);
        let mut config = Config::new();
        config.methods.config = Some(method);
        let planned = plan_all(&scan, &config, &world, PlanMode::Review);
        for disk in world.state().targets.values_mut() {
            disk.refusal = Some(ErrorCode::ParentChanged);
        }

        let receipt = run(&planned.plan, &scan, &config, &world);

        assert!(
            matches!(
                skip_of(&receipt.items[0].outcome),
                Some(Skip::Refused(error)) if error.code == ErrorCode::ParentChanged
            ),
            "{method:?}: {:?}",
            receipt.items[0].outcome
        );
        let state = world.state();
        assert_eq!(state.removals, Vec::new(), "{method:?}");
        assert_eq!(state.trash.len(), 0, "{method:?}");
    }
}

/// Catches a trash move that stranded something swapped in for the item reported as a
/// skip, which says nothing moved: it fails, naming the slot it is at.
#[test]
fn a_trash_move_that_stranded_something_fails() {
    let (item, target) = project(0, 100, TARGET);
    let world = world_of(&[(item.clone(), target)]);
    let scan = scan_of(&[item]);
    let mut config = Config::new();
    config.methods.config = Some(Method::Trash);
    let planned = plan_all(&scan, &config, &world, PlanMode::Review);
    for disk in world.state().targets.values_mut() {
        disk.refusal = Some(ErrorCode::Stranded);
    }

    let receipt = run(&planned.plan, &scan, &config, &world);

    assert!(
        matches!(
            &receipt.items[0].outcome,
            ItemOutcome::Failed { error, .. } if error.code == ErrorCode::Stranded
        ),
        "{:?}",
        receipt.items[0].outcome
    );
}

/// Runs `plan` until `crash`, and returns the intents it left pending.
fn crash_run(
    plan: &Plan,
    scan: &ScanReport,
    config: &Config,
    world: &World,
    crash: Crash,
) -> Vec<kipple_core::Intent> {
    world.state().crash = Some(crash);
    let crashed = catch_unwind(AssertUnwindSafe(|| run(plan, scan, config, world)));
    assert!(
        matches!(&crashed, Err(payload) if payload.is::<Crashed>()),
        "the run crashes, and nothing else unwinds"
    );
    let mut state = world.state();
    state.crash = None;
    state.pending()
}

/// Runs `plan` until `crash`, then reconciles what it left.
fn crash_then_reconcile(
    plan: &Plan,
    scan: &ScanReport,
    config: &Config,
    world: &World,
    crash: Crash,
) -> Vec<Reconciled> {
    crash_run(plan, scan, config, world, crash);
    let ran = world.state().ran.clone();
    let removals = world.state().removals.len();
    let mut journal = world.recorder();
    let run = stored(world, plan);
    let reconciled = reconcile(&run, &ports(world), &mut journal, &CancelToken::new()).unwrap();
    let state = world.state();
    assert_eq!(state.ran, ran, "reconciliation runs nothing");
    assert_eq!(
        state.removals.len(),
        removals,
        "reconciliation removes nothing"
    );
    assert_eq!(state.order_problem(), None);
    assert_eq!(state.pending(), Vec::new());
    reconciled
}

/// The run `world` stored for `plan`, read back with its trees as reconciliation reads it.
fn stored(world: &World, plan: &Plan) -> StoredRun {
    let id = world.store(plan);
    let read = read_run(world, &id, Depth::Trees, &TestCodec);
    let run = if let RunRead::Read(run) = read {
        Some(run)
    } else {
        None
    };
    run.unwrap()
}

/// The outcome reconciliation recorded for an interrupted item.
const fn outcome_of(reconciled: &Reconciled) -> Option<&ItemOutcome> {
    match reconciled {
        Reconciled::Item { outcome, .. } => Some(outcome),
        Reconciled::Restore { .. } => None,
    }
}

/// What reconciliation found of each item, which is always indeterminate.
fn found(reconciled: &[Reconciled]) -> Vec<Option<Found>> {
    reconciled
        .iter()
        .map(|one| match outcome_of(one) {
            Some(ItemOutcome::Indeterminate(found)) => Some(found.clone()),
            Some(
                ItemOutcome::Done(_)
                | ItemOutcome::Partial(_)
                | ItemOutcome::Skipped(_)
                | ItemOutcome::Failed { .. }
                | ItemOutcome::Cancelled(_),
            )
            | None => None,
        })
        .collect()
}

/// Catches reconciliation that counts what a crash left as removed by kipple, or runs
/// anything again (pinned 8; v2.1 D, F; 04 §10 "crash reconciliation"): a crash in the
/// middle of a delete leaves the item partly gone.
#[test]
fn a_crash_during_a_delete_is_reconciled_as_partly_gone() {
    let (world, scan, config, planned) = one();
    let reconciled = crash_then_reconcile(
        &planned.plan,
        &scan,
        &config,
        &world,
        Crash::AfterRemovals(2),
    );
    assert_eq!(
        found(&reconciled),
        [Some(Found::PartlyGone { gone: 2, of: 7 })]
    );
}

/// A freezer that is cancelled while it walks, as when the user quits during start-up.
#[derive(Debug)]
struct CancelledMidFreeze(CancelToken);

impl kipple_core::Freezer for CancelledMidFreeze {
    fn freeze(
        &self,
        _: &kipple_core::Anchor,
        _: &CancelToken,
    ) -> Result<kipple_core::Freeze, kipple_core::ApplyError> {
        self.0.cancel();
        Err(kipple_core::ApplyError::new(ErrorCode::Cancelled))
    }
}

/// Catches a cancel during reconciliation recorded as the item's final outcome: the
/// item must stay pending, so the next start can still find what the crash left.
#[test]
fn a_cancel_during_reconciliation_leaves_the_item_pending() {
    let (world, scan, config, planned) = one();
    let pending = crash_run(
        &planned.plan,
        &scan,
        &config,
        &world,
        Crash::AfterRemovals(2),
    );
    let cancel = CancelToken::new();
    let freezer = CancelledMidFreeze(cancel.clone());
    let ports = MutationPorts {
        freezer: &freezer,
        executor: &world,
        trash: &world,
        tracked: &world,
    };
    let mut journal = world.recorder();
    let run = stored(&world, &planned.plan);

    let reconciled = reconcile(&run, &ports, &mut journal, &cancel).unwrap();

    assert_eq!(reconciled, Vec::new());
    assert_eq!(world.state().pending(), pending);
}

/// Catches an intent read as a removal: a crash right after the intent leaves the item
/// unchanged, and one before its outcome, once it is all gone, gone.
#[test]
fn a_crash_around_a_delete_is_reconciled_as_unchanged_or_gone() {
    for (crash, expected) in [
        (Crash::AfterWrite(1), Found::Unchanged),
        (Crash::BeforeWrite(2), Found::Gone),
    ] {
        let (world, scan, config, planned) = one();
        let reconciled = crash_then_reconcile(&planned.plan, &scan, &config, &world, crash);
        assert_eq!(found(&reconciled), [Some(expected)]);
    }
}

/// Catches a trashed item's restore handle lost to a crash between the move and its
/// outcome (pinned 8, 9): reconciliation finds it at the slot its intent named.
#[test]
fn a_crash_after_a_trash_move_keeps_the_restore_handle() {
    let (item, target) = project(0, 100, TARGET);
    let world = world_of(&[(item.clone(), target)]);
    let scan = scan_of(&[item]);
    let mut config = Config::new();
    config.methods.config = Some(Method::Trash);
    let planned = plan_all(&scan, &config, &world, PlanMode::Review);
    let reconciled =
        crash_then_reconcile(&planned.plan, &scan, &config, &world, Crash::BeforeWrite(2));
    let slot = kipple_core::TrashSlot {
        trash: world::trash_dir(),
        name: kipple_core::Component::new("item-0").unwrap(),
    };
    assert_eq!(
        found(&reconciled),
        [Some(Found::Trashed(kipple_core::Trashed { slot }))]
    );
}

/// The baseline run a crash interrupted between a trash move and its outcome, as an
/// earlier release stored it (`crates/kipple/tests/compat/`). The baselines were written
/// on Unix, so their paths and names are Unix ones.
#[cfg(unix)]
const BASELINE_PLAN: &[u8] =
    include_bytes!("../../kipple/tests/compat/stored-plan/v1/interrupted-trash.plan");
#[cfg(unix)]
const BASELINE_JOURNAL: &[u8] =
    include_bytes!("../../kipple/tests/compat/journal/v1/interrupted-trash.journal");

/// The world the baseline run was made in: one project's `target/`, planned for the
/// trash, crashed after the move and before its outcome.
#[cfg(unix)]
fn baseline_world() -> (World, Plan) {
    let (item, target) = project(0, 100, TARGET);
    let world = world_of(&[(item.clone(), target)]);
    let scan = scan_of(&[item]);
    let mut config = Config::new();
    config.methods.config = Some(Method::Trash);
    let planned = plan_all(&scan, &config, &world, PlanMode::Review);
    crash_run(&planned.plan, &scan, &config, &world, Crash::BeforeWrite(2));
    (world, planned.plan)
}

/// Catches a run an earlier kipple left pending that no longer reconciles after an
/// upgrade, so its trashed item loses its restore handle (pinned 9, 18): the baseline's
/// intent matches its plan by the digests stored, and the item is found in the trash.
#[cfg(unix)]
#[test]
fn a_baseline_run_with_a_pending_trash_move_reconciles_to_trashed() {
    let (world, _) = baseline_world();
    let id = kipple_core::RunId::at(std::time::UNIX_EPOCH, 0);
    let files = world::Files {
        plan: Some(BASELINE_PLAN.to_vec()),
        journal: BASELINE_JOURNAL.to_vec(),
    };
    world.state().runs.insert(id.clone(), files);
    let read = read_run(&world, &id, Depth::Trees, &TestCodec);
    let RunRead::Read(run) = read else {
        panic!("the baseline run reads: {read:?}");
    };
    let mut journal = world.recorder();
    let reconciled = reconcile(&run, &ports(&world), &mut journal, &CancelToken::new()).unwrap();
    let slot = kipple_core::TrashSlot {
        trash: world::trash_dir(),
        name: kipple_core::Component::new("item-0").unwrap(),
    };
    assert_eq!(
        found(&reconciled),
        [Some(Found::Trashed(kipple_core::Trashed { slot }))]
    );
}

/// Catches the `kipple.frozen-tree` v1 encoding changing for the same tree, which would
/// make every stored tree's digest disagree with the tree a run reads back: the baseline
/// holds the bytes this tree encoded to.
#[cfg(unix)]
#[test]
fn a_frozen_tree_encodes_to_the_baseline_bytes() {
    let (_, plan) = baseline_world();
    let end = BASELINE_PLAN
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap();
    let header: serde_json::Value = serde_json::from_slice(&BASELINE_PLAN[..end]).unwrap();
    let tree = &header["items"][0]["tree"];
    let offset = usize::try_from(tree["offset"].as_u64().unwrap()).unwrap();
    let length = usize::try_from(tree["length"].as_u64().unwrap()).unwrap();
    let stored = &BASELINE_PLAN[end + 1..][offset..offset + length];
    assert_eq!(plan.entries()[0].tree().encode(&TestCodec), stored);
}

/// A finding at `path`, with `own` its identity and `lineage` the rest.
fn at(path: &Path, own: FileIdentity, above: &[FileIdentity]) -> Item {
    Item {
        path: path.to_owned(),
        rule: rule_id("builtin.cargo.target"),
        integration: CARGO,
        own,
        above: above.to_vec(),
    }
}

fn refusals(planned: &Planned) -> Vec<(FindingId, Skip)> {
    planned
        .refused
        .iter()
        .map(|refused| (refused.finding, refused.skip.clone()))
        .collect()
}

/// Catches a plan that could remove the home directory, a file system's root or a
/// declared subresource, or that freezes one before refusing it (04 §3.2, §3.9;
/// pinned 12).
#[test]
fn the_host_protects_home_roots_and_subresources_before_anything_is_frozen() {
    let home_dir = Path::new(HOME);
    let mut itself = finding(0, &at(home_dir, HOME_ID, &[]));
    itself.facts.as_mut().unwrap().lineage =
        Lineage(vec![Some(HOME_ID), Some(HOME_PARENT), Some(FS_ROOT)]);
    let mut above = finding(1, &at(home_dir.parent().unwrap(), HOME_PARENT, &[]));
    above.facts.as_mut().unwrap().lineage = Lineage(vec![Some(HOME_PARENT), Some(FS_ROOT)]);
    let mut root = finding(2, &at(home_dir.ancestors().last().unwrap(), FS_ROOT, &[]));
    root.facts.as_mut().unwrap().lineage = Lineage(vec![Some(FS_ROOT)]);
    let (item, target) = project(0, 100, TARGET);
    let inside = finding(3, &item);
    let world = world_of(&[(item, target)]);
    let mut scan = scan_of(&[]);
    scan.findings = vec![itself, above, root, inside];
    scan.integrations = vec![IntegrationReport {
        id: CARGO,
        omissions: Vec::new(),
        advice: Vec::new(),
        opened: vec![home_root()],
    }];
    let mut config = Config::new();
    config.protections.subresources.push(HeldPath {
        forms: vec![home("p0")],
        lineage: Lineage(vec![Some(id(99))]),
        note: None,
    });
    let planned = plan_all(&scan, &config, &world, PlanMode::Review);
    assert_eq!(
        refusals(&planned),
        [
            (FindingId(0), Skip::HostProtected(Protection::Home)),
            (FindingId(1), Skip::HostProtected(Protection::Home)),
            (
                FindingId(2),
                Skip::HostProtected(Protection::FileSystemRoot)
            ),
            (FindingId(3), Skip::HostProtected(Protection::Subresource)),
        ]
    );
    config.protections = Protections::default();
    let unknown_home = plan_all(&scan, &config, &world, PlanMode::Review);
    assert!(
        unknown_home
            .refused
            .iter()
            .all(|refused| matches!(refused.skip, Skip::HostProtected(_))),
        "with home unknown, nothing can be shown not to hold it"
    );
    assert_eq!(world.state().freezes, 0);
}

/// Catches overlapping operations both authorized (04 §3.10; 11 M3's overlap
/// regressions): a directory and one inside it, selected together, conflict until one
/// is left out.
#[test]
fn a_parent_and_a_child_selected_together_conflict_until_one_is_left_out() {
    let (parent, target) = project(0, 100, TARGET);
    let child = at(&home("p0/target/debug"), id(103), &[id(100), id(99)]);
    let mut world_targets = vec![(parent.clone(), target.clone())];
    let mut debug = BTreeMap::new();
    for (path, entry) in &target.entries {
        if let [first, rest @ ..] = path.components()
            && first.as_os_str() == "debug"
        {
            debug.insert(RelPath::new(rest.to_vec()), *entry);
        }
    }
    world_targets.push((
        child.clone(),
        Target::new(Scope::Whole, Some(id(100)), debug),
    ));
    let world = world_of(&world_targets);
    let scan = scan_of(&[parent, child]);
    let config = Config::new();
    let planned = plan_all(&scan, &config, &world, PlanMode::Review);
    let overlap = |with| {
        Skip::Conflict(Conflict {
            with: FindingId(with),
            kind: ConflictKind::Overlap,
        })
    };
    assert_eq!(
        refusals(&planned),
        [(FindingId(0), overlap(1)), (FindingId(1), overlap(0))]
    );
    assert!(planned.plan.entries().is_empty(), "nothing is planned");
    let child_only = plan_some(&scan, &[1], &config, &world);
    assert_eq!(child_only.plan.entries().len(), 1);
}

/// Catches an overlap missed because one side was merged or refused first (04 §3.10):
/// a parent selected through two rules, or a child whose aliases conflict, still
/// conflicts with the other, and nothing is planned.
#[test]
fn an_overlap_holds_whatever_the_aliases_on_either_side_do() {
    let (world, item, alias) = aliases();
    let child = at(&home("p0/target/debug"), id(103), &[id(100), id(99)]);
    let parent_aliases = scan_of(&[item.clone(), alias, child.clone()]);
    let parents = plan_all(&parent_aliases, &Config::new(), &world, PlanMode::Review);
    assert!(parents.plan.entries().is_empty(), "nothing is planned");
    assert_eq!(parents.refused.len(), 3);
    let child_alias = Item {
        path: home("link/target/debug"),
        rule: rule_id("builtin.other.build"),
        integration: OTHER,
        own: id(103),
        above: vec![id(100), id(55)],
    };
    let child_aliases = scan_of(&[item, child, child_alias]);
    let mut config = Config::new();
    config
        .methods
        .rules
        .insert(rule_id("builtin.other.build"), Method::Trash);
    let children = plan_all(&child_aliases, &config, &world, PlanMode::Review);
    assert!(children.plan.entries().is_empty(), "nothing is planned");
    assert_eq!(
        refusals(&children).first(),
        Some(&(
            FindingId(0),
            Skip::Conflict(Conflict {
                with: FindingId(1),
                kind: ConflictKind::Overlap,
            })
        ))
    );
}

/// The project `target/` and the same directory reached through a link elsewhere,
/// found by another integration's rule.
fn aliases() -> (World, Item, Item) {
    let (item, target) = project(0, 100, TARGET);
    let alias = Item {
        path: home("link/target"),
        rule: rule_id("builtin.other.build"),
        integration: OTHER,
        own: id(100),
        above: vec![id(55)],
    };
    let mut through = target.clone();
    through.parent = Some(id(55));
    let world = world_of(&[(item.clone(), target), (alias.clone(), through)]);
    (world, item, alias)
}

/// Catches an alias kept as a second item, so one directory is removed twice
/// (04 §3.10): aliases with one method merge into one item.
#[test]
fn aliases_with_one_method_merge_into_one_item() {
    let (world, item, alias) = aliases();
    let scan = scan_of(&[item, alias]);
    let planned = plan_all(&scan, &Config::new(), &world, PlanMode::Review);
    assert_eq!(planned.plan.entries().len(), 1);
    assert_eq!(planned.merged.len(), 1);
}

/// Catches one of two methods picked for one directory (11 M3's overlap regression "a
/// method conflict"): both aliases are refused.
#[test]
fn aliases_with_two_methods_conflict() {
    let (world, item, alias) = aliases();
    let scan = scan_of(&[item, alias]);
    let mut config = Config::new();
    config
        .methods
        .rules
        .insert(rule_id("builtin.other.build"), Method::Trash);
    let planned = plan_all(&scan, &config, &world, PlanMode::Review);
    assert!(
        planned.plan.entries().is_empty(),
        "a method conflict plans nothing"
    );
    assert_eq!(planned.refused.len(), 2);
    assert!(
        planned.refused.iter().all(|refused| matches!(
            refused.skip,
            Skip::Conflict(Conflict {
                kind: ConflictKind::Method,
                ..
            })
        )),
        "both aliases conflict: {:?}",
        planned.refused
    );
}

/// Catches a merge that discards a protection (04 §3.10; 11 M3's overlap regression
/// "an alias across a protected root"): an alias that is in use, or that the host
/// protects, holds the item even when only the other is selected.
#[test]
fn an_unselected_alias_keeps_its_protection() {
    let (world, item, alias) = aliases();
    let scan = scan_of(&[item, alias.clone()]);
    let mut busy = scan.clone();
    busy.findings[1].facts.as_mut().unwrap().activity = in_use();
    let planned = plan_some(&busy, &[0], &Config::new(), &world);
    assert_eq!(
        refusals(&planned),
        [(
            FindingId(0),
            Skip::HoldsProtected {
                path: alias.path,
                holds: vec![Hold::InUse],
            }
        )]
    );
    let mut config = Config::new();
    config.protections.subresources.push(HeldPath {
        forms: vec![home("link")],
        lineage: Lineage(vec![Some(id(55))]),
        note: None,
    });
    let across = plan_some(&scan, &[0], &config, &world);
    assert_eq!(
        refusals(&across),
        [(FindingId(0), Skip::HostProtected(Protection::Subresource))]
    );
}

/// Catches an item removed while another finding inside it is in use (04 §3.10, §4).
#[test]
fn a_protected_finding_inside_an_item_holds_it() {
    let (item, target) = project(0, 100, TARGET);
    let tool = at(
        &home("p0/target/debug/tool"),
        id(300),
        &[id(103), id(100), id(99)],
    );
    let world = world_of(&[(item.clone(), target)]);
    let mut scan = scan_of(&[item, tool.clone()]);
    scan.findings[1].facts.as_mut().unwrap().activity = in_use();
    let planned = plan_some(&scan, &[0], &Config::new(), &world);
    assert_eq!(
        refusals(&planned),
        [(
            FindingId(0),
            Skip::HoldsProtected {
                path: tool.path,
                holds: vec![Hold::InUse],
            }
        )]
    );
}

/// Catches automation planning what only a person may authorize (pinned 1): a Review
/// item, or a whole-directory trash move.
#[test]
fn automation_plans_only_safe_deletes() {
    let (item, target) = project(0, 100, TARGET);
    let world = world_of(&[(item.clone(), target)]);
    let mut recent = scan_of(std::slice::from_ref(&item));
    recent.findings[0].facts.as_mut().unwrap().modified = Modified::Ago(Duration::from_hours(1));
    let review = plan_all(&recent, &Config::new(), &world, PlanMode::SafeOnly);
    assert_eq!(
        refusals(&review),
        [(
            FindingId(0),
            Skip::Riskier(vec![kipple_core::Caution::Recent])
        )]
    );
    let scan = scan_of(&[item]);
    let mut config = Config::new();
    config.methods.config = Some(Method::Trash);
    let trash = plan_all(&scan, &config, &world, PlanMode::SafeOnly);
    assert_eq!(refusals(&trash), [(FindingId(0), Skip::WholeNotReviewed)]);
    let delete = plan_all(&scan, &Config::new(), &world, PlanMode::SafeOnly);
    assert_eq!(delete.plan.entries().len(), 1);
}

/// Catches automation removing what only a person may authorize, inside an item it may
/// (pinned 1): a finding in Review inside a Safe item, selected or not, makes the item
/// Review.
#[test]
fn a_finding_in_review_inside_an_item_makes_it_review() {
    let (item, target) = project(0, 100, TARGET);
    let tool = at(
        &home("p0/target/debug/tool"),
        id(300),
        &[id(103), id(100), id(99)],
    );
    let world = world_of(&[(item.clone(), target)]);
    let mut scan = scan_of(&[item, tool]);
    scan.findings[1].facts.as_mut().unwrap().modified = Modified::Ago(Duration::from_hours(1));
    let request = PlanRequest {
        scan: &scan,
        selection: &Selection(BTreeSet::from([FindingId(0)])),
        mode: PlanMode::SafeOnly,
    };
    let planned = plan(
        &request,
        &Config::new().judge(),
        &ports(&world),
        &CancelToken::new(),
    );
    assert_eq!(
        refusals(&planned),
        [(
            FindingId(0),
            Skip::Riskier(vec![kipple_core::Caution::Recent])
        )]
    );
    let reviewed = plan_some(&scan, &[0], &Config::new(), &world);
    assert_eq!(
        reviewed.plan.entries().first().map(PlanEntry::standing),
        Some(&Standing::Review(BTreeSet::from([
            kipple_core::Caution::Recent
        ])))
    );
}

/// A change to a test's config, machine and scan.
type Setup = fn(&mut Config, &World, &mut ScanReport);

/// Catches row 2 of 04 §2a unchecked (pinned 12): a method the rule doesn't offer, a
/// third-party rule never granted, a file system kipple doesn't change and a trash that
/// can't take the item (04 §10 "an unsupported trash backend") each refuse the item,
/// and never with another method.
#[test]
fn an_item_whose_method_isnt_available_or_granted_isnt_planned() {
    let cases: [(Setup, Skip); 4] = [
        (
            |config, _, scan| {
                let facts = scan.findings[0].facts.as_mut().unwrap();
                facts.proposal = Proposal::Remove(vec![Method::Delete]);
                config.methods.config = Some(Method::Trash);
            },
            Skip::MethodNotOffered(Method::Trash),
        ),
        (
            |config, _, _| config.rules[0].integration = IntegrationId::GENERIC,
            Skip::NotGranted,
        ),
        (
            |_, world, _| {
                world
                    .state()
                    .targets
                    .values_mut()
                    .next()
                    .unwrap()
                    .file_system = FileSystemClass::Unsupported(Unsupported::Network);
            },
            Skip::FileSystem(Unsupported::Network),
        ),
        (
            |config, world, _| {
                config.methods.config = Some(Method::Trash);
                world.state().trash_unsupported = true;
            },
            Skip::TrashUnavailable(kipple_core::ApplyError::new(ErrorCode::Unsupported)),
        ),
    ];
    for (change, skip) in cases {
        let (item, target) = project(0, 100, TARGET);
        let world = world_of(&[(item.clone(), target)]);
        let mut scan = scan_of(&[item]);
        let mut config = Config::new();
        change(&mut config, &world, &mut scan);
        let planned = plan_all(&scan, &config, &world, PlanMode::Review);
        assert_eq!(refusals(&planned), [(FindingId(0), skip)]);
    }
}

/// Catches a trash item counted as freed space, or one that falls back to deleting
/// (04 §5, §8): it moves whole, removes nothing, and is counted as moved.
#[test]
fn a_trash_item_moves_whole_and_frees_nothing() {
    let (item, target) = project(0, 100, TARGET);
    let world = world_of(&[(item.clone(), target)]);
    let scan = scan_of(&[item]);
    let mut config = Config::new();
    config.methods.config = Some(Method::Trash);
    let planned = plan_all(&scan, &config, &world, PlanMode::Review);
    let receipt = run(&planned.plan, &scan, &config, &world);
    assert_eq!(statuses(&receipt), [Status::Done]);
    let totals = receipt.totals();
    assert_eq!(totals.removed.names, 0);
    assert_eq!(totals.trashed.items, 1);
    let state = world.state();
    assert_eq!(state.removals, Vec::new());
    assert_eq!(state.trash.len(), 1);
}

/// Catches a hard-linked file whose other name is outside the item counted as freed
/// space (04 §3.10, §8; 11 M3's overlap regression "a hard link with an unselected
/// sibling").
#[test]
fn a_hard_link_with_a_sibling_outside_frees_nothing() {
    let (item, mut target) = project(0, 100, TARGET);
    target
        .entries
        .insert(rel("debug/app"), snapshot(EntryKind::File, 104, 100, 2));
    let world = world_of(&[(item.clone(), target)]);
    let scan = scan_of(&[item]);
    let config = Config::new();
    let planned = plan_all(&scan, &config, &world, PlanMode::Review);
    let receipt = run(&planned.plan, &scan, &config, &world);
    let removed = receipt.totals().removed;
    assert_eq!(removed.names, 7);
    assert_eq!(removed.shared, 1);
    assert_eq!(removed.estimated, Allocated(6 * 4096));
}

/// A small deterministic generator, so a failing case can be replayed from its seed.
#[derive(Debug, Clone, Copy)]
struct SplitMix(u64);

impl SplitMix {
    const fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    const fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn pick(&mut self, bound: usize) -> usize {
        usize::try_from(self.below(bound as u64)).unwrap()
    }

    const fn one_in(&mut self, odds: u64) -> bool {
        self.below(odds) == 0
    }
}

/// A random `target/` of up to 12 entries below the top.
fn random_entries(rng: &mut SplitMix) -> Vec<String> {
    let mut dirs = vec![String::new()];
    let mut entries: Vec<String> = Vec::new();
    for _ in 0..rng.below(13) {
        let parent = dirs[rng.pick(dirs.len())].clone();
        let own = format!("{parent}n{}", rng.below(5));
        if entries
            .iter()
            .any(|entry| entry.trim_end_matches('/') == own)
        {
            continue;
        }
        if rng.one_in(3) {
            entries.push(format!("{own}/"));
            dirs.push(format!("{own}/"));
        } else {
            entries.push(own);
        }
    }
    entries
}

/// What a generated case did to one item between plan and apply.
#[derive(Debug, Default, Clone)]
struct Taint {
    /// Every entry of the item is unknown or changed now: none may be removed.
    whole: bool,
    /// Entries changed, gone or holding something new: none of them, nor anything
    /// inside them, may be removed.
    entries: Vec<RelPath>,
}

impl Taint {
    fn covers(&self, path: &RelPath) -> bool {
        self.whole
            || self
                .entries
                .iter()
                .any(|tainted| path.components().starts_with(tainted.components()))
    }
}

/// Changes item `index` between plan and apply, as the generator picks, and says what
/// that taints.
fn change_item(
    rng: &mut SplitMix,
    world: &World,
    rescan: &mut ScanReport,
    item: &Item,
    index: usize,
) -> Taint {
    let mut taint = Taint::default();
    let mut state = world.state();
    let target = state.targets.get_mut(&item.path).unwrap();
    let paths: Vec<RelPath> = target.entries.keys().skip(1).cloned().collect();
    let facts = rescan.findings[index].facts.as_mut().unwrap();
    match rng.below(13) {
        0 | 1 if !paths.is_empty() => {
            let path = paths[rng.pick(paths.len())].clone();
            let entry = target.entries.get_mut(&path).unwrap();
            entry.fingerprint.changed = Stamp::new(500, 0).unwrap();
            taint.entries.push(path);
        }
        2 if !paths.is_empty() => {
            let path = paths[rng.pick(paths.len())].clone();
            target
                .entries
                .retain(|other, _| !other.components().starts_with(path.components()));
            taint.entries.push(path);
        }
        3 => {
            let dirs: Vec<RelPath> = target
                .entries
                .iter()
                .filter(|(_, entry)| entry.kind == EntryKind::Dir)
                .map(|(path, _)| path.clone())
                .collect();
            let dir = dirs[rng.pick(dirs.len())].clone();
            let mut child = dir.components().to_vec();
            child.push(kipple_core::Component::new("appeared").unwrap());
            target
                .entries
                .insert(RelPath::new(child), snapshot(EntryKind::File, 7777, 100, 1));
            let holder = target.entries.get_mut(&dir).unwrap();
            holder.fingerprint.changed = Stamp::new(600, 0).unwrap();
            taint.entries.push(dir);
        }
        4 => {
            target.unreadable = true;
            taint.whole = true;
        }
        5 => {
            facts.activity = Activity::Unknown(ActivityGap::Uninspected(1));
            taint.whole = true;
        }
        6 => {
            rescan.findings[index].facts = None;
            taint.whole = true;
        }
        7 => {
            let top = target.entries.get_mut(&RelPath::default()).unwrap();
            top.identity = id(8000 + index as u128);
            facts.lineage.0[0] = Some(top.identity);
            taint.whole = true;
        }
        8 if !paths.is_empty() => {
            let path = paths[rng.pick(paths.len())].clone();
            target.denied.insert(path);
        }
        9 => {
            facts.modified = Modified::Ago(Duration::from_hours(1));
            taint.whole = true;
        }
        10 if !paths.is_empty() => {
            let path = paths[rng.pick(paths.len())].clone();
            target.fails_at = Some(path);
        }
        11 => target.refusal = Some(ErrorCode::ParentChanged),
        _ => {}
    }
    taint
}

/// One generated case: the plan, what changed, the faults, and what ran.
#[derive(Debug)]
struct Case {
    plan: Plan,
    taints: Vec<Taint>,
    crashed: bool,
    receipt: Option<Receipt>,
}

fn generated(seed: u64) -> (World, Case) {
    let mut rng = SplitMix(seed);
    let count = 1 + rng.pick(3);
    let projects: Vec<(Item, Target)> = (0..count)
        .map(|n| {
            let entries = random_entries(&mut rng);
            let entries: Vec<&str> = entries.iter().map(String::as_str).collect();
            project(n as u128, 1000 * (n as u128 + 1), &entries)
        })
        .collect();
    let world = world_of(&projects);
    let items: Vec<Item> = projects.into_iter().map(|(item, _)| item).collect();
    let scan = scan_of(&items);
    let mut config = Config::new();
    if rng.one_in(4) {
        config.methods.config = Some(Method::Trash);
    }
    let planned = plan_all(&scan, &config, &world, PlanMode::Review);
    assert_eq!(planned.refused, Vec::new(), "seed {seed} plans every item");
    let mut rescan = scan;
    let mut taints: Vec<Taint> = (0..count)
        .map(|index| change_item(&mut rng, &world, &mut rescan, &items[index], index))
        .collect();
    if rng.one_in(10) {
        rescan.integrations[0].opened[0].identity = Some(id(4242));
        for taint in &mut taints {
            taint.whole = true;
        }
    }
    if rng.one_in(8) && config.methods.config == Some(Method::Trash) {
        world.state().trash_unsupported = true;
        for taint in &mut taints {
            taint.whole = true;
        }
    }
    if rng.one_in(10) {
        config.policy.level = Level::Thorough;
        for taint in &mut taints {
            taint.whole = true;
        }
    }
    {
        let mut state = world.state();
        if rng.one_in(4) {
            state.journal_fails_at = Some(rng.pick(2 * count + 2));
            state.journal_recovers = rng.one_in(2);
        }
        match rng.below(8) {
            0 => state.crash = Some(Crash::AfterWrite(rng.pick(2 * count + 1))),
            1 => state.crash = Some(Crash::BeforeWrite(rng.pick(2 * count + 1))),
            2 => state.crash = Some(Crash::AfterRemovals(1 + rng.pick(6))),
            _ => {}
        }
        if rng.one_in(5) {
            state.cancel_after = Some(rng.pick(8));
        }
    }
    let ran = catch_unwind(AssertUnwindSafe(|| {
        run(&planned.plan, &rescan, &config, &world)
    }));
    assert!(
        ran.as_ref()
            .err()
            .is_none_or(|payload| payload.is::<Crashed>()),
        "seed {seed}: only a crash unwinds"
    );
    let case = Case {
        plan: planned.plan,
        taints,
        crashed: ran.is_err(),
        receipt: ran.ok(),
    };
    (world, case)
}

/// Catches the apply loop breaking its invariants under any mix of fact changes between
/// plan and apply, port failures, journal failures, cancellation and crashes after any
/// journal write or removal (spec v2 M3a-apply; 04 §3.6, §7): nothing runs without a
/// durable intent or twice, nothing outside what the plan authorized and the fresh facts
/// confirm is removed, an unknown fact drops its item whole, every item ends in exactly
/// one outcome, totals count only confirmed removals, and reconciling a crash runs
/// nothing and leaves nothing pending.
#[test]
fn applying_only_subtracts_runs_once_after_its_intent_and_ends_every_item() {
    for seed in 0..1000 {
        let (world, case) = generated(seed);
        {
            let state = world.state();
            assert_eq!(state.unjournalled, Vec::new(), "seed {seed}: intent first");
            let ran: BTreeSet<_> = state.ran.iter().collect();
            assert_eq!(
                ran.len(),
                state.ran.len(),
                "seed {seed}: nothing runs twice"
            );
            for removal in &state.removals {
                let entry = case
                    .plan
                    .entries()
                    .iter()
                    .find(|entry| entry.item() == removal.item)
                    .unwrap();
                let node = entry.tree().find(&removal.path);
                assert!(
                    node.is_some_and(|node| entry.tree().mark(node) == kipple_core::Mark::Remove),
                    "seed {seed}: {} was reviewed for removal",
                    removal.path
                );
                assert!(
                    !case.taints[removal.item.0 as usize].covers(&removal.path),
                    "seed {seed}: {} changed or was unknown at apply",
                    removal.path
                );
            }
            assert_eq!(state.order_problem(), None, "seed {seed}");
            if let Some(failed) = state.failed_write.filter(|_| !case.crashed) {
                assert_eq!(
                    state.writes,
                    failed + 1,
                    "seed {seed}: a failed journal write is the apply's last"
                );
            }
        }
        if let Some(receipt) = &case.receipt {
            check_receipt(seed, &world, &case, receipt);
        }
        if case.crashed {
            check_reconciled(seed, &world, &case);
        }
    }
}

fn check_receipt(seed: u64, world: &World, case: &Case, receipt: &Receipt) {
    let items: Vec<_> = receipt.items.iter().map(|item| item.item).collect();
    let planned: Vec<_> = case.plan.entries().iter().map(PlanEntry::item).collect();
    assert_eq!(
        items, planned,
        "seed {seed}: one outcome per item, in order"
    );
    let state = world.state();
    let removed = receipt.totals().removed;
    assert_eq!(
        removed.names,
        state.removals.len() as u64,
        "seed {seed}: totals count confirmed removals"
    );
    assert_eq!(
        removed.estimated,
        Allocated(removed.names * 4096),
        "seed {seed}"
    );
    for (item, taint) in receipt.items.iter().zip(&case.taints) {
        if matches!(item.outcome, ItemOutcome::Skipped(_)) {
            assert!(
                !state
                    .removals
                    .iter()
                    .any(|removal| removal.item == item.item),
                "seed {seed}: skipped item {} had nothing removed",
                item.item.0
            );
        }
        if taint.whole {
            assert!(
                matches!(item.outcome, ItemOutcome::Skipped(_)),
                "seed {seed}: an unknown fact drops item {}: {:?}",
                item.item.0,
                item.outcome
            );
        }
    }
}

fn check_reconciled(seed: u64, world: &World, case: &Case) {
    let (pending, ran, removals) = {
        let mut state = world.state();
        state.crash = None;
        state.journal_fails_at = None;
        state.failed_write = None;
        (state.pending(), state.ran.clone(), state.removals.len())
    };
    let mut journal = world.recorder();
    let run = stored(world, &case.plan);
    let reconciled = reconcile(&run, &ports(world), &mut journal, &CancelToken::new());
    assert!(
        reconciled.is_ok(),
        "seed {seed}: reconciling records: {reconciled:?}"
    );
    let reconciled = reconciled.unwrap();
    assert_eq!(reconciled.len(), pending.len(), "seed {seed}");
    assert!(
        reconciled
            .iter()
            .all(|one| outcome_of(one).map(ItemOutcome::status) == Some(Status::Indeterminate)),
        "seed {seed}: what a crash left is indeterminate"
    );
    let state = world.state();
    assert_eq!(state.ran, ran, "seed {seed}: reconciliation runs nothing");
    assert_eq!(state.removals.len(), removals, "seed {seed}");
    assert_eq!(
        state.pending(),
        Vec::new(),
        "seed {seed}: nothing stays pending"
    );
    assert_eq!(state.order_problem(), None, "seed {seed}");
}

#[path = "apply/replan.rs"]
mod replan;
#[path = "apply/runs.rs"]
mod runs;
#[path = "apply/session.rs"]
mod session;
