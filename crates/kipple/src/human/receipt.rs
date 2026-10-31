//! The sentences the mutating commands print (`kipple clean`, `plan`, `apply`, `history`
//! and `restore`): the review a person confirms, each item's outcome, the totals in the
//! wording of 04 §8, and what these commands refuse. The words for each value come from
//! [`super::outcome`].

use std::path::Path;

use kipple_core::{
    Completeness, Ending, ItemOutcome, Left, Method, Plan, RunState, SpaceEstimate, Status, Total,
    Totals,
};

use super::outcome::{error_text, found, left_reason, skip, status};
use super::{bytes, escaped};

/// Why `clean --yes` without `--only` runs nothing (J6).
pub(crate) const YES_NEEDS_ONLY: &str = "--yes cleans only what you name: \
     `kipple clean --only RULE --yes` for some rules, or `kipple plan -o FILE` then \
     `kipple apply FILE --yes` for exactly the items of a plan. Nothing was changed.";

/// Why a mutating command without a terminal and without `--yes` runs nothing.
pub(crate) const NO_TERMINAL: &str = "nothing was changed: confirming needs a terminal; \
     without one, name the rules with --only and pass --yes";

/// What a trash move takes, said before a person confirms one.
pub(crate) const TRASH_WHOLE: &str = "A trash move takes each folder whole, with anything \
     added to it since this scan; it stays on disk until the trash is emptied.";

/// What deleting means, said before a person confirms a delete.
pub(crate) const DELETE_PERMANENT: &str = "Deleting is permanent: these can't be restored.";

/// The heading of the Review findings a command line clean leaves out.
pub(crate) const NEEDS_TUI: &str = "Needs review in the TUI (`kipple`), not cleaned here:";

/// The heading of the selected findings planning refused.
pub(crate) const NOT_PLANNED: &str = "Not planned:";

/// The heading of a plan file's items left out of the fresh plan.
pub(crate) const DROPPED: &str = "Left out of the plan file's items:";

/// What `history` says when there are no runs.
pub(crate) const NO_RUNS: &str = "No runs yet. `kipple clean` records each one here.";

/// What `restore` says on Windows (D-062).
pub(crate) const NO_TRASH_HERE: &str =
    "trash isn't supported on Windows, so no run moved anything there to restore";

/// The question a person answers to run `items` items.
pub(crate) fn question(items: usize) -> String {
    format!("Clean {}? [y/N] ", items_word(items))
}

/// Whether `answer`, a line a person typed, says yes: `y` or `yes`, in any case.
pub(crate) fn is_yes(answer: &str) -> bool {
    let answer = answer.trim();
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

/// The size a person types to confirm removing `total`, above
/// `review.confirm_by_typing_above` (spec M3c pinned 16): as [`bytes`] writes it, and
/// "at least" before it when part of it wasn't measured.
pub(crate) fn typed_size(total: &Total) -> String {
    match total.complete {
        Completeness::Complete => bytes(total.known),
        Completeness::Incomplete { .. } | Completeness::Cancelled => {
            format!("at least {}", bytes(total.known))
        }
    }
}

/// Whether `typed`, a line a person typed, is `total`'s [`typed_size`], with case and
/// spaces ignored.
pub(crate) fn is_typed_size(typed: &str, total: &Total) -> bool {
    let squeezed = |text: &str| -> String {
        text.chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect()
    };
    squeezed(typed) == squeezed(&typed_size(total))
}

/// The question a person answers to run `items` items freeing `total`, above the
/// threshold: the size, typed.
pub(crate) fn typed_question(items: usize, total: &Total) -> String {
    format!(
        "Clean {}? Type {} to confirm, anything else declines: ",
        items_word(items),
        typed_size(total)
    )
}

/// What a typed size that isn't the one asked for means.
pub(crate) const NOT_THE_SIZE: &str = "That isn't the size shown, so nothing was changed.";

/// `review.confirm_by_typing_above`, as `kipple config show` shows it.
pub(crate) fn threshold(above: u64) -> String {
    format!("{above} bytes ({})", bytes(above))
}

/// The plan's heading: what it removes, by method (J3).
pub(crate) fn plan_heading(plan: &Plan) -> Vec<String> {
    [Method::Delete, Method::Trash]
        .into_iter()
        .filter_map(|method| {
            let entries: Vec<_> = plan
                .entries()
                .iter()
                .filter(|entry| entry.method() == method)
                .collect();
            if entries.is_empty() {
                return None;
            }
            let size: u64 = entries
                .iter()
                .filter_map(|entry| entry.estimate())
                .map(SpaceEstimate::reclaim)
                .sum();
            Some(format!(
                "{} {} in {}",
                method_verb(method),
                bytes(size),
                items_word(entries.len())
            ))
        })
        .collect()
}

/// A plan item's size, as its line shows it.
pub(crate) fn estimate(estimate: Option<&SpaceEstimate>) -> String {
    estimate.map_or_else(|| "unsized".to_owned(), |e| bytes(on_disk(Some(e))))
}

/// What an estimate takes on disk: allocated storage where known, else its length.
fn on_disk(estimate: Option<&SpaceEstimate>) -> u64 {
    estimate.map_or(0, |e| e.allocated.map_or(e.apparent.0, |a| a.0))
}

/// How a method is named in a plan's heading.
const fn method_verb(method: Method) -> &'static str {
    match method {
        Method::Delete => "delete",
        Method::Trash => "move to the trash",
    }
}

/// A plan item's method, in its line.
pub(crate) const fn method_word(method: Method) -> &'static str {
    match method {
        Method::Delete => "delete",
        Method::Trash => "trash",
    }
}

/// `count` items, in words.
pub(crate) fn items_word(count: usize) -> String {
    if count == 1 {
        "1 item".to_owned()
    } else {
        format!("{count} items")
    }
}

/// Why an item ended as it did, after its line, when there is more to say than its
/// status: what it left behind, why it was skipped or failed, what was found.
pub(crate) fn outcome_detail(outcome: &ItemOutcome) -> Option<String> {
    let detail = match outcome {
        ItemOutcome::Done(effect)
        | ItemOutcome::Partial(effect)
        | ItemOutcome::Cancelled(effect) => left_counts(&effect.left),
        ItemOutcome::Skipped(why) => skip(why),
        ItemOutcome::Failed { error, .. } => error_text(error),
        ItemOutcome::Indeterminate(seen) => found(seen),
    };
    (!detail.is_empty()).then_some(detail)
}

/// What was left, by reason: `2 in use, 1 changed since review`.
fn left_counts(left: &Left) -> String {
    left.0
        .iter()
        .filter(|(_, counted)| counted.count > 0)
        .map(|(reason, counted)| format!("{} {}", counted.count, left_reason(*reason)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a run did, in the words of 04 §8: what was removed, what was moved to the trash,
/// and how many items ended otherwise.
pub(crate) fn totals(totals: &Totals) -> String {
    let mut parts = Vec::new();
    let removed = &totals.removed;
    if removed.names > 0 {
        parts.push(format!(
            "Removed an estimated {} of allocated storage ({}).",
            bytes(removed.estimated.0),
            names_word(removed.names)
        ));
    }
    let trashed = &totals.trashed;
    if trashed.items > 0 {
        let size = trashed.allocated.map_or(trashed.apparent.0, |a| a.0);
        parts.push(format!(
            "Moved {} in {} to the trash, still on disk.",
            bytes(size),
            items_word(usize::try_from(trashed.items).unwrap_or(usize::MAX))
        ));
    }
    let others: Vec<String> = totals
        .items
        .iter()
        .filter(|(status, count)| **status != Status::Done && **count > 0)
        .map(|(ended, count)| {
            let count = usize::try_from(*count).unwrap_or(usize::MAX);
            format!("{} {}", items_word(count), status(*ended))
        })
        .collect();
    if !others.is_empty() {
        parts.push(format!("{}.", others.join(", ")));
    }
    parts.join(" ")
}

/// `count` names, in words.
fn names_word(count: u64) -> String {
    if count == 1 {
        "1 name".to_owned()
    } else {
        format!("{count} names")
    }
}

/// How a command ended, as its last line says it.
pub(crate) fn ending_line(ending: &Ending) -> String {
    let words = super::outcome::ending(ending);
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |first| {
        format!("{}{}.", first.to_uppercase(), chars.as_str())
    })
}

/// A run's line in `history`: its ID, where it stands and its items.
pub(crate) fn run_line(id: &str, state: RunState, items: usize) -> String {
    format!(
        "{id}  {}, {}",
        super::outcome::run_state(state),
        items_word(items)
    )
}

/// An earlier run a mutating command couldn't read.
pub(crate) fn unreadable_run(id: &str, why: &str) -> String {
    format!("warning: run {id} can't be read: {why}; nothing in it counts as removed")
}

/// What reconciling an item of an earlier run found.
pub(crate) fn reconciled(id: &str, item: u32, outcome: &ItemOutcome) -> String {
    let word = status(outcome.status());
    with_detail(
        format!("run {id} item {item} was interrupted: {word}"),
        outcome,
    )
}

/// What reconciling a restore of an earlier run found.
pub(crate) fn reconciled_restore(id: &str, item: u32, result: &str) -> String {
    format!("run {id} item {item}'s restore was interrupted: {result}")
}

/// A plan of `items` items written to the file `to`, or to stdout.
pub(crate) fn plan_written(to: Option<&Path>, items: usize) -> String {
    let to = to.map_or_else(
        || "stdout".to_owned(),
        |file| escaped(&file.display().to_string()),
    );
    format!(
        "Wrote a plan of {} to {to}. `kipple apply` plans it again before anything runs.",
        items_word(items)
    )
}

/// Why `restore` didn't take a run's ID.
pub(crate) fn not_a_run(text: &str) -> String {
    format!("{} isn't a run; `kipple history` lists them", escaped(text))
}

/// Why `apply` couldn't read a plan file.
pub(crate) fn not_a_plan(file: &Path, why: &str) -> String {
    format!(
        "{} can't be applied: {why}",
        escaped(&file.display().to_string())
    )
}

/// Why `plan -o` couldn't write the plan file.
pub(crate) fn plan_not_written(file: &Path, why: &str) -> String {
    format!(
        "the plan couldn't be written to {}: {why}",
        escaped(&file.display().to_string())
    )
}

/// Why `--only` was refused.
pub(crate) fn no_such_rule(rule: &str) -> String {
    format!(
        "there is no rule {}; `kipple rules` lists them",
        escaped(rule)
    )
}

/// What the interrupt handler couldn't do.
pub(crate) fn no_interrupt(why: &str) -> String {
    format!("warning: Ctrl+C will end kipple at once, not after the current item: {why}")
}

/// A run's heading above its items.
pub(crate) fn run_heading(id: &str) -> String {
    format!("Run {id}")
}

/// An item as a receipt or `history` lists it: its number, status, method and path,
/// then why it ended so, if there is more to say.
pub(crate) fn item_line(item: u32, outcome: &ItemOutcome, method: Method, path: &Path) -> String {
    let line = format!(
        "  {item:>3}  {:<11}  {:<6}  {}",
        status(outcome.status()),
        method_word(method),
        escaped(&path.display().to_string())
    );
    with_detail(line, outcome)
}

/// `line`, then why `outcome` ended so, if there is more to say.
fn with_detail(line: String, outcome: &ItemOutcome) -> String {
    match outcome_detail(outcome) {
        Some(detail) => format!("{line}: {detail}"),
        None => line,
    }
}

/// An item's last restore, under its line.
pub(crate) fn restored_line(result: &str) -> String {
    format!("       restore: {result}")
}

/// A finding or plan item that wasn't planned, and why.
pub(crate) fn left_line(path: &Path, why: &str) -> String {
    let path = escaped(&path.display().to_string());
    if why.is_empty() {
        format!("  {path}")
    } else {
        format!("  {path}: {why}")
    }
}

/// A plan item as the person confirming it sees it.
pub(crate) fn plan_line(method: Method, size: &str, path: &Path) -> String {
    format!(
        "  {:<6}  {size:>10}  {}",
        method_word(method),
        escaped(&path.display().to_string())
    )
}

/// What `restore` did with one item.
pub(crate) fn restore_line(item: u32, path: Option<&Path>, what: &str) -> String {
    path.map_or_else(
        || format!("  {item:>3}  {what}"),
        |path| {
            format!(
                "  {item:>3}  {}  {what}",
                escaped(&path.display().to_string())
            )
        },
    )
}
