//! Text for people: every word the CLI's human output and the home screen show for a
//! fact kipple established, so the two never word one fact apart. Frontends lay these
//! out, fit and colour them (`docs/05-architecture.md` §4).

use std::path::{MAIN_SEPARATOR, Path};

use kipple_core::{
    Activity, ActivityGap, Bucket, Caution, Cost, Facts, Finding, Hold, Lens, Level, Loss,
    Modified, Policy, Proposal, Recovery, ScanOutcome, Seen, Tracked, Verdict, WalkGap,
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
    match scaled(count) {
        Scaled { unit: None, .. } => format!("{count} B"),
        Scaled {
            whole,
            tenths,
            unit: Some(unit),
        } => format!("{whole}.{tenths} {unit}"),
    }
}

/// A byte count written short, for a narrow column: its unit's letter, and no decimal
/// from 100 on (`512M`, `2.5G`, `512B`).
pub(crate) fn bytes_short(count: u64) -> String {
    match scaled(count) {
        Scaled { unit: None, .. } => format!("{count}B"),
        Scaled {
            whole,
            tenths,
            unit: Some(unit),
        } => {
            let letter = unit.get(..1).unwrap_or(unit);
            if whole >= 100 {
                format!("{whole}{letter}")
            } else {
                format!("{whole}.{tenths}{letter}")
            }
        }
    }
}

/// A byte count in its binary unit, in whole units and tenths rounded down; no unit
/// below a KiB.
struct Scaled {
    whole: u64,
    tenths: u64,
    unit: Option<&'static str>,
}

fn scaled(count: u64) -> Scaled {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if count < 1024 {
        return Scaled {
            whole: count,
            tenths: 0,
            unit: None,
        };
    }
    let mut value = count;
    let mut unit = 0;
    while value >= 1024 * 1024 && unit + 1 < UNITS.len() {
        value /= 1024;
        unit += 1;
    }
    // Tenths of the unit, computed in integers.
    let tenths = value * 10 / 1024;
    Scaled {
        whole: tenths / 10,
        tenths: tenths % 10,
        unit: Some(UNITS.get(unit).copied().unwrap_or("PiB")),
    }
}

/// What removing an item frees: allocated storage where known, else apparent size. A
/// partial measurement is a lower bound.
pub(crate) fn size(size: Option<&SizeOut>) -> String {
    sized(size, bytes, ">= ")
}

/// [`size`] written short, as [`bytes_short`] writes bytes.
pub(crate) fn size_short(size: Option<&SizeOut>) -> String {
    sized(size, bytes_short, ">=")
}

fn sized(size: Option<&SizeOut>, write: fn(u64) -> String, bound: &str) -> String {
    let Some(size) = size else {
        return "unsized".to_owned();
    };
    let shown = write(size.on_disk());
    match size.completeness {
        CompletenessOut::Complete => shown,
        CompletenessOut::Incomplete | CompletenessOut::Cancelled => format!("{bound}{shown}"),
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
    standing(Some(bucket)).title
}

/// How a bucket is worded, or a finding not in one yet.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Standing {
    /// Its heading: `Safe to clean`.
    pub(crate) title: &'static str,
    /// Its name where room is short: `Safe`.
    pub(crate) name: &'static str,
    /// What puts a finding there, in a few words beside its heading.
    pub(crate) hint: &'static str,
    /// What the reasons a finding is there are labelled.
    pub(crate) why: &'static str,
    /// What being there means, in full.
    pub(crate) about: &'static str,
}

/// How `bucket` is worded, or with `None` a finding still being measured, in one table
/// so the heading, the band and the detail pane never drift apart.
pub(crate) const fn standing(bucket: Option<Bucket>) -> Standing {
    match bucket {
        Some(Bucket::Safe) => Standing {
            title: "Safe to clean",
            name: "Safe",
            hint: "passes every check",
            why: "Why safe",
            about: "Passes every check at this level: kipple recognised it, knows how it \
                    comes back, nothing inside is tracked by git, no running program shows \
                    it in use, and it hasn't changed recently.",
        },
        Some(Bucket::Review) => Standing {
            title: "Review",
            name: "Review",
            hint: "loses more, or recent",
            why: "Why review",
            about: "Eligible, but it loses more than this level allows, costs a lot to bring \
                    back, or changed recently. Each row says which.",
        },
        Some(Bucket::Protected) => Standing {
            title: "Protected",
            name: "Protected",
            hint: "not eligible",
            why: "Why it's held",
            about: "Not eligible: blocked, unknown, report only or advice only. Each row \
                    says why. kipple never selects these.",
        },
        None => Standing {
            title: "Measuring",
            name: "Measuring",
            hint: "not sorted yet",
            why: "Status",
            about: "Still being measured and checked. Nothing counts as Safe until kipple \
                    knows.",
        },
    }
}

/// A finding still being measured, as its row says it.
pub(crate) const MEASURING: &str = "measuring";

/// What a finished report calls a finding that was never measured, so it has no bucket.
pub(crate) const NOT_MEASURED: &str = "Not measured";

/// A finding's bucket as `kipple scan` lists it: the name reports use, or
/// `unmeasured`.
pub(crate) const fn bucket_name(bucket: Option<Bucket>) -> &'static str {
    match bucket {
        Some(bucket) => bucket.name(),
        None => "unmeasured",
    }
}

/// Where a finding stands at `level`: `Safe to clean at Balanced`, or that it is still
/// being measured.
pub(crate) fn placed(verdict: Option<&Verdict>, level: Level) -> String {
    verdict.map_or_else(
        || MEASURING.to_owned(),
        |verdict| format!("{} at {}", bucket(verdict.bucket()), self::level(level)),
    )
}

/// Why a finding stands where it does at `level`, in full: what Safe passed, every
/// reason it isn't Safe, or that it is still being measured.
pub(crate) fn why(verdict: Option<&Verdict>, level: Level) -> String {
    match verdict {
        None => "still being measured and checked".to_owned(),
        Some(Verdict::Safe) => format!(
            "passes every check at {}: recognised, comes back, untracked, no running program \
             shows it in use, not recent",
            self::level(level)
        ),
        Some(verdict) => reasons_in_full(verdict),
    }
}

/// Why a finding can't be selected.
pub(crate) fn not_selectable(verdict: Option<&Verdict>) -> String {
    match verdict {
        Some(verdict @ Verdict::Protected(_)) => {
            format!("Protected ({}): kipple never selects it.", reasons(verdict))
        }
        _ => "Still measuring: it can be selected once kipple knows its bucket.".to_owned(),
    }
}

/// A scan still running, as a title says it.
pub(crate) const SCANNING: &str = "scanning";

/// How a scan ended, as a title says it.
pub(crate) const fn outcome(outcome: ScanOutcome) -> &'static str {
    match outcome {
        ScanOutcome::Completed => "scan finished",
        ScanOutcome::Cancelled => "scan stopped early",
    }
}

/// What the scan couldn't cover, as a heading.
pub(crate) const NOT_COVERED: &str = "Not covered";

/// What anything not covered makes of the report.
pub(crate) const PARTIAL: &str = "so this report is partial";

/// A lens's name on screen.
pub(crate) const fn lens(lens: Lens) -> &'static str {
    match lens {
        Lens::Agents => "Agents",
        Lens::Dev => "Dev",
        Lens::System => "System",
    }
}

/// A level's name on screen.
pub(crate) const fn level(level: Level) -> &'static str {
    match level {
        Level::Careful => "Careful",
        Level::Balanced => "Balanced",
        Level::Thorough => "Thorough",
    }
}

/// What `level` puts in Safe (`docs/04-safety-model.md` §2a).
pub(crate) const fn level_about(level: Level) -> &'static str {
    match level {
        Level::Careful => "Safe: build output and regenerated caches; recent: 14 days",
        Level::Balanced => "Safe: rebuilt, reinstalled, re-downloaded; recent: 7 days",
        Level::Thorough => "Safe: Balanced plus what costs a rollback; recent: 2 days",
    }
}

/// Why a finding is Protected, as a row shows it: never colour alone. At most 22
/// columns, so it fits the state column with ` +N`.
pub(crate) const fn hold(hold: Hold) -> &'static str {
    hold_text(hold).0
}

/// Why a finding is Protected, as a sentence for the detail pane and `explain`.
pub(crate) const fn hold_sentence(hold: Hold) -> &'static str {
    hold_text(hold).1
}

/// Each hold's short and long text, in one table so the two never drift apart.
const fn hold_text(hold: Hold) -> (&'static str, &'static str) {
    match hold {
        Hold::Disabled => (
            "rule turned off",
            "Its rule is turned off in your settings.",
        ),
        Hold::Kept => ("kept", "You chose to keep it."),
        Hold::Excluded => (
            "excluded",
            "A path in your config's exclude list covers it.",
        ),
        Hold::ReportOnly => (
            "report only",
            "kipple only reports this kind of item; it never offers to remove it.",
        ),
        Hold::Advice => (
            "advice only",
            "Its own tool manages it; kipple shows how to trim it instead.",
        ),
        Hold::UnknownLayout => (
            "layout not recognised",
            "kipple doesn't recognise what is inside, so it can't tell what is safe to remove.",
        ),
        Hold::UnknownConsequence => (
            "unknown loss",
            "kipple can't tell what removing it would lose or how it would come back.",
        ),
        Hold::UnknownGit => (
            "git state unknown",
            "kipple couldn't check whether git tracks anything inside.",
        ),
        Hold::HoldsRepository => ("holds a git repo", "A git repository lives inside it."),
        Hold::InUse => ("in use", "A running program is using it."),
        Hold::UnknownActivity => (
            "can't tell if in use",
            "kipple couldn't check every running program, so it may be in use.",
        ),
        Hold::UnknownModified => (
            "last change unknown",
            "kipple couldn't read when it last changed.",
        ),
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

/// Every reason for `verdict`, each as a row shows it. Empty when Safe.
pub(crate) fn reason_labels(verdict: &Verdict) -> Vec<&'static str> {
    match verdict {
        Verdict::Safe => Vec::new(),
        Verdict::Review(cautions) => cautions.iter().map(|&c| caution(c)).collect(),
        Verdict::Protected(holds) => holds.iter().map(|&h| hold(h)).collect(),
    }
}

/// Every reason for `verdict`, joined for one line. Empty when Safe.
pub(crate) fn reasons(verdict: &Verdict) -> String {
    reason_labels(verdict).join(", ")
}

/// Every reason for `verdict` in full: a Protected finding's holds as sentences, a
/// Review finding's cautions as in [`reasons`]. Empty when Safe.
pub(crate) fn reasons_in_full(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Protected(holds) => holds
            .iter()
            .map(|&h| hold_sentence(h))
            .collect::<Vec<_>>()
            .join(" "),
        Verdict::Safe | Verdict::Review(_) => reasons(verdict),
    }
}

/// A reason as reports name it (`unknown-layout`), in the words a row shows.
pub(crate) fn reason(name: &'static str) -> &'static str {
    Hold::from_name(name)
        .map(hold)
        .or_else(|| Caution::from_name(name).map(caution))
        .unwrap_or(name)
}

/// An advice snippet's lines, each escaped for the terminal: a snippet may span lines,
/// and escaping it whole would show its line breaks as `\n`.
pub(crate) fn snippet_lines(snippet: &str) -> impl Iterator<Item = String> + '_ {
    snippet.lines().map(escaped)
}

/// `path` as the screen shows it: below the home directory, from `~`.
pub(crate) fn homed(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~{MAIN_SEPARATOR}{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// What kipple established about a finding, as labelled lines for a detail view: the
/// note it is kept with, what comes back and what is lost, then each check its bucket
/// rests on. Nothing until it is measured.
pub(crate) fn facts(finding: &Finding, policy: &Policy) -> Vec<(&'static str, String)> {
    let mut lines = Vec::new();
    let Some(facts) = &finding.facts else {
        return lines;
    };
    if let Some(kept) = policy.kept(&finding.path, &facts.lineage) {
        let note = kept.note.as_deref().unwrap_or("in the config's keep.paths");
        lines.push(("Kept", escaped(note)));
    }
    if let Some(consequence) = &facts.consequence {
        lines.push(("Comes back", recovery(consequence.recovery).to_owned()));
        lines.push(("You lose", losses(consequence.loss)));
        lines.push(("Restore cost", restore_cost(consequence.cost).to_owned()));
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
    let window = policy.recent_window_days(&finding.rule, facts);
    lines.push(("Last change", modified(facts, window)));
    lines
}

/// How expensive bringing it back is. An unknown cost is one kipple doesn't estimate;
/// it never changes the bucket.
const fn restore_cost(cost: Cost) -> &'static str {
    match cost {
        Cost::Unknown => "not estimated",
        Cost::Low | Cost::High => cost.name(),
    }
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

/// The D-050 notice, when any process was left out.
pub(crate) fn unchecked_system(count: usize) -> Option<String> {
    (count > 0).then(|| {
        format!(
            "{count} processes owned by root or SYSTEM, or running with root's rights, couldn't \
             be inspected and were not counted for items in your home."
        )
    })
}

/// The D-052 notice, when any of the user's own processes was judged by its command line.
pub(crate) fn by_command_line(count: usize) -> Option<String> {
    (count > 0).then(|| {
        format!(
            "{count} of your own processes couldn't be fully inspected and were checked by \
             their command line only for items in your home."
        )
    })
}

/// Whether a running program uses a finding, in words.
pub(crate) fn activity(activity: &Activity) -> String {
    match activity {
        Activity::Idle => "no running program shows it in use".to_owned(),
        Activity::IdleExceptSystem {
            unchecked,
            by_command_line,
        } => idle_except(*unchecked, *by_command_line),
        Activity::InUse { pid, seen } => in_use(*pid, seen),
        Activity::Unknown(ActivityGap::Uninspected(count)) => format!(
            "{count} running programs that count couldn't be inspected, so one of them might \
             use it"
        ),
        Activity::Unknown(ActivityGap::Partial(reason)) => format!(
            "the list of running programs may be incomplete ({})",
            escaped(reason)
        ),
        Activity::Unknown(ActivityGap::NoListing(error)) => format!(
            "the running programs couldn't be listed: {}",
            escaped(&error.to_string())
        ),
        Activity::Unknown(ActivityGap::Unsupported) => {
            "this OS doesn't show which paths running programs use, beyond their executable"
                .to_owned()
        }
    }
}

/// How process `pid` was seen using a finding: only the path that matched.
fn in_use(pid: u32, seen: &Seen) -> String {
    let (how, path) = match seen {
        Seen::Executable(path) => ("runs", path),
        Seen::WorkingDirectory(path) => ("works in", path),
        Seen::Argument(path) => ("was started with", path),
        Seen::Environment(path) => ("has in its environment", path),
        Seen::MappedFile(path) => ("has loaded", path),
        Seen::Runtime(path) => ("is its runtime and uses its project at", path),
    };
    format!(
        "process {pid} {how} {}",
        escaped(&path.display().to_string())
    )
}

fn modified(facts: &Facts, window: u16) -> String {
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
        WalkGap::Untimed => "an entry inside didn't say when it last changed",
    }
}

/// An idle finding in the home directory that some processes couldn't be fully checked
/// for, in words.
fn idle_except(unchecked: usize, by_command_line: usize) -> String {
    let system = (unchecked > 0).then(|| {
        format!(
            "{unchecked} owned by root or SYSTEM, or running with root's rights, couldn't be \
             inspected and weren't counted, since it is in your home"
        )
    });
    let own = (by_command_line > 0)
        .then(|| format!("{by_command_line} of your own were checked by their command line only"));
    std::iter::once("no running program kipple could inspect shows it in use".to_owned())
        .chain(system)
        .chain(own)
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests;
