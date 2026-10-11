//! Buckets: the host's ordered, exhaustive classification of each finding into Safe,
//! Review or Protected (`docs/04-safety-model.md` §2a). Rules never declare a bucket; it
//! is derived here from the facts the host established and the consequences the
//! integration verified.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::probe::ProbeError;
use crate::rule::Method;

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
        /// The default: rebuilt, reinstalled and re-downloaded things are Safe when they
        /// lose nothing more than cache warmth and offline use.
        Balanced = "balanced",
    }
}

impl Level {
    /// The recoveries a Safe finding may have at this level.
    const fn recoveries(self) -> &'static [Recovery] {
        match self {
            Self::Balanced => &[
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
            Self::Balanced => &[Loss::CacheWarmth, Loss::OfflineAbility],
        }
    }

    /// The level's recent window, in days.
    #[must_use]
    pub const fn recent_days(self) -> u16 {
        match self {
            Self::Balanced => 7,
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
    /// Not everything inside could be read, so a checkout inside it can't be ruled out.
    Unwalked,
}

/// Whether a program is using the finding, from the running executables (D-035).
#[derive(Debug, Clone)]
pub enum Activity {
    /// Every process was listed and identified, and none runs a file inside it.
    Idle,
    /// A process runs a file inside it.
    InUse {
        /// The process.
        pid: u32,
        /// The file it runs.
        executable: PathBuf,
    },
    /// Some processes couldn't be identified, so one of them might be using it.
    Unknown(ActivityGap),
}

/// Why the running executables don't prove a finding idle.
#[derive(Debug, Clone)]
pub enum ActivityGap {
    /// The processes couldn't be listed at all.
    NoListing(ProbeError),
    /// The listing may leave processes out, without an error for each.
    Partial(String),
    /// This many processes run something kipple couldn't identify.
    Uninspected(usize),
}

/// How long ago something inside the finding last changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modified {
    /// The newest modification time of everything inside, this long ago. Zero when it is
    /// in the future.
    Ago(Duration),
    /// Not every entry could be read, or the finding couldn't be measured.
    Unknown,
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
}

named! {
    /// Why a finding is Protected. Each is shown on its own, never lumped together.
    Hold {
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
        /// A program runs a file inside it.
        InUse = "in-use",
        /// Not every running program could be identified.
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

/// The bucket of a finding with `facts` at `level`.
///
/// The first matching row of `docs/04-safety-model.md` §2a wins: not eligible is
/// Protected, eligible and within the level is Safe, and every other eligible finding is
/// Review.
#[must_use]
pub fn classify(facts: &Facts, level: Level) -> Verdict {
    let holds = holds(facts);
    if !holds.is_empty() {
        return Verdict::Protected(holds);
    }
    let cautions = cautions(facts, level);
    if cautions.is_empty() {
        Verdict::Safe
    } else {
        Verdict::Review(cautions)
    }
}

/// Every reason the finding isn't eligible. Unknown never makes it Review: it holds.
fn holds(facts: &Facts) -> Vec<Hold> {
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
        Tracked::InsideRepository { .. } | Tracked::Unchecked(_) | Tracked::Unwalked => {
            holds.push(Hold::UnknownGit);
        }
    }
    match facts.activity {
        Activity::Idle => {}
        Activity::InUse { .. } => holds.push(Hold::InUse),
        Activity::Unknown(_) => holds.push(Hold::UnknownActivity),
    }
    if facts.modified == Modified::Unknown {
        holds.push(Hold::UnknownModified);
    }
    holds
}

/// Every reason an eligible finding falls short of Safe at `level`.
fn cautions(facts: &Facts, level: Level) -> Vec<Caution> {
    let mut cautions = Vec::new();
    let window = u64::from(facts.recent_window_days(level)) * 24 * 60 * 60;
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

impl Facts {
    /// The effective recent window at `level`, in days: the longer of the level's and the
    /// rule's (`docs/04-safety-model.md` §2a).
    #[must_use]
    pub fn recent_window_days(&self, level: Level) -> u16 {
        let level_days = level.recent_days();
        self.recent_days
            .map_or(level_days, |days| days.max(level_days))
    }
}
