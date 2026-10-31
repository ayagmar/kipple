//! A clean while it runs (spec M3c pinned 7): what was found of earlier runs, which item
//! of how many is going and its path, and how each ended item ended.

use kipple_core::{ItemOutcome, Status};
use ratatui::text::Line as Text;

use std::path::Path;

use super::{Look, homed};
use crate::human::{receipt, review};
use crate::theme::Role;
use crate::tui::app::{Applying, ItemEnded};

/// The progress's title: the item under way and how many have ended, or the reconciling
/// of earlier runs before this one starts.
pub(super) fn title(applying: &Applying) -> String {
    let items = applying.review.prepared.plan().entries().len();
    if applying.run.is_none() {
        return review::RECONCILING.to_owned();
    }
    let under_way = applying.current.map(|item| item.0);
    review::cleaning(under_way, applying.ended.len(), items)
}

/// The progress's lines, each in its role.
pub(super) fn lines(
    applying: &Applying,
    home: Option<&Path>,
    look: Look<'_>,
) -> Vec<Text<'static>> {
    let plan = applying.review.prepared.plan();
    let mut lines: Vec<Text<'static>> = applying
        .notices
        .iter()
        .map(|notice| Text::styled(notice.clone(), look.style(Role::Muted)))
        .collect();
    for ItemEnded { item, outcome } in &applying.ended {
        if let Some(entry) = plan.entry(*item) {
            let path = homed(entry.path(), home);
            let line = receipt::item_line(item.0, outcome, entry.method(), &path);
            lines.push(Text::styled(line, look.style(role_of(outcome))));
        }
    }
    if let Some(entry) = applying.current.and_then(|item| plan.entry(item)) {
        let now = review::working_on(entry.method(), &homed(entry.path(), home));
        lines.push(Text::styled(now, look.style(Role::Accent)));
    }
    lines
}

/// How an item's line is painted: what went wrong stands out.
pub(super) const fn role_of(outcome: &ItemOutcome) -> Role {
    match outcome.status() {
        Status::Done => Role::Text,
        Status::Skipped | Status::Cancelled => Role::Muted,
        Status::Partial | Status::Indeterminate => Role::Warning,
        Status::Failed => Role::Error,
    }
}
