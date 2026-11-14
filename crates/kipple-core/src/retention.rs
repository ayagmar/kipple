//! Which releases of a store may go (`docs/06-adapters-and-rules.md` §4, spec M4 pinned
//! 12). Whether one may depends on the others in its group, so this is a decision the core
//! makes, from what an integration read.
//!
//! It keeps what the tool's own prune keeps, and never less: the selected release, the
//! rollback the store names and anything a program runs. When the store names no rollback
//! in a group, the newest release left there stays in its place. Whatever a component
//! records answers for every group of it, so a component that can't be relied on keeps
//! every release of it, and so does anything that stops the tool's own prune of it: an
//! install bringing a release in, a process under a release already set aside, or one that
//! may use any of its releases but couldn't be inspected. A version that can't be ordered
//! keeps its group, and a group with a member that wasn't measured isn't settled at all,
//! which holds it too.

use std::cmp::Ordering;
use std::fmt;
use std::path::PathBuf;

use crate::policy::Activity;
use crate::size::Completeness;

named! {
    /// What a release is to its store.
    ReleaseRole {
        /// Installed, and neither selected nor kept for a rollback.
        Installed = "installed",
        /// The one the store's selection points to.
        Active = "active",
        /// The one the store would roll back to.
        Previous = "previous",
        /// One an install in progress is bringing in.
        PendingInstall = "pending-install",
        /// One already set aside for removal.
        Retired = "retired",
    }
}

named! {
    /// Why a store's group of releases can't be settled.
    GroupGap {
        /// Its selection is missing, dangling, or points outside the store.
        Selection = "selection",
        /// The history that names its rollback is missing.
        HistoryMissing = "history-missing",
        /// That history can't be parsed.
        HistoryMalformed = "history-malformed",
        /// That history was written for another selection.
        HistoryStale = "history-stale",
        /// An installer left a marker of work in progress.
        InstallerBusy = "installer-busy",
        /// Whether a program uses one of its releases isn't known.
        ActivityUnknown = "activity-unknown",
        /// A process runs from a release already set aside.
        RetiredRunning = "retired-running",
        /// Part of its store couldn't be read, so its listing may be short.
        Unreadable = "unreadable",
        /// A release's version can't be ordered against the others.
        UnorderedVersion = "unordered-version",
        /// Its store changed between the scan and reading it again at apply.
        Changed = "changed",
    }
}

/// Which releases are compared with each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseGroup {
    /// The component, such as a tool's command line or its daemon.
    pub component: String,
    /// The platform it was built for.
    pub target: String,
}

/// A release's version as its store names it. Ordered by version precedence: a prerelease
/// comes before its release, and text that isn't a version before any version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseVersion(pub String);

impl ReleaseVersion {
    /// Whether it reads as a version at all.
    #[must_use]
    pub fn is_version(&self) -> bool {
        self.parts().is_some()
    }

    /// The numbers of its core and its prerelease, if it reads as a version.
    fn parts(&self) -> Option<(Vec<u64>, Option<&str>)> {
        let (core, prerelease) = match self.0.split_once('-') {
            Some((core, prerelease)) => (core, Some(prerelease)),
            None => (self.0.as_str(), None),
        };
        let numbers = core.split('.').map(|part| part.parse().ok());
        Some((numbers.collect::<Option<_>>()?, prerelease))
    }
}

impl Ord for ReleaseVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let precedence = match (self.parts(), other.parts()) {
            (Some(ours), Some(theirs)) => ours
                .0
                .cmp(&theirs.0)
                .then_with(|| prerelease_order(ours.1, theirs.1)),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
        };
        precedence.then_with(|| self.0.cmp(&other.0))
    }
}

impl PartialOrd for ReleaseVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for ReleaseVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How two prereleases of one core compare: none comes after any, and otherwise field by
/// field, numbers by value and before words, with the shorter first when one runs out.
fn prerelease_order(ours: Option<&str>, theirs: Option<&str>) -> Ordering {
    let (ours, theirs) = match (ours, theirs) {
        (None, None) => return Ordering::Equal,
        (None, Some(_)) => return Ordering::Greater,
        (Some(_), None) => return Ordering::Less,
        (Some(ours), Some(theirs)) => (ours, theirs),
    };
    let mut theirs_fields = theirs.split('.');
    for our_field in ours.split('.') {
        let Some(their_field) = theirs_fields.next() else {
            return Ordering::Greater;
        };
        let order = field_order(our_field, their_field);
        if order != Ordering::Equal {
            return order;
        }
    }
    if theirs_fields.next().is_some() {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

/// How one field of two prereleases compares.
fn field_order(ours: &str, theirs: &str) -> Ordering {
    match (ours.parse::<u64>(), theirs.parse::<u64>()) {
        (Ok(our_number), Ok(their_number)) => our_number.cmp(&their_number),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => ours.cmp(theirs),
    }
}

/// Whether what the store says about a release can be relied on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseState {
    /// It can.
    Sound,
    /// It can't, and so nothing in its group can go.
    Blocked(GroupGap),
}

/// What an integration read about one release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFacts {
    /// The group it is compared in.
    pub group: ReleaseGroup,
    /// Its version.
    pub version: ReleaseVersion,
    /// What it is to its store.
    pub role: ReleaseRole,
    /// Its files, whose identities show a process running it from elsewhere.
    pub files: Vec<PathBuf>,
    /// The locks upstream takes before it changes the store.
    pub locks: Vec<PathBuf>,
    /// Whether its store's records can be relied on.
    pub state: ReleaseState,
}

/// One release as it is settled.
#[derive(Debug, Clone, Copy)]
pub struct ReleaseView<'finding> {
    /// What its integration read.
    pub facts: &'finding ReleaseFacts,
    /// How completely it was measured, when it was.
    pub measured: Option<Completeness>,
    /// Whether a program uses it.
    pub activity: &'finding Activity,
}

/// Why a release stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retained {
    /// It is the selected one.
    Active,
    /// It is the store's rollback.
    Previous,
    /// The store names no rollback there, and it is the newest left.
    NewestLeft,
    /// A program runs it.
    Running,
    /// An install is bringing it in.
    Pending,
    /// Its group can't be settled.
    GroupBlocked(GroupGap),
}

/// What retention decided about a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    /// Nothing has decided, which holds it.
    Unsettled,
    /// It may go.
    Candidate,
    /// It stays.
    Retained(Retained),
}

/// What retention decides for each of `views`, in their order.
///
/// A view's group is its component and target; a [`Retired`](ReleaseRole::Retired) one belongs
/// to no group, since what is left of it may name no target, and answers for its component.
#[must_use]
pub fn settle(views: &[ReleaseView<'_>]) -> Vec<Settlement> {
    (0..views.len()).map(|at| decide(at, views)).collect()
}

/// What retention decides for the view at `at` among `views`.
fn decide(at: usize, views: &[ReleaseView<'_>]) -> Settlement {
    let Some(view) = views.get(at) else {
        return Settlement::Unsettled;
    };
    let facts = view.facts;
    if let Some(gap) = component_gap(&facts.group.component, views) {
        return Settlement::Retained(Retained::GroupBlocked(gap));
    }
    match facts.role {
        ReleaseRole::Retired => return Settlement::Unsettled,
        ReleaseRole::PendingInstall => return Settlement::Retained(Retained::Pending),
        ReleaseRole::Installed | ReleaseRole::Active | ReleaseRole::Previous => {}
    }
    let installing = views.iter().any(|other| {
        other.facts.group.component == facts.group.component
            && other.facts.role == ReleaseRole::PendingInstall
    });
    if installing {
        return Settlement::Retained(Retained::GroupBlocked(GroupGap::InstallerBusy));
    }
    let group: Vec<(usize, &ReleaseView<'_>)> = views
        .iter()
        .enumerate()
        .filter(|(_, other)| other.facts.role != ReleaseRole::Retired)
        .filter(|(_, other)| other.facts.group == facts.group)
        .collect();
    if group.iter().any(|(_, member)| !is_measured(member)) {
        return Settlement::Unsettled;
    }
    if let Some(gap) = group_gap(&group) {
        return Settlement::Retained(Retained::GroupBlocked(gap));
    }
    if let Some(why) = kept(view) {
        return Settlement::Retained(why);
    }
    let has_rollback = group
        .iter()
        .any(|(_, member)| member.facts.role == ReleaseRole::Previous);
    let newest_left = group
        .iter()
        .filter(|(_, member)| kept(member).is_none())
        .map(|(_, member)| &member.facts.version)
        .max();
    if !has_rollback && newest_left == Some(&facts.version) {
        Settlement::Retained(Retained::NewestLeft)
    } else {
        Settlement::Candidate
    }
}

/// Why everything of `component` stays, if something does: what it records can't be relied
/// on, a release of it already set aside is in use, or whether a program uses one of its
/// releases isn't known, as each stops the tool's own prune of it. What it records is read
/// before any release's role, so a blocked component keeps even its selected release.
fn component_gap(component: &str, views: &[ReleaseView<'_>]) -> Option<GroupGap> {
    let members = || {
        views
            .iter()
            .filter(move |view| view.facts.group.component == component)
    };
    let state = members().find_map(|view| match view.facts.state {
        ReleaseState::Blocked(gap) => Some(gap),
        ReleaseState::Sound => None,
    });
    let retired_running = || {
        members()
            .filter(|view| view.facts.role == ReleaseRole::Retired)
            .any(|view| matches!(view.activity, Activity::InUse { .. }))
            .then_some(GroupGap::RetiredRunning)
    };
    let unknown = || {
        members()
            .any(|view| matches!(view.activity, Activity::Unknown(_)))
            .then_some(GroupGap::ActivityUnknown)
    };
    state.or_else(retired_running).or_else(unknown)
}

/// Why everything of one group stays, if something does: a release's version can't be
/// ordered against the others.
fn group_gap(group: &[(usize, &ReleaseView<'_>)]) -> Option<GroupGap> {
    let unordered = group
        .iter()
        .any(|(_, view)| !view.facts.version.is_version());
    unordered.then_some(GroupGap::UnorderedVersion)
}

/// Whether `view` was measured to the end: a release stopped short of may hold anything.
const fn is_measured(view: &ReleaseView<'_>) -> bool {
    matches!(
        view.measured,
        Some(Completeness::Complete | Completeness::Incomplete { .. })
    )
}

/// Why `view` stays whatever the rest of its group is, if it does.
const fn kept(view: &ReleaseView<'_>) -> Option<Retained> {
    match view.facts.role {
        ReleaseRole::Active => return Some(Retained::Active),
        ReleaseRole::Previous => return Some(Retained::Previous),
        ReleaseRole::PendingInstall => return Some(Retained::Pending),
        ReleaseRole::Installed | ReleaseRole::Retired => {}
    }
    match view.activity {
        Activity::InUse { .. } => Some(Retained::Running),
        Activity::Idle | Activity::IdleExceptSystem { .. } | Activity::Unknown(_) => None,
    }
}
