//! A clean's result (spec M3c pinned 9): what happened to earlier runs, each item of the
//! run and how it ended, the totals in the wording of 04 §8, how the clean ended, and
//! how to put back what went to the trash. A worker that failed reads as failed.

use kipple_core::{Ending, Finished};
use ratatui::text::Line as Text;

use std::path::Path;

use super::progress::role_of;
use super::{Look, homed};
use crate::human::{outcome, receipt, review};
use crate::theme::Role;
use crate::tui::app::Ended;

/// The result's title: the run's, when one was stored.
pub(super) fn title(ended: &Ended) -> String {
    match ended {
        Ended::Ran { finished, .. } => finished.run.as_ref().map_or_else(
            || review::RESULT.to_owned(),
            |run| receipt::run_heading(run.as_str()),
        ),
        Ended::Failed { .. } => review::RESULT.to_owned(),
    }
}

/// The result's lines, each in its role.
pub(super) fn lines(ended: &Ended, home: Option<&Path>, look: Look<'_>) -> Vec<Text<'static>> {
    let line = |role, text: String| Text::styled(text, look.style(role));
    let (notices, finished) = match ended {
        Ended::Ran {
            finished, notices, ..
        } => (notices, Some(finished)),
        Ended::Failed { notices } => (notices, None),
    };
    let mut lines: Vec<Text<'static>> = notices
        .iter()
        .map(|notice| line(Role::Muted, notice.clone()))
        .collect();
    let Some(finished) = finished else {
        lines.push(line(Role::Error, review::WORKER_FAILED.to_owned()));
        return lines;
    };
    receipt_lines(finished, home, look, &mut lines);
    lines
}

/// What `finished` did, as the command line's receipt says it.
fn receipt_lines(
    finished: &Finished,
    home: Option<&Path>,
    look: Look<'_>,
    lines: &mut Vec<Text<'static>>,
) {
    let line = |role, text: String| Text::styled(text, look.style(role));
    for item in &finished.receipt.items {
        let path = homed(&item.path, home);
        let text = receipt::item_line(item.item.0, &item.outcome, item.method, &path);
        lines.push(line(role_of(&item.outcome), text));
        if let Some(result) = &item.restored {
            let restored = receipt::restored_line(&outcome::restore_result(result));
            lines.push(line(Role::Muted, restored));
        }
    }
    let totals = finished.receipt.totals();
    let said = receipt::totals(&totals);
    if !said.is_empty() {
        lines.push(line(Role::Text, said));
    }
    // A store that refused is said in its own words, as the command line says it.
    let ending = if let Ending::Refused(why) = &finished.ending {
        outcome::store_refusal(why)
    } else {
        receipt::ending_line(&finished.ending)
    };
    let role = if finished.ending.code() == 0 {
        Role::Text
    } else {
        Role::Warning
    };
    lines.push(line(role, ending));
    if let Some(run) = &finished.run
        && totals.trashed.items > 0
    {
        lines.push(line(Role::Muted, review::restore_hint(run.as_str())));
    }
}
