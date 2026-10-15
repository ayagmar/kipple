//! Text for people: escaping and sizes.

use kipple_core::{
    Activity, ActivityGap, Bucket, Caution, Facts, Hold, Level, Loss, Modified, Proposal, Recovery,
    Tracked, Verdict, WalkGap,
};

use crate::json::{CompletenessOut, SizeOut};

/// `text` for the terminal: paths and OS messages may hold control characters.
pub(crate) fn escaped(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            let shown: Vec<char> = if c.is_control() {
                c.escape_default().collect()
            } else {
                vec![c]
            };
            shown
        })
        .collect()
}

/// A byte count in binary units, such as `8.0 MiB`.
pub(crate) fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if count < 1024 {
        return format!("{count} B");
    }
    let mut value = count;
    let mut unit = 0;
    while value >= 1024 * 1024 && unit + 1 < UNITS.len() {
        value /= 1024;
        unit += 1;
    }
    // One decimal, computed in integers: tenths of the unit, rounded down.
    let tenths = value * 10 / 1024;
    let name = UNITS.get(unit).copied().unwrap_or("PiB");
    format!("{}.{} {name}", tenths / 10, tenths % 10)
}

/// What removing an item frees: allocated storage where known, else apparent size. A
/// partial measurement is a lower bound.
pub(crate) fn size(size: Option<&SizeOut>) -> String {
    let Some(size) = size else {
        return "unsized".to_owned();
    };
    let shown = bytes(size.allocated.unwrap_or(size.apparent));
    match size.completeness {
        CompletenessOut::Complete => shown,
        CompletenessOut::Incomplete | CompletenessOut::Cancelled => format!(">= {shown}"),
    }
}

/// `count` findings, in words.
pub(crate) fn findings(count: usize) -> String {
    if count == 1 {
        "1 finding".to_owned()
    } else {
        format!("{count} findings")
    }
}

/// A bucket's name on screen.
pub(crate) const fn bucket(bucket: Bucket) -> &'static str {
    match bucket {
        Bucket::Safe => "Safe to clean",
        Bucket::Review => "Review",
        Bucket::Protected => "Protected",
    }
}

/// Why a finding is Protected, as a row shows it: never colour alone.
pub(crate) const fn hold(hold: Hold) -> &'static str {
    match hold {
        Hold::ReportOnly => "report only",
        Hold::Advice => "advice",
        Hold::UnknownLayout => "unknown: layout",
        Hold::UnknownConsequence => "unknown: what is lost",
        Hold::UnknownGit => "unknown: git",
        Hold::HoldsRepository => "blocked: git checkout",
        Hold::InUse => "blocked: in use",
        Hold::UnknownActivity => "unknown: in use",
        Hold::UnknownModified => "unknown: last change",
    }
}

/// Why an eligible finding is in Review, as a row shows it.
pub(crate) const fn caution(caution: Caution) -> &'static str {
    match caution {
        Caution::Recent => "recent",
        Caution::Recovery => "hard to bring back",
        Caution::Loss => "loses more",
        Caution::HighCost => "costly to restore",
    }
}

/// Every reason for `verdict`, joined for one line. Empty when Safe.
pub(crate) fn reasons(verdict: &Verdict) -> String {
    let labels: Vec<&str> = match verdict {
        Verdict::Safe => Vec::new(),
        Verdict::Review(cautions) => cautions.iter().map(|&c| caution(c)).collect(),
        Verdict::Protected(holds) => holds.iter().map(|&h| hold(h)).collect(),
    };
    labels.join(", ")
}

/// What kipple established about a finding, as labelled lines for a detail view: what
/// comes back and what is lost, then each check its bucket rests on.
pub(crate) fn facts(facts: &Facts, level: Level) -> Vec<(&'static str, String)> {
    let mut lines = Vec::new();
    if let Some(consequence) = &facts.consequence {
        lines.push(("Comes back", recovery(consequence.recovery).to_owned()));
        lines.push(("You lose", losses(consequence.loss)));
        lines.push(("Restore cost", consequence.cost.name().to_owned()));
    }
    let proposal = match &facts.proposal {
        Proposal::Report => "report only: kipple never changes it".to_owned(),
        Proposal::Advice => "report only: the tool's own cleanup is advised".to_owned(),
        Proposal::Remove(methods) => {
            let names: Vec<&str> = methods.iter().map(|m| m.name()).collect();
            format!("remove ({})", names.join(" or "))
        }
    };
    lines.push(("Proposes", proposal));
    if !matches!(facts.proposal, Proposal::Remove(_)) {
        return lines;
    }
    lines.push(("Git", tracked(&facts.tracked)));
    lines.push(("In use", activity(&facts.activity)));
    lines.push(("Last change", modified(facts, level)));
    lines
}

const fn recovery(recovery: Recovery) -> &'static str {
    match recovery {
        Recovery::Automatic => "the tool regenerates it when it needs it",
        Recovery::RebuildLocal => "the next build makes it again, without the network",
        Recovery::Reinstall => "the package manager installs it again, possibly downloading",
        Recovery::Redownload => "it is downloaded again",
        Recovery::None => "it doesn't come back",
    }
}

fn losses(losses: &[Loss]) -> String {
    if losses.is_empty() {
        return "nothing besides the space".to_owned();
    }
    let lost: Vec<&str> = losses
        .iter()
        .map(|loss| match loss {
            Loss::CacheWarmth => "a warm cache, so the next run is slower",
            Loss::RollbackAbility => "rolling back to an older version",
            Loss::OfflineAbility => "getting it back offline",
            Loss::History => "records of the past",
        })
        .collect();
    lost.join("; ")
}

fn tracked(tracked: &Tracked) -> String {
    match tracked {
        Tracked::NoRepository => {
            "no repository holds it or lives inside it, so git tracks nothing inside".to_owned()
        }
        Tracked::HoldsRepository { work_tree } => format!(
            "a git checkout lives inside it at {}",
            escaped(&work_tree.display().to_string())
        ),
        Tracked::Unwalked(gap) => format!(
            "{}, so a git checkout inside can't be ruled out",
            walk_gap(*gap)
        ),
        Tracked::InsideRepository { work_tree } => format!(
            "inside the repository at {}; whether git tracks files in it isn't checked yet",
            escaped(&work_tree.display().to_string())
        ),
        Tracked::Unchecked(error) => format!(
            "couldn't check for a repository: {}",
            escaped(&error.to_string())
        ),
    }
}

fn activity(activity: &Activity) -> String {
    match activity {
        Activity::Idle => "no running program runs an executable inside it".to_owned(),
        Activity::InUse { pid, executable } => format!(
            "process {pid} runs {}",
            escaped(&executable.display().to_string())
        ),
        Activity::Unknown(ActivityGap::Uninspected(count)) => {
            format!("{count} running programs couldn't be identified, so one of them might use it")
        }
        Activity::Unknown(ActivityGap::Partial(reason)) => format!(
            "the list of running programs may be incomplete ({})",
            escaped(reason)
        ),
        Activity::Unknown(ActivityGap::NoListing(error)) => format!(
            "the running programs couldn't be listed: {}",
            escaped(&error.to_string())
        ),
    }
}

fn modified(facts: &Facts, level: Level) -> String {
    let age = match facts.modified {
        Modified::Ago(age) => age,
        Modified::Unknown(gap) => return format!("unknown: {}", walk_gap(gap)),
    };
    let days = age.as_secs() / (24 * 60 * 60);
    let ago = match days {
        0 => "today".to_owned(),
        1 => "1 day ago".to_owned(),
        days => format!("{days} days ago"),
    };
    let window = facts.recent_window_days(level);
    if days < u64::from(window) {
        format!("{ago}, within the {window}-day recent window")
    } else {
        ago
    }
}

const fn walk_gap(gap: WalkGap) -> &'static str {
    match gap {
        WalkGap::Unread => "it couldn't be measured completely",
        WalkGap::OtherFileSystem => "a directory inside is on another file system and wasn't read",
    }
}
