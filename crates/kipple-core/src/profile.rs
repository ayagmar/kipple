//! Profiles: a saved, frozen automation scope for `kipple clean --profile NAME --yes`
//! (`docs/03-functional-spec.md` §2 J6, D-021, D-068).
//!
//! A profile freezes, for every built-in rule that proposes removal on this platform,
//! what decides that rule's findings now: digests of the rule and of its integration, the
//! rule's effective level, recent window, enabled state and method, and the roots it
//! resolves to with their identities; and, for the whole profile, the kept and excluded
//! paths and [`POLICY_VERSION`]. Every run checks each entry again
//! ([`check`]): an entry is active only while every frozen value equals the current one,
//! and only active entries' Safe findings are selected and authorized
//! ([`ProfileGrant`]). A rule new since the profile was made is pending and never active.
//!
//! The types keep the authority where it was confirmed:
//!
//! - [`freeze`] makes a [`DraftProfile`], which can only be shown and encoded;
//! - only a profile read back from a [`ProfileStore`] is a [`Profile`]: [`create`] and
//!   [`replace`] store a draft and read back what they stored, [`read`] reads one;
//! - only [`check`] makes a [`ProfileGrant`], from a [`Profile`] and the current config
//!   and scan.
//!
//! ```compile_fail,E0451
//! // A profile can't be made from values of a frontend's choosing.
//! fn forge(draft: kipple_core::profile::DraftProfile) -> kipple_core::profile::Profile {
//!     kipple_core::profile::Profile { contents: draft.contents().clone() }
//! }
//! ```
//!
//! ```compile_fail,E0277
//! // A draft can't be checked: it was never stored or confirmed.
//! fn forge(draft: kipple_core::profile::DraftProfile) -> kipple_core::profile::Profile {
//!     kipple_core::profile::Profile::from(draft)
//! }
//! ```
//!
//! ```compile_fail,E0277
//! // Nor by a conversion that may refuse it.
//! fn forge(draft: kipple_core::profile::DraftProfile) -> kipple_core::profile::Profile {
//!     kipple_core::profile::Profile::try_from(draft).unwrap()
//! }
//! ```
//!
//! ```compile_fail,E0451
//! // A draft can't be made but by freezing what is configured and scanned now.
//! fn forge(profile: &kipple_core::profile::Profile) -> kipple_core::profile::DraftProfile {
//!     kipple_core::profile::DraftProfile { contents: profile.contents().clone() }
//! }
//! ```
//!
//! ```compile_fail,E0451
//! // A grant can't be made but by checking a stored profile.
//! fn forge(other: &kipple_core::profile::ProfileGrant) -> kipple_core::profile::ProfileGrant {
//!     kipple_core::profile::ProfileGrant { policy: kipple_core::Digest(0), rules: Default::default() }
//! }
//! ```

mod store;

pub use store::{
    ProfileError, ProfileStore, ProfileStoreError, create, list, read, remove, replace,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::digest::Digest;
use crate::engine::{ScanOutcome, ScanReport};
use crate::format::{IntegrationDigests, rule_digest};
use crate::integration::IntegrationId;
use crate::plan::{MethodChoice, PlanEntry, Selection, VerifiedRoot};
use crate::planner::Judge;
use crate::policy::{Level, POLICY_VERSION, Policy, Verdict};
use crate::probe::FileIdentity;
use crate::root::{Roots, SymbolicRoot};
use crate::rule::{Method, Operation, Rule, RuleId};

/// A profile's name: 1 to 64 lowercase ASCII letters, digits and `-`, not starting with
/// `-`, and not a name Windows keeps for a device. It names the file, so it is checked
/// before anything is opened.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProfileName(String);

impl ProfileName {
    /// The most characters a name may have.
    pub const MAX: usize = 64;

    /// `text` as a name, if it is one.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        const DEVICES: &[&str] = &["con", "prn", "aux", "nul"];
        let shape = !text.is_empty()
            && text.len() <= Self::MAX
            && !text.starts_with('-')
            && text
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        let numbered = |prefix: &str| {
            text.strip_prefix(prefix)
                .is_some_and(|rest| rest.len() == 1 && rest.bytes().all(|b| b.is_ascii_digit()))
        };
        let device = DEVICES.contains(&text) || numbered("com") || numbered("lpt");
        (shape && !device).then(|| Self(text.to_owned()))
    }

    /// The name as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0)
    }
}

named! {
    /// Which config list a held path is in.
    HeldKind {
        /// `keep.paths`.
        Keep = "keep",
        /// `exclude.paths`.
        Exclude = "exclude",
    }
}

named! {
    /// Whether a profile authorizes a rule.
    EntryScope {
        /// It does, while the entry is active.
        Included = "included",
        /// It doesn't: `--only` named other rules. Never active, and never pending.
        LeftOut = "left-out",
    }
}

named! {
    /// Whether the config turned a rule on or off.
    RuleState {
        /// On.
        Enabled = "enabled",
        /// Off (`exclude.rules`, or `rules.<id>.enabled = false`).
        Disabled = "disabled",
    }
}

named! {
    /// A frozen value that differs from the current one, which suspends an entry.
    ProfileChange {
        /// The rule's selector, root, requirements, eligibility, operation, platforms or
        /// recent window.
        Rule = "rule",
        /// What its integration declares, its revision or what shows its findings in use.
        Integration = "integration",
        /// Its effective level.
        Level = "level",
        /// Its configured recent window.
        RecentWindow = "recent-window",
        /// Whether it is turned on.
        Enabled = "enabled",
        /// Its method.
        Method = "method",
        /// The roots it resolves to, or one of their identities.
        Roots = "roots",
        /// A kept or excluded path the profile was made with is gone.
        Held = "held",
        /// The classification changed with a kipple upgrade.
        PolicyVersion = "policy-version",
        /// A rule `--only` left out is to be included (`profile review --add`).
        Scope = "scope",
    }
}

/// A kept or excluded path a profile was made with: the path as configured, `~`
/// expanded, without its note (04 §8a).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HeldEntry {
    pub(crate) kind: HeldKind,
    pub(crate) path: PathBuf,
}

impl HeldEntry {
    /// Which list.
    #[must_use]
    pub const fn kind(&self) -> HeldKind {
        self.kind
    }

    /// The path.
    #[must_use]
    pub const fn path(&self) -> &PathBuf {
        &self.path
    }
}

/// A root a rule resolved to when its entry was frozen, with its identity when the scan
/// opened it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrozenRoot {
    pub(crate) root: SymbolicRoot,
    pub(crate) path: PathBuf,
    pub(crate) identity: Option<FileIdentity>,
}

impl FrozenRoot {
    /// Which root.
    #[must_use]
    pub const fn root(&self) -> SymbolicRoot {
        self.root
    }

    /// Its directory, as resolved.
    #[must_use]
    pub const fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Its identity, when the scan opened it. A root without one authorizes nothing.
    #[must_use]
    pub const fn identity(&self) -> Option<FileIdentity> {
        self.identity
    }

    /// The root as a plan anchors at it, when its identity is known.
    fn verified(&self) -> Option<VerifiedRoot> {
        Some(VerifiedRoot {
            root: self.root,
            path: self.path.clone(),
            identity: self.identity?,
        })
    }
}

/// What a profile froze about one rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileEntry {
    pub(crate) rule: RuleId,
    pub(crate) scope: EntryScope,
    pub(crate) rule_digest: Digest,
    pub(crate) integration_digest: Digest,
    pub(crate) enabled: RuleState,
    pub(crate) level: Level,
    pub(crate) recent_days: u16,
    pub(crate) method: Method,
    pub(crate) roots: Vec<FrozenRoot>,
}

impl ProfileEntry {
    /// The rule.
    #[must_use]
    pub const fn rule(&self) -> &RuleId {
        &self.rule
    }

    /// Whether the profile authorizes it.
    #[must_use]
    pub const fn scope(&self) -> EntryScope {
        self.scope
    }

    /// [`rule_digest`] of the rule.
    #[must_use]
    pub const fn rule_digest(&self) -> Digest {
        self.rule_digest
    }

    /// [`integration_digest`](crate::format::integration_digest) of its integration.
    #[must_use]
    pub const fn integration_digest(&self) -> Digest {
        self.integration_digest
    }

    /// Whether it was turned on.
    #[must_use]
    pub const fn enabled(&self) -> RuleState {
        self.enabled
    }

    /// Its effective level.
    #[must_use]
    pub const fn level(&self) -> Level {
        self.level
    }

    /// Its configured recent window, in days ([`Policy::configured_window`]).
    #[must_use]
    pub const fn recent_days(&self) -> u16 {
        self.recent_days
    }

    /// Its method.
    #[must_use]
    pub const fn method(&self) -> Method {
        self.method
    }

    /// The roots it resolved to, sorted.
    #[must_use]
    pub fn roots(&self) -> &[FrozenRoot] {
        &self.roots
    }
}

/// What a profile or a draft of one holds. Read only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileContents {
    pub(crate) name: ProfileName,
    pub(crate) created: u64,
    pub(crate) policy_version: u32,
    pub(crate) held: Vec<HeldEntry>,
    pub(crate) entries: Vec<ProfileEntry>,
}

impl ProfileContents {
    /// Its name.
    #[must_use]
    pub const fn name(&self) -> &ProfileName {
        &self.name
    }

    /// When it was made, in seconds since 1970 (UTC).
    #[must_use]
    pub const fn created(&self) -> u64 {
        self.created
    }

    /// The [`POLICY_VERSION`] it was made under.
    #[must_use]
    pub const fn policy_version(&self) -> u32 {
        self.policy_version
    }

    /// The kept and excluded paths it was made with, sorted.
    #[must_use]
    pub fn held(&self) -> &[HeldEntry] {
        &self.held
    }

    /// One entry per rule, sorted by rule.
    #[must_use]
    pub fn entries(&self) -> &[ProfileEntry] {
        &self.entries
    }

    /// Its entry for `rule`.
    fn entry(&self, rule: &RuleId) -> Option<&ProfileEntry> {
        self.entries.iter().find(|entry| entry.rule == *rule)
    }
}

/// A profile as stored: read back from a [`ProfileStore`]. Only [`check`] takes one, so
/// authority comes only from a file a person confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub(crate) contents: ProfileContents,
}

impl Profile {
    /// What it holds.
    #[must_use]
    pub const fn contents(&self) -> &ProfileContents {
        &self.contents
    }
}

/// A profile frozen and not stored yet: shown to a person, then stored as it was shown
/// ([`create`], [`replace`]). It can't be checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftProfile {
    pub(crate) contents: ProfileContents,
}

impl DraftProfile {
    /// What it holds.
    #[must_use]
    pub const fn contents(&self) -> &ProfileContents {
        &self.contents
    }
}

/// What a profile is frozen and checked against now: the current config, the scan and
/// the roots it resolved, and each integration's digest.
#[derive(Debug, Clone, Copy)]
pub struct ProfileScene<'now> {
    /// The current config.
    pub judge: Judge<'now>,
    /// A scan of every integration, finished.
    pub scan: &'now ScanReport,
    /// The roots it scanned.
    pub roots: &'now Roots,
    /// Each integration's digest ([`Engine::integration_digests`](crate::Engine::integration_digests)).
    pub integrations: &'now IntegrationDigests,
}

/// Why a profile can't be frozen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreezeError {
    /// The scan didn't finish, so roots it didn't reach would be frozen as unknown.
    ScanIncomplete,
    /// `--only` or `--add` names a rule that isn't a built-in rule proposing removal on
    /// this platform.
    NotRemovable(RuleId),
    /// A rule's integration isn't registered.
    UnknownIntegration(RuleId),
    /// It would authorize no rule.
    Empty,
}

/// Freezes a profile called `name`, made at `created`.
///
/// It has an entry for every built-in rule proposing removal on this platform, each as
/// `scene` configures and resolves it now. With `only`, the other rules are frozen too,
/// left out (so they are never pending).
///
/// The method frozen is each rule's `rules.<id>.method`, else `clean.method`, else
/// delete: never `--method`, which a profile run can't take.
///
/// # Errors
/// When the scan didn't finish, `only` names a rule this freezes no entry for, a rule's
/// integration isn't known, or no rule would be included.
pub fn freeze(
    name: ProfileName,
    scene: &ProfileScene<'_>,
    created: SystemTime,
    only: Option<&BTreeSet<RuleId>>,
) -> Result<DraftProfile, FreezeError> {
    if scene.scan.outcome != ScanOutcome::Completed {
        return Err(FreezeError::ScanIncomplete);
    }
    let removal = removal_rules(scene.judge.rules);
    if let Some(named) = only
        .into_iter()
        .flatten()
        .find(|named| !removal.iter().any(|rule| rule.id == **named))
    {
        return Err(FreezeError::NotRemovable(named.clone()));
    }
    let mut entries = Vec::with_capacity(removal.len());
    for rule in removal {
        let scope = match only {
            Some(only) if !only.contains(&rule.id) => EntryScope::LeftOut,
            Some(_) | None => EntryScope::Included,
        };
        entries.push(entry_now(rule, scope, scene)?);
    }
    if !entries
        .iter()
        .any(|entry| entry.scope == EntryScope::Included)
    {
        return Err(FreezeError::Empty);
    }
    entries.sort_by(|a, b| a.rule.cmp(&b.rule));
    let created = created
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    Ok(DraftProfile {
        contents: ProfileContents {
            name,
            created,
            policy_version: POLICY_VERSION,
            held: held_now(scene.judge.policy),
            entries,
        },
    })
}

/// The built-in rules proposing removal, in `rules`' order: what a profile has an entry
/// for. Report-only rules never count.
fn removal_rules(rules: &[Rule]) -> Vec<&Rule> {
    rules
        .iter()
        .filter(|rule| rule.integration != IntegrationId::GENERIC)
        .filter(|rule| matches!(rule.operation, Operation::RemoveFiles(_)))
        .collect()
}

/// `rule`'s entry as `scene` configures and resolves it now.
fn entry_now(
    rule: &Rule,
    scope: EntryScope,
    scene: &ProfileScene<'_>,
) -> Result<ProfileEntry, FreezeError> {
    let integration_digest = scene
        .integrations
        .get(rule.integration)
        .ok_or_else(|| FreezeError::UnknownIntegration(rule.id.clone()))?;
    let policy = scene.judge.policy;
    let choice = MethodChoice {
        command_line: None,
        ..scene.judge.methods.choice(&rule.id)
    };
    Ok(ProfileEntry {
        rule: rule.id.clone(),
        scope,
        rule_digest: rule_digest(rule),
        integration_digest,
        enabled: state(policy, &rule.id),
        level: policy.level_of(&rule.id),
        recent_days: policy.configured_window(&rule.id),
        method: choice.resolve(),
        roots: roots_now(rule, scene.scan, scene.roots),
    })
}

fn state(policy: &Policy, rule: &RuleId) -> RuleState {
    if policy.disabled(rule) {
        RuleState::Disabled
    } else {
        RuleState::Enabled
    }
}

/// Every path `rule`'s root resolves to, with the identity its integration opened it
/// with in `scan`, or none when it didn't open.
fn roots_now(rule: &Rule, scan: &ScanReport, roots: &Roots) -> Vec<FrozenRoot> {
    let opened: Vec<_> = scan
        .integrations
        .iter()
        .filter(|report| report.id == rule.integration)
        .flat_map(|report| &report.opened)
        .filter(|opened| opened.root == rule.root)
        .collect();
    let mut frozen: Vec<FrozenRoot> = roots
        .all(rule.root)
        .iter()
        .filter_map(|resolution| resolution.as_ref().ok())
        .map(|resolved| FrozenRoot {
            root: rule.root,
            path: resolved.path.clone(),
            identity: opened
                .iter()
                .find(|opened| opened.path == resolved.path)
                .and_then(|opened| opened.identity),
        })
        .collect();
    frozen.sort();
    frozen.dedup();
    frozen
}

/// The kept and excluded paths `policy` holds, as configured. A path that couldn't be
/// expanded holds every finding, so a profile has nothing to freeze of it.
fn held_now(policy: &Policy) -> Vec<HeldEntry> {
    let lists = [
        (HeldKind::Keep, &policy.keep),
        (HeldKind::Exclude, &policy.exclude),
    ];
    let mut held: Vec<HeldEntry> = lists
        .into_iter()
        .flat_map(|(kind, paths)| {
            paths.iter().filter_map(move |held| {
                Some(HeldEntry {
                    kind,
                    path: held.forms.first()?.clone(),
                })
            })
        })
        .collect();
    held.sort();
    held.dedup();
    held
}

/// An entry suspended, with every value that changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suspended {
    /// Its rule.
    pub rule: RuleId,
    /// What changed, in [`ProfileChange`] order.
    pub changes: Vec<ProfileChange>,
}

/// What a profile authorizes now: each active entry's rule, with its method and the
/// roots it may remove under, for exactly the config it was checked against. Made only
/// by [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileGrant {
    /// The digest of the policy it was checked against ([`Judgement::policy`](crate::Judgement::policy)).
    policy: Digest,
    rules: BTreeMap<RuleId, Granted>,
}

/// What a grant allows for one rule.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Granted {
    /// The rule as it was checked ([`Judgement::rule_digest`](crate::Judgement::rule_digest)).
    rule: Digest,
    method: Method,
    roots: Vec<VerifiedRoot>,
}

impl ProfileGrant {
    /// Whether `entry` is within the grant: its rule is an active entry's, judged under
    /// the policy and the rule the grant was checked against, removed by the frozen
    /// method, below one of the entry's frozen roots with the identity it had then.
    pub(crate) fn admits(&self, entry: &PlanEntry) -> bool {
        let judgement = entry.judgement();
        judgement.policy == self.policy
            && self.rules.get(&judgement.rule).is_some_and(|granted| {
                granted.rule == judgement.rule_digest
                    && granted.method == entry.method()
                    && granted.method == judgement.method
                    && granted.roots.contains(&entry.anchor().root)
            })
    }
}

/// A profile checked against the current config and scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileCheck {
    name: ProfileName,
    active: Vec<RuleId>,
    suspended: Vec<Suspended>,
    pending: Vec<RuleId>,
    gone: Vec<RuleId>,
    left_out: Vec<RuleId>,
    grant: Option<ProfileGrant>,
    selection: Selection,
}

impl ProfileCheck {
    /// The profile's name.
    #[must_use]
    pub const fn name(&self) -> &ProfileName {
        &self.name
    }

    /// The rules of its active entries: every frozen value equals the current one.
    #[must_use]
    pub fn active(&self) -> &[RuleId] {
        &self.active
    }

    /// Its included entries that aren't active, with what changed.
    #[must_use]
    pub fn suspended(&self) -> &[Suspended] {
        &self.suspended
    }

    /// Rules proposing removal now that it has no entry for: new since it was made.
    /// Never active.
    #[must_use]
    pub fn pending(&self) -> &[RuleId] {
        &self.pending
    }

    /// Its entries whose rule kipple no longer has.
    #[must_use]
    pub fn gone(&self) -> &[RuleId] {
        &self.gone
    }

    /// Its entries `--only` left out.
    #[must_use]
    pub fn left_out(&self) -> &[RuleId] {
        &self.left_out
    }

    /// What it authorizes, when an entry is active and it was checked against a scan.
    #[must_use]
    pub const fn grant(&self) -> Option<&ProfileGrant> {
        self.grant.as_ref()
    }

    /// The scan's Safe findings of active entries below their frozen roots: what a
    /// profile run plans. Empty without a grant.
    #[must_use]
    pub const fn selection(&self) -> &Selection {
        &self.selection
    }

    /// What a receipt says of the run: the profile, and what it left out.
    #[must_use]
    pub fn summary(&self) -> ProfileRun {
        ProfileRun {
            name: self.name.clone(),
            suspended: self.suspended.iter().map(|one| one.rule.clone()).collect(),
            pending: self.pending.clone(),
        }
    }
}

/// What a receipt records of a profile run (`kipple.receipt/v1` `profile`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRun {
    /// The profile.
    pub name: ProfileName,
    /// Its suspended entries' rules.
    pub suspended: Vec<RuleId>,
    /// The rules pending review.
    pub pending: Vec<RuleId>,
}

/// Checks `profile` against `scene`.
///
/// Each entry is active only while every value it froze equals the current one, and only
/// active entries are authorized, for the Safe findings of the scan below their frozen
/// roots. Stateless: putting a value back as it was makes its entry active again, at
/// exactly its frozen scope.
#[must_use]
pub fn check(profile: &Profile, scene: &ProfileScene<'_>) -> ProfileCheck {
    let mut checked = compare(
        &profile.contents,
        scene.judge,
        scene.integrations,
        Some(scene),
    );
    if scene.scan.outcome != ScanOutcome::Completed {
        // A scan that stopped early proves nothing about the roots it didn't reach.
        return checked;
    }
    let granted: BTreeMap<RuleId, Granted> = checked
        .active
        .iter()
        .filter_map(|rule| {
            let entry = profile.contents.entry(rule)?;
            let now = scene.judge.rules.iter().find(|now| now.id == *rule)?;
            let granted = Granted {
                rule: Digest::of_debug(now),
                method: entry.method,
                roots: entry
                    .roots
                    .iter()
                    .filter_map(FrozenRoot::verified)
                    .collect(),
            };
            Some((rule.clone(), granted))
        })
        .collect();
    if granted.is_empty() {
        return checked;
    }
    checked.selection = selection(&granted, scene);
    checked.grant = Some(ProfileGrant {
        policy: Digest::of_debug(scene.judge.policy),
        rules: granted,
    });
    checked
}

/// Checks `profile` against the current config alone, without a scan: every change but
/// one of roots, and no grant (`kipple profile list`).
#[must_use]
pub fn check_config(
    profile: &Profile,
    judge: Judge<'_>,
    integrations: &IntegrationDigests,
) -> ProfileCheck {
    compare(&profile.contents, judge, integrations, None)
}

/// Each entry of `contents` compared with the current config, and with the scan's roots
/// when `scanned`; no grant yet.
fn compare(
    contents: &ProfileContents,
    judge: Judge<'_>,
    integrations: &IntegrationDigests,
    scanned: Option<&ProfileScene<'_>>,
) -> ProfileCheck {
    let removal = removal_rules(judge.rules);
    let mut whole = Vec::new();
    if contents.policy_version != POLICY_VERSION {
        whole.push(ProfileChange::PolicyVersion);
    }
    let held = held_now(judge.policy);
    if !contents.held.iter().all(|frozen| held.contains(frozen)) {
        whole.push(ProfileChange::Held);
    }
    let mut checked = ProfileCheck {
        name: contents.name.clone(),
        active: Vec::new(),
        suspended: Vec::new(),
        pending: removal
            .iter()
            .filter(|rule| contents.entry(&rule.id).is_none())
            .map(|rule| rule.id.clone())
            .collect(),
        gone: Vec::new(),
        left_out: Vec::new(),
        grant: None,
        selection: Selection::default(),
    };
    for entry in &contents.entries {
        let Some(rule) = removal.iter().find(|rule| rule.id == entry.rule) else {
            checked.gone.push(entry.rule.clone());
            continue;
        };
        if entry.scope == EntryScope::LeftOut {
            checked.left_out.push(entry.rule.clone());
            continue;
        }
        let mut changes = changes(entry, rule, judge, integrations, scanned);
        changes.extend(whole.iter().copied());
        changes.sort();
        if changes.is_empty() {
            checked.active.push(entry.rule.clone());
        } else {
            checked.suspended.push(Suspended {
                rule: entry.rule.clone(),
                changes,
            });
        }
    }
    checked.pending.sort();
    checked
}

/// What differs between `entry` and `rule` as configured, and as resolved when
/// `scanned`.
fn changes(
    entry: &ProfileEntry,
    rule: &Rule,
    judge: Judge<'_>,
    integrations: &IntegrationDigests,
    scanned: Option<&ProfileScene<'_>>,
) -> Vec<ProfileChange> {
    let policy = judge.policy;
    let id = &rule.id;
    let roots = scanned.map(|scene| roots_now(rule, scene.scan, scene.roots));
    [
        (rule_digest(rule) != entry.rule_digest, ProfileChange::Rule),
        (
            integrations.get(rule.integration) != Some(entry.integration_digest),
            ProfileChange::Integration,
        ),
        (policy.level_of(id) != entry.level, ProfileChange::Level),
        (
            policy.configured_window(id) != entry.recent_days,
            ProfileChange::RecentWindow,
        ),
        (state(policy, id) != entry.enabled, ProfileChange::Enabled),
        (
            judge.methods.choice(id).resolve() != entry.method,
            ProfileChange::Method,
        ),
        (
            roots.is_some_and(|roots| roots != entry.roots),
            ProfileChange::Roots,
        ),
    ]
    .into_iter()
    .filter_map(|(differs, change)| differs.then_some(change))
    .collect()
}

/// The findings of `scene`'s scan a profile run plans: each of a granted rule, Safe now,
/// below one of its rule's frozen roots that has an identity.
fn selection(granted: &BTreeMap<RuleId, Granted>, scene: &ProfileScene<'_>) -> Selection {
    let policy = scene.judge.policy;
    Selection(
        scene
            .scan
            .findings
            .iter()
            .filter(|finding| {
                granted.get(&finding.rule).is_some_and(|granted| {
                    granted
                        .roots
                        .iter()
                        .any(|root| finding.path.starts_with(&root.path))
                })
            })
            .filter(|finding| finding.verdict(policy) == Some(Verdict::Safe))
            .map(|finding| finding.id)
            .collect(),
    )
}

/// What `profile review` would store, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileReview {
    /// The profile frozen again, or `None` when nothing changed: nothing to review.
    pub draft: Option<DraftProfile>,
    /// Each included entry frozen again, with what changed.
    pub refrozen: Vec<Suspended>,
    /// The rules `--add` includes.
    pub added: Vec<RuleId>,
    /// Entries dropped because their rule is gone.
    pub dropped: Vec<RuleId>,
    /// Rules pending review that aren't added.
    pub not_added: Vec<RuleId>,
}

/// What `profile review` stores for `profile`.
///
/// Every suspended entry is frozen again as `scene` configures and resolves it now, the
/// kept and excluded paths and the policy version when they changed, each rule `add`
/// names is included (a pending rule, or one `--only` left out), and entries whose rule
/// is gone are dropped. It never adds a rule that isn't named.
///
/// # Errors
/// When the scan didn't finish, `add` names a rule that isn't a built-in rule proposing
/// removal here, a rule's integration isn't known, or the profile would include no rule:
/// every included rule is gone and `add` names none.
pub fn review(
    profile: &Profile,
    scene: &ProfileScene<'_>,
    add: &BTreeSet<RuleId>,
) -> Result<ProfileReview, FreezeError> {
    if scene.scan.outcome != ScanOutcome::Completed {
        return Err(FreezeError::ScanIncomplete);
    }
    let removal = removal_rules(scene.judge.rules);
    if let Some(named) = add
        .iter()
        .find(|named| !removal.iter().any(|rule| rule.id == **named))
    {
        return Err(FreezeError::NotRemovable(named.clone()));
    }
    let checked = check(profile, scene);
    let old = &profile.contents;
    let mut review = ProfileReview {
        draft: None,
        refrozen: checked.suspended.clone(),
        added: Vec::new(),
        dropped: checked.gone.clone(),
        not_added: checked
            .pending
            .iter()
            .filter(|rule| !add.contains(*rule))
            .cloned()
            .collect(),
    };
    let mut entries = Vec::with_capacity(removal.len());
    for rule in &removal {
        let frozen = old.entry(&rule.id);
        let suspended = checked.suspended.iter().any(|one| one.rule == rule.id);
        let entry = match frozen {
            Some(frozen) if frozen.scope == EntryScope::Included && !suspended => frozen.clone(),
            Some(frozen) if frozen.scope == EntryScope::LeftOut && !add.contains(&rule.id) => {
                frozen.clone()
            }
            None if !add.contains(&rule.id) => continue,
            Some(_) | None => {
                if !suspended {
                    review.added.push(rule.id.clone());
                }
                entry_now(rule, EntryScope::Included, scene)?
            }
        };
        entries.push(entry);
    }
    if review.refrozen.is_empty() && review.added.is_empty() && review.dropped.is_empty() {
        return Ok(review);
    }
    if !entries
        .iter()
        .any(|entry| entry.scope == EntryScope::Included)
    {
        return Err(FreezeError::Empty);
    }
    entries.sort_by(|a, b| a.rule.cmp(&b.rule));
    let changed = |change| {
        checked
            .suspended
            .iter()
            .any(|one| one.changes.contains(&change))
    };
    let held = if changed(ProfileChange::Held) {
        held_now(scene.judge.policy)
    } else {
        old.held.clone()
    };
    let policy_version = if changed(ProfileChange::PolicyVersion) {
        POLICY_VERSION
    } else {
        old.policy_version
    };
    review.draft = Some(DraftProfile {
        contents: ProfileContents {
            name: old.name.clone(),
            created: old.created,
            policy_version,
            held,
            entries,
        },
    });
    Ok(review)
}
