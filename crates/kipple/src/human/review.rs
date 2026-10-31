//! The words of a clean a person confirms and watches (spec M3c pinned 4–9, 15): the
//! TUI's review, its progress while apply runs and its result, and what the prompt says
//! when a question is cancelled. The review's plan and the result's items and totals are
//! worded by [`super::receipt`], as the command line words them.

use std::path::Path;

use kipple_core::{ApplyEvent, Method, Reconciled, Standing, Waited};

use super::receipt::{self, items_word};
use super::{caution, escaped, outcome};

/// What a wait for the person's answer ended in, when it says anything.
pub(crate) const fn waited(waited: Waited) -> Option<&'static str> {
    match waited {
        Waited::Ready => None,
        Waited::Cancelled => Some("Cancelled at the question, so nothing was changed."),
    }
}

/// What `enter` or `c` says while the scan runs (pinned 4).
pub(crate) const REVIEW_AFTER_SCAN: &str = "review opens when the scan has finished";

/// What the review says while its plan is being made (pinned 4).
pub(crate) const PLANNING: &str = "planning: freezing each item as it is now…";

/// What the key bar says while the plan is made: `back` stops it.
pub(crate) fn stop_planning(back: &str) -> String {
    format!("{back} stops planning")
}

/// What the key bar says before the size being typed.
pub(crate) const SIZE_PROMPT: &str = "Size: ";

/// What the key bar says after the size being typed: `confirm` confirms it, `back` goes
/// back to the review.
pub(crate) fn size_keys(confirm: &str, back: &str) -> String {
    format!("  {confirm} confirms, {back} goes back")
}

/// What help says last: selecting removes nothing, and `confirm` on the review cleans.
pub(crate) fn selecting_removes_nothing(confirm: &str) -> String {
    format!("Selecting removes nothing: only {confirm} on the review cleans.")
}

/// The review's title over a plan of `items` items.
pub(crate) fn review_title(items: usize) -> String {
    format!("Review: clean {}", items_word(items))
}

/// Why confirming refused: the config no longer loads, worded by its error (pinned 6).
pub(crate) fn config_changed(why: &str) -> String {
    format!("nothing was changed: the config no longer loads: {why}")
}

/// Progress: the item being removed, of how many (pinned 7).
pub(crate) fn item_of(item: usize, items: usize) -> String {
    format!("item {item} of {items}")
}

/// What progress says before the run starts, while earlier runs are reconciled.
pub(crate) const RECONCILING: &str = "finishing what an earlier run left";

/// What `q` says while apply runs (pinned 7).
pub(crate) const STOP_KEY: &str = "ctrl+c stops after this item";

/// What the first Ctrl+C during apply says (pinned 8).
pub(crate) const STOPPING: &str = "stopping after the current item";

/// What the second Ctrl+C during apply leaves on stderr, for the run `run` (pinned 8).
pub(crate) fn interrupted_run(run: &str) -> String {
    format!("run {run} was interrupted; the next kipple command that changes anything finishes it")
}

/// What the second Ctrl+C leaves on stderr when it came before the run started.
pub(crate) const INTERRUPTED_RECONCILING: &str =
    "an earlier run's reconciliation was interrupted; the next command finishes it";

/// The result when the worker that applied failed (pinned 9).
pub(crate) const WORKER_FAILED: &str =
    "The run failed: the run is reconciled by the next kipple that changes anything.";

/// How to put back what a run `run` moved to the trash.
pub(crate) fn restore_hint(run: &str) -> String {
    format!("`kipple restore {run}` moves its trashed items back.")
}

/// What `esc` while the plan is made says.
pub(crate) const PLANNING_STOPPED: &str = "Planning stopped, so nothing was changed.";

/// What a planning worker that failed leaves on the tree.
pub(crate) const PLANNING_FAILED: &str = "Planning failed, so nothing was changed.";

/// What the review says above what was left out when nothing selected can be cleaned.
pub(crate) const NOTHING_TO_CLEAN: &str = "Nothing selected can be cleaned, for the reasons below.";

/// What the review's head says of the list under it, when it holds anything a person
/// should read before confirming: how many items need review, with their cautions under
/// them, and how many selected findings weren't planned.
pub(crate) fn below(cautioned: usize, refused: usize) -> Option<String> {
    let mut said = Vec::new();
    if cautioned > 0 {
        let verb = if cautioned == 1 { "needs" } else { "need" };
        said.push(format!("{} {verb} review", items_word(cautioned)));
    }
    if refused > 0 {
        said.push(format!("{} not planned", items_word(refused)));
    }
    (!said.is_empty()).then(|| format!("{}: see below.", said.join(", ")))
}

/// The heading of the selected findings that are part of another item.
pub(crate) const MERGED: &str = "Part of another item:";

/// Why a selected finding is part of another item: the item's path.
pub(crate) fn merged(into: &Path) -> String {
    format!("part of {}", escaped(&into.display().to_string()))
}

/// What a Review item needs reviewed, under its line in the review: its cautions.
pub(crate) fn cautions(standing: &Standing) -> Option<String> {
    match standing {
        Standing::Safe => None,
        Standing::Review(cautions) => {
            let words: Vec<&str> = cautions.iter().map(|&c| caution(c)).collect();
            Some(format!("{:22}needs review: {}", "", words.join(", ")))
        }
    }
}

/// Why confirming refused: the run store can't exist, or the machine can't be made,
/// worded by `why` (pinned 6).
pub(crate) fn not_run(why: &str) -> String {
    format!("nothing was changed: {why}")
}

/// What the review says when the config now asks for the size to be typed (pinned 16).
pub(crate) const TYPE_THE_SIZE: &str =
    "The config now asks for the size to be typed: type it, then press enter.";

/// The progress heading while the run's items go.
pub(crate) const CLEANING: &str = "Cleaning";

/// The item being removed now, and how.
pub(crate) fn working_on(method: Method, path: &Path) -> String {
    let path = escaped(&path.display().to_string());
    match method {
        Method::Delete => format!("deleting {path}"),
        Method::Trash => format!("moving {path} to the trash"),
    }
}

/// The result's heading when no run was stored.
pub(crate) const RESULT: &str = "Result";

/// What an event of a running clean leaves to say once it is over: an earlier run
/// reconciled, or one that can't be read.
pub(crate) fn notice(event: &ApplyEvent) -> Option<String> {
    match event {
        ApplyEvent::Reconciled {
            run,
            reconciled: Reconciled::Item { item, outcome },
        } => Some(receipt::reconciled(run.as_str(), item.0, outcome)),
        ApplyEvent::Reconciled {
            run,
            reconciled: Reconciled::Restore { item, result },
        } => Some(receipt::reconciled_restore(
            run.as_str(),
            item.0,
            &outcome::restore_result(result),
        )),
        ApplyEvent::Unreadable { run, why } => Some(receipt::unreadable_run(
            run.as_str(),
            outcome::unreadable(why),
        )),
        ApplyEvent::RunStarted { .. }
        | ApplyEvent::Item { .. }
        | ApplyEvent::RestoreStarted { .. }
        | ApplyEvent::RestoreEnded { .. } => None,
    }
}
