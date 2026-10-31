//! The words of a clean a person confirms and watches (spec M3c pinned 4–9, 15): the
//! TUI's review, its progress while apply runs and its result, and what the prompt says
//! when a question is cancelled. The review's plan and the result's items and totals are
//! worded by [`super::receipt`], as the command line words them.

use kipple_core::Waited;

use super::receipt::items_word;

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

/// What `enter` or `esc` on the result does.
pub(crate) const BACK_HOME: &str = "enter or esc: back home, and scan again";
