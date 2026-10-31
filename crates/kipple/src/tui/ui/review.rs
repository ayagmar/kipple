//! The review of a clean (`docs/03-functional-spec.md` §2 J3, spec M3c pinned 5): above
//! the list, however far it is scrolled, what deleting and a trash move mean and how
//! many items need review or weren't planned; in it, the plan as it will run, by method,
//! each item with what it frees, a Review item's cautions under it, then what was left
//! out of the plan and why. It is read-only: what it shows is exactly what `y` runs.

use kipple_core::{Method, Standing};
use ratatui::text::Line as Text;

use super::{Look, homed};
use crate::human::{outcome, receipt, review};
use crate::theme::Role;
use crate::tui::app::{App, Review};

/// The review's title.
pub(super) fn title(review: &Review) -> String {
    review::review_title(review.prepared.plan().entries().len())
}

/// What stays above the review's list, so `y` never confirms what wasn't drawn:
/// `title`, why nothing can be cleaned when nothing can, what deleting and a trash move
/// mean, and how many items need review and how many weren't planned.
pub(super) fn head(title: String, review: &Review, look: Look<'_>) -> Vec<Text<'static>> {
    let line = |role, text: String| Text::styled(text, look.style(role));
    let prepared = &review.prepared;
    let entries = prepared.plan().entries();
    let mut head = vec![line(Role::Title, title)];
    if prepared.ending().is_some() {
        head.push(line(Role::Warning, review::NOTHING_TO_CLEAN.to_owned()));
    }
    let has = |method| entries.iter().any(|entry| entry.method() == method);
    if has(Method::Delete) {
        head.push(line(Role::Warning, receipt::DELETE_PERMANENT.to_owned()));
    }
    if has(Method::Trash) {
        head.push(line(Role::Warning, receipt::TRASH_WHOLE.to_owned()));
    }
    let cautioned = entries
        .iter()
        .filter(|entry| matches!(entry.standing(), Standing::Review(_)))
        .count();
    if let Some(below) = review::below(cautioned, prepared.refused_items().len()) {
        head.push(line(Role::Review, below));
    }
    head
}

/// The review's list, each line in its role.
pub(super) fn lines(app: &App, review: &Review, look: Look<'_>) -> Vec<Text<'static>> {
    let line = |role, text: String| Text::styled(text, look.style(role));
    let home = app.home.as_deref();
    let prepared = &review.prepared;
    let plan = prepared.plan();
    let mut lines = Vec::new();
    lines.extend(
        receipt::plan_heading(plan)
            .into_iter()
            .map(|heading| line(Role::Title, heading)),
    );
    for entry in plan.entries() {
        let size = receipt::estimate(entry.estimate());
        let text = receipt::plan_line(entry.method(), &size, &homed(entry.path(), home));
        lines.push(line(Role::Text, text));
        if let Some(cautions) = review::cautions(entry.standing()) {
            lines.push(line(Role::Review, cautions));
        }
    }
    let refused: Vec<String> = prepared
        .refused_items()
        .iter()
        .map(|item| receipt::left_line(&homed(&item.path, home), &outcome::skip(&item.skip)))
        .collect();
    section(&mut lines, look, receipt::NOT_PLANNED, refused);
    let merged: Vec<String> = prepared
        .merged()
        .iter()
        .filter_map(|merged| {
            let path = &app.row(merged.finding)?.finding.path;
            let into = plan.entry(merged.into)?.path();
            let into = homed(into, home);
            Some(receipt::left_line(
                &homed(path, home),
                &review::merged(&into),
            ))
        })
        .collect();
    section(&mut lines, look, review::MERGED, merged);
    lines
}

/// `items` under `heading`, when there are any.
fn section(lines: &mut Vec<Text<'static>>, look: Look<'_>, heading: &str, items: Vec<String>) {
    if items.is_empty() {
        return;
    }
    lines.push(Text::styled(heading.to_owned(), look.style(Role::Title)));
    lines.extend(
        items
            .into_iter()
            .map(|text| Text::styled(text, look.style(Role::Text))),
    );
}
