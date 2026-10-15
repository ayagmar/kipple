//! Buckets: the host's ordered, exhaustive classification of each finding into Safe,
//! Review or Protected (`docs/04-safety-model.md` §2a). Rules never declare a bucket; it
//! is derived here from the facts the host established and the consequences the
//! integration verified.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::probe::{FileIdentity, ProbeError};
use crate::rule::{Method, RuleId};

named! {
    /// How a removed finding comes back.
    Recovery {
        /// The tool regenerates it when it needs it.
        Automatic = "automatic",
        /// A local rebuild, with no network.
        RebuildLocal = "rebuild-local",
        /// The package manager installs it again, possibly downloading.
        Reinstall = "reinstall",
        /// It is downloaded again.
        Redownload = "redownload",
        /// It doesn't come back.
        None = "none",
    }
}

named! {
    /// Something removing a finding loses, besides the bytes.
    Loss {
        /// The next run is slower until the cache is warm again.
        CacheWarmth = "cache-warmth",
        /// An older version can no longer be rolled back to.
        RollbackAbility = "rollback-ability",
        /// It can't be restored without the network.
        OfflineAbility = "offline-ability",
        /// Records of the past are gone.
        History = "history",
    }
}

named! {
    /// How expensive bringing a finding back is.
    Cost {
        /// Cheap.
        Low = "low",
        /// Expensive, such as a very large build. Never Safe.
        High = "high",
        /// Not known. Doesn't block Safe; shown as "restore cost unknown".
        Unknown = "unknown",
    }
}

/// What removing a finding costs, from the integration's verified semantics. Never taken
/// from a rule's prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Consequence {
    /// How it comes back.
    pub recovery: Recovery,
    /// What is lost besides the bytes, each once.
    pub loss: &'static [Loss],
    /// How expensive coming back is.
    pub cost: Cost,
}

named! {
    /// How careful kipple is: where the line between Safe and Review sits. It never moves
    /// anything out of Protected.
    Level {
        /// Only build output and regenerated caches are Safe: they come back without the
        /// network and lose nothing more than cache warmth.
        Careful = "careful",
        /// The default: rebuilt, reinstalled and re-downloaded things are Safe when they
        /// lose nothing more than cache warmth and offline use.
        Balanced = "balanced",
        /// Balanced, plus things whose removal costs a rollback.
        Thorough = "thorough",
    }
}

impl Level {
    /// The recoveries a Safe finding may have at this level.
    const fn recoveries(self) -> &'static [Recovery] {
        match self {
            Self::Careful => &[Recovery::Automatic, Recovery::RebuildLocal],
            Self::Balanced | Self::Thorough => &[
                Recovery::Automatic,
                Recovery::RebuildLocal,
                Recovery::Reinstall,
                Recovery::Redownload,
            ],
        }
    }

    /// The losses a Safe finding may have at this level.
    const fn losses(self) -> &'static [Loss] {
        match self {
            Self::Careful => &[Loss::CacheWarmth],
            Self::Balanced => &[Loss::CacheWarmth, Loss::OfflineAbility],
            Self::Thorough => &[
                Loss::CacheWarmth,
                Loss::OfflineAbility,
                Loss::RollbackAbility,
            ],
        }
    }

    /// The level's recent window, in days.
    #[must_use]
    pub const fn recent_days(self) -> u16 {
        match self {
            Self::Careful => 14,
            Self::Balanced => 7,
            Self::Thorough => 2,
        }
    }
}

/// What a finding's rule proposes, as far as the host lets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proposal {
    /// Report only: kipple never acts on it.
    Report,
    /// Report only, with the integration's advice on the tool's own way to clean it.
    Advice,
    /// Remove its files, by one of these methods.
    Remove(Vec<Method>),
}

/// Whether a git repository could track files inside the finding
/// (`git.tracked_files_in_item`, `docs/06-adapters-and-rules.md` §3).
#[derive(Debug, Clone)]
pub enum Tracked {
    /// No `.git` in the finding, in any directory above it up to the file system's root,
    /// or anywhere inside it, and every one of them could be checked: nothing inside is
    /// tracked.
    NoRepository,
    /// A repository holds it: whether it tracks anything inside is not known yet.
    InsideRepository {
        /// The directory holding `.git`.
        work_tree: PathBuf,
    },
    /// A directory above it couldn't be checked for `.git`.
    Unchecked(ProbeError),
    /// A git checkout lives inside it, so it holds files git may track.
    HoldsRepository {
        /// The directory inside it holding `.git`.
        work_tree: PathBuf,
    },
    /// Not everything inside was measured, so a checkout inside it can't be ruled out.
    Unwalked(WalkGap),
}

/// Why the walk that measured a finding didn't see everything inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkGap {
    /// Some entries couldn't be read, or the finding couldn't be measured at all.
    Unread,
    /// Everything was read except directories on another file system, such as a mount
    /// or a btrfs subvolume, which the walk doesn't enter.
    OtherFileSystem,
    /// Everything was read, but an entry didn't say when it last changed.
    Untimed,
}

/// Whether a program is using the finding, from the process snapshot (D-035, D-050).
#[derive(Debug, Clone)]
pub enum Activity {
    /// Every process that counts was inspected, and none shows it in use.
    Idle,
    /// Every process that counts was inspected, and none shows it in use, but it is in the
    /// home directory and some processes couldn't be fully inspected: root's, `SYSTEM`'s
    /// or ones running with root's rights that weren't counted (D-050, D-052), or the current user's that were judged by their
    /// command line alone (D-052). Not the same as every process inspected.
    IdleExceptSystem {
        /// How many processes of root, `SYSTEM` or root's rights were left out.
        unchecked: usize,
        /// How many of the current user's processes were judged by their command line.
        by_command_line: usize,
    },
    /// A process shows it in use.
    InUse {
        /// The process.
        pid: u32,
        /// How.
        seen: Seen,
    },
    /// Some processes that count couldn't be inspected, so one of them might use it.
    Unknown(ActivityGap),
}

/// How a process showed a finding in use. Only the path that matched is kept, never the
/// rest of its command line or environment (docs/04 §8a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// It runs this executable, inside the finding.
    Executable(PathBuf),
    /// Its working directory is inside the finding.
    WorkingDirectory(PathBuf),
    /// One of its arguments names this path inside the finding.
    Argument(PathBuf),
    /// One of its environment variables names this path inside the finding.
    Environment(PathBuf),
    /// It has this file inside the finding mapped into its memory.
    MappedFile(PathBuf),
    /// It is the finding's runtime, working in the finding's project at this directory.
    Runtime(PathBuf),
}

/// Why the processes don't prove a finding idle.
#[derive(Debug, Clone)]
pub enum ActivityGap {
    /// The processes couldn't be listed at all.
    NoListing(ProbeError),
    /// The listing may leave processes out, without an error for each.
    Partial(String),
    /// This many processes that count couldn't be inspected: some path they could show
    /// couldn't be read, or they see files through another mount namespace or root.
    Uninspected(usize),
}

/// How long ago something inside the finding last changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modified {
    /// The newest modification time of everything inside, this long ago. Zero when it is
    /// in the future.
    Ago(Duration),
    /// The walk didn't see everything inside, or a time wasn't reported.
    Unknown(WalkGap),
}

/// What the host learned about a finding once it was measured: everything its bucket is
/// derived from.
#[derive(Debug, Clone)]
pub struct Facts {
    /// What its rule proposes.
    pub proposal: Proposal,
    /// Whether its integration recognised what its tool wrote inside: a
    /// [`Signature`](crate::Evidence::Signature) among its evidence.
    pub layout_verified: bool,
    /// What removing it costs, when its integration verified that.
    pub consequence: Option<Consequence>,
    /// Whether a repository could track files inside it.
    pub tracked: Tracked,
    /// Whether a program uses it.
    pub activity: Activity,
    /// How long ago it last changed.
    pub modified: Modified,
    /// Its rule's recent window, when it asks for more than the level's.
    pub recent_days: Option<u16>,
    /// The file identities of the finding and of each directory above it, so a kept or
    /// excluded path matches it whatever path reaches it.
    pub lineage: Lineage,
}

/// The file identities of an entry, then of each directory above it up to the file
/// system's root. `None` where one couldn't be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lineage(pub Vec<Option<FileIdentity>>);

impl Lineage {
    /// The entry's own identity, when it was read.
    #[must_use]
    pub fn own(&self) -> Option<FileIdentity> {
        self.0.first().copied().flatten()
    }

    /// Whether every identity was read.
    #[must_use]
    pub fn is_known(&self) -> bool {
        !self.0.is_empty() && self.0.iter().all(Option::is_some)
    }

    fn contains(&self, identity: FileIdentity) -> bool {
        self.0.contains(&Some(identity))
    }
}

named! {
    /// Why a finding is Protected. Each is shown on its own, never lumped together.
    Hold {
        /// Its rule is turned off in config (`exclude.rules` or `rules.<id>.enabled`).
        Disabled = "disabled",
        /// It is at, inside or above a path the user keeps (`keep.paths`).
        Kept = "kept",
        /// It is at, inside or above a path excluded in config (`exclude.paths`), or that
        /// can't be ruled out because the home directory is unknown.
        Excluded = "excluded",
        /// Its rule only reports.
        ReportOnly = "report-only",
        /// Its rule only reports; the tool's own cleanup is suggested instead.
        Advice = "advice",
        /// Its tool's layout wasn't recognised inside it.
        UnknownLayout = "unknown-layout",
        /// What removing it costs isn't known.
        UnknownConsequence = "unknown-consequence",
        /// A git repository holds it, and whether it tracks files inside isn't known.
        UnknownGit = "unknown-git",
        /// A git checkout lives inside it.
        HoldsRepository = "holds-repository",
        /// A running program shows it in use.
        InUse = "in-use",
        /// Not every running program that counts could be inspected.
        UnknownActivity = "unknown-activity",
        /// Not everything inside could be read, so when it last changed isn't known.
        UnknownModified = "unknown-modified",
    }
}

named! {
    /// Why an eligible finding is in Review rather than Safe.
    Caution {
        /// It changed within the recent window.
        Recent = "recent",
        /// The way it comes back is beyond the level.
        Recovery = "recovery",
        /// It loses something beyond the level.
        Loss = "loss",
        /// Bringing it back is expensive.
        HighCost = "high-cost",
    }
}

named! {
    /// The home screen's grouping (`docs/03-functional-spec.md` §2 J1).
    Bucket {
        /// Passes every check at the level.
        Safe = "safe",
        /// Eligible, but loses something or is recent.
        Review = "review",
        /// Blocked, unknown, report-only or advice only.
        Protected = "protected",
    }
}

/// A finding's bucket with every reason for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Passes every check at the level.
    Safe,
    /// Eligible, for these reasons not Safe. Never empty.
    Review(Vec<Caution>),
    /// Not eligible, for these reasons. Never empty.
    Protected(Vec<Hold>),
}

impl Verdict {
    /// Its bucket.
    #[must_use]
    pub const fn bucket(&self) -> Bucket {
        match self {
            Self::Safe => Bucket::Safe,
            Self::Review(_) => Bucket::Review,
            Self::Protected(_) => Bucket::Protected,
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.bucket().name())
    }
}

/// How a rule's findings are classified, from config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuleSettings {
    /// Its own level, over the global one (`rules.<id>.level`).
    pub level: Option<Level>,
    /// Its own recent window, which can only lengthen the effective one
    /// (`rules.<id>.recent_days`).
    pub recent_days: Option<u16>,
    /// Turned off: its findings are held as `disabled` (`exclude.rules`, or
    /// `rules.<id>.enabled = false`).
    pub disabled: bool,
}

/// A path from `keep.paths` or `exclude.paths`, in every form a finding's path may take.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldPath {
    /// The absolute paths it means, compared by components and ignoring case: the path
    /// as configured with `~` expanded, and the same path below the directory a linked
    /// root leads to (D-041). Empty when it couldn't be expanded because the home
    /// directory is unknown, which holds every finding.
    pub forms: Vec<PathBuf>,
    /// The identities of what is there and of each directory above it, as far as they
    /// could be read: a finding with one of them, or below the first, is held whatever
    /// path reaches it.
    pub lineage: Lineage,
    /// The user's note, for a kept path.
    pub note: Option<String>,
}

impl HeldPath {
    /// Whether a finding at `path`, with `lineage`, is at, inside or above this path.
    /// Removing a directory above a kept path would remove it too.
    #[must_use]
    pub fn holds(&self, path: &Path, lineage: &Lineage) -> bool {
        if self.forms.is_empty() || self.forms.iter().any(|form| related(path, form)) {
            return true;
        }
        if let Some(identity) = self.lineage.own() {
            // A finding whose ancestry can't be read might be below it.
            if !lineage.is_known() || lineage.contains(identity) {
                return true;
            }
        }
        lineage.own().is_some_and(|own| self.lineage.contains(own))
    }

    /// Whether the directory at `path` is this path or inside it, by path alone: what the
    /// project walk doesn't descend into.
    #[must_use]
    pub fn covers(&self, path: &Path) -> bool {
        self.forms.iter().any(|form| starts_with(path, form))
    }
}

/// Whether one of `a` and `b` is the other or inside it.
fn related(a: &Path, b: &Path) -> bool {
    starts_with(a, b) || starts_with(b, a)
}

/// Whether `path` is `base` or inside it, comparing components and ignoring case on every
/// OS: a match only ever holds more.
fn starts_with(path: &Path, base: &Path) -> bool {
    let fold = |c: std::path::Component<'_>| c.as_os_str().to_string_lossy().to_lowercase();
    let mut path = path.components().map(fold);
    base.components()
        .map(fold)
        .all(|part| path.next() == Some(part))
}

/// Everything the user configured that decides buckets (`docs/03-functional-spec.md` §6).
///
/// The level, each rule's settings, and the paths kept and excluded. Every frontend gets
/// its verdicts from [`Policy::verdict`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// The global level (`care.level`).
    pub level: Level,
    /// The rules with settings of their own.
    pub rules: BTreeMap<RuleId, RuleSettings>,
    /// The paths kept (`keep.paths`).
    pub keep: Vec<HeldPath>,
    /// The paths excluded (`exclude.paths`).
    pub exclude: Vec<HeldPath>,
}

impl Default for Policy {
    fn default() -> Self {
        Self::at(Level::Balanced)
    }
}

impl Policy {
    /// `level` for every rule, and nothing kept, excluded or turned off.
    #[must_use]
    pub const fn at(level: Level) -> Self {
        Self {
            level,
            rules: BTreeMap::new(),
            keep: Vec::new(),
            exclude: Vec::new(),
        }
    }

    /// The level `rule`'s findings are classified at: its own, else the global one.
    #[must_use]
    pub fn level_of(&self, rule: &RuleId) -> Level {
        self.settings(rule).level.unwrap_or(self.level)
    }

    /// The effective recent window of `rule`'s finding with `facts`, in days: the longest
    /// of its level's, its rule's and the config's (`docs/04-safety-model.md` §2a).
    #[must_use]
    pub fn recent_window_days(&self, rule: &RuleId, facts: &Facts) -> u16 {
        let settings = self.settings(rule);
        let level = self.level_of(rule).recent_days();
        [facts.recent_days, settings.recent_days]
            .into_iter()
            .flatten()
            .fold(level, u16::max)
    }

    /// The kept path holding a finding at `path` with `lineage`, if any.
    #[must_use]
    pub fn kept(&self, path: &Path, lineage: &Lineage) -> Option<&HeldPath> {
        self.keep.iter().find(|kept| kept.holds(path, lineage))
    }

    /// The bucket of `rule`'s finding at `path` with `facts`.
    ///
    /// The first matching row of `docs/04-safety-model.md` §2a wins: not eligible is
    /// Protected, eligible and within the level is Safe, and every other eligible finding
    /// is Review. A finding the config turns off, keeps or excludes is never eligible.
    #[must_use]
    pub fn verdict(&self, rule: &RuleId, path: &Path, facts: &Facts) -> Verdict {
        let mut holds = self.config_holds(rule, path, facts);
        holds.extend(fact_holds(facts));
        if !holds.is_empty() {
            return Verdict::Protected(holds);
        }
        let cautions = cautions(
            facts,
            self.level_of(rule),
            self.recent_window_days(rule, facts),
        );
        if cautions.is_empty() {
            Verdict::Safe
        } else {
            Verdict::Review(cautions)
        }
    }

    fn settings(&self, rule: &RuleId) -> RuleSettings {
        self.rules.get(rule).copied().unwrap_or_default()
    }

    /// What the config holds a finding for. A kept path that couldn't be expanded is
    /// held as excluded: nothing is known to be kept there.
    fn config_holds(&self, rule: &RuleId, path: &Path, facts: &Facts) -> Vec<Hold> {
        let mut holds = Vec::new();
        if self.settings(rule).disabled {
            holds.push(Hold::Disabled);
        }
        let lineage = &facts.lineage;
        let kept = self.keep.iter().filter(|kept| kept.holds(path, lineage));
        let (unexpanded, kept): (Vec<_>, Vec<_>) = kept.partition(|kept| kept.forms.is_empty());
        if !kept.is_empty() {
            holds.push(Hold::Kept);
        }
        let excluded = self
            .exclude
            .iter()
            .any(|excluded| excluded.holds(path, lineage));
        if excluded || !unexpanded.is_empty() {
            holds.push(Hold::Excluded);
        }
        holds
    }
}

/// Every reason the facts make the finding ineligible. Unknown never makes it Review: it
/// holds.
fn fact_holds(facts: &Facts) -> Vec<Hold> {
    match facts.proposal {
        Proposal::Report => return vec![Hold::ReportOnly],
        Proposal::Advice => return vec![Hold::Advice],
        Proposal::Remove(_) => {}
    }
    let mut holds = Vec::new();
    if !facts.layout_verified {
        holds.push(Hold::UnknownLayout);
    }
    if facts.consequence.is_none() {
        holds.push(Hold::UnknownConsequence);
    }
    match facts.tracked {
        Tracked::NoRepository => {}
        Tracked::HoldsRepository { .. } => holds.push(Hold::HoldsRepository),
        Tracked::InsideRepository { .. } | Tracked::Unchecked(_) | Tracked::Unwalked(_) => {
            holds.push(Hold::UnknownGit);
        }
    }
    match facts.activity {
        Activity::Idle | Activity::IdleExceptSystem { .. } => {}
        Activity::InUse { .. } => holds.push(Hold::InUse),
        Activity::Unknown(_) => holds.push(Hold::UnknownActivity),
    }
    if matches!(facts.modified, Modified::Unknown(_)) {
        holds.push(Hold::UnknownModified);
    }
    holds
}

/// Every reason an eligible finding falls short of Safe at `level`, with a recent window
/// of `window_days`.
fn cautions(facts: &Facts, level: Level, window_days: u16) -> Vec<Caution> {
    let mut cautions = Vec::new();
    let window = u64::from(window_days) * 24 * 60 * 60;
    if let Modified::Ago(age) = facts.modified
        && age < Duration::from_secs(window)
    {
        cautions.push(Caution::Recent);
    }
    let Some(consequence) = &facts.consequence else {
        return cautions;
    };
    if !level.recoveries().contains(&consequence.recovery) {
        cautions.push(Caution::Recovery);
    }
    if consequence
        .loss
        .iter()
        .any(|loss| !level.losses().contains(loss))
    {
        cautions.push(Caution::Loss);
    }
    if consequence.cost == Cost::High {
        cautions.push(Caution::HighCost);
    }
    cautions
}
