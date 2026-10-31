//! The question a person answers before anything is removed (J3, spec M3b pinned 1, M3c
//! pinned 15, 16): the plan as it will be authorized, what it removes by each method,
//! what deleting and a trash move mean, and what was left out. Asked on stderr; the
//! answer is read from the input given, a terminal or, in tests, any reader, once what
//! was typed before the question is thrown away. Ctrl+C while it waits cancels the
//! command at once, and a plan above `review.confirm_by_typing_above` is confirmed only
//! by typing its size.

use std::io::{self, Write};

use kipple_core::{
    Answer, CancelToken, Confirm, Finding, Method, Plan, PlanEntry, Policy, RefusedItem, Review,
    ScanReport, Waited,
};

use super::output::{Line, Shown, Told, left_out};
use crate::Answers;
use crate::human::{receipt as words, review};
use crate::theme::{Ink, Role};

/// Asks on `out` and reads the answer from `input`.
pub(super) struct Prompt<'io> {
    pub(super) input: &'io mut dyn Answers,
    pub(super) out: &'io mut dyn Write,
    pub(super) ink: &'io Ink,
    /// The scan the plan was made from, for the paths of what planning refused.
    pub(super) scan: &'io ScanReport,
    /// The Review findings left to the TUI.
    pub(super) review: &'io [&'io Finding],
    pub(super) policy: &'io Policy,
    /// `review.confirm_by_typing_above`: above it, the size is typed instead of `y`.
    pub(super) above: Option<u64>,
    /// The command's: cancelled when Ctrl+C ends the wait for the answer.
    pub(super) cancel: &'io CancelToken,
    /// Whether the question was put.
    pub(super) told: Told,
}

impl Confirm for Prompt<'_> {
    fn confirm(&mut self, review: &Review<'_>) -> Answer {
        match self.ask(review) {
            Ok(true) => Answer::Yes,
            // A prompt that can't be shown or answered is a no.
            Ok(false) | Err(_) => Answer::No,
        }
    }
}

impl Prompt<'_> {
    /// Shows `review`, asks, and reads whether the answer confirms it.
    fn ask(&mut self, review: &Review<'_>) -> io::Result<bool> {
        let shown = Shown {
            review: self.review,
            policy: self.policy,
            told: Told::NotYet,
        };
        for line in plan_lines(review, self.scan, &shown) {
            writeln!(self.out, "{}", self.ink.paint(line.role, &line.text))?;
        }
        let items = review.plan.entries().len();
        let total = review.plan.estimated_total();
        let typed = self.above.is_some_and(|above| total.exceeds(above));
        let question = if typed {
            words::typed_question(items, &total)
        } else {
            words::question(items)
        };
        self.told = Told::InPrompt;
        let Some(answer) = answer(self.input, self.out, &question, self.cancel)? else {
            return Ok(false);
        };
        if !typed {
            return Ok(words::is_yes(&answer));
        }
        if words::is_typed_size(&answer, &total) {
            return Ok(true);
        }
        if !answer.trim().is_empty() {
            writeln!(self.out, "{}", words::NOT_THE_SIZE)?;
        }
        Ok(false)
    }
}

/// Puts `question` on `out` once what was typed before it is thrown away, then waits for
/// the answer and reads it.
///
/// `None` when `cancel` was cancelled meanwhile, while waiting or while reading the
/// answer, or the wait says so: `cancel` is then cancelled, so the command ends as
/// cancelled with nothing written, and the person is told so.
pub(super) fn answer(
    input: &mut dyn Answers,
    out: &mut dyn Write,
    question: &str,
    cancel: &CancelToken,
) -> io::Result<Option<String>> {
    // Only an answer typed once the question is shown counts.
    input.discard_typeahead()?;
    write!(out, "{question}")?;
    out.flush()?;
    let waited = input.wait(cancel)?;
    if matches!(waited, Waited::Cancelled) || cancel.is_cancelled() {
        writeln!(out)?;
        return cancelled(out, cancel);
    }
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    // Where the wait can't watch the input (a pipe, a terminal that isn't a console), the
    // read blocks instead, and Ctrl+C during it still cancels whatever was typed.
    if cancel.is_cancelled() {
        return cancelled(out, cancel);
    }
    Ok(Some(answer))
}

/// Ends a question as cancelled: `cancel` is cancelled and the person told so on `out`.
fn cancelled(out: &mut dyn Write, cancel: &CancelToken) -> io::Result<Option<String>> {
    cancel.cancel();
    if let Some(said) = review::waited(Waited::Cancelled) {
        writeln!(out, "{said}")?;
    }
    Ok(None)
}

/// The lines that show `review` before it is confirmed: what it removes by each method,
/// each item, what deleting and a trash move mean, and what was left out.
pub(super) fn plan_lines(review: &Review<'_>, scan: &ScanReport, shown: &Shown<'_>) -> Vec<Line> {
    let mut lines = item_lines(review.plan);
    let methods = review.plan.entries().iter().map(PlanEntry::method);
    lines.extend(method_lines(methods));
    let refused = RefusedItem::list(scan, review.refused);
    lines.extend(left_out(&refused, review.dropped, shown));
    lines
}

/// What `plan` removes by each method, then each of its items.
pub(super) fn item_lines(plan: &Plan) -> Vec<Line> {
    let headings = words::plan_heading(plan)
        .into_iter()
        .map(|heading| Line::new(Role::Title, heading));
    let items = plan.entries().iter().map(|entry| {
        let size = words::estimate(entry.estimate());
        Line::new(
            Role::Text,
            words::plan_line(entry.method(), &size, entry.path()),
        )
    });
    headings.chain(items).collect()
}

/// What deleting and a trash move mean, for each of `methods` used.
pub(super) fn method_lines(methods: impl Iterator<Item = Method>) -> Vec<Line> {
    let used: Vec<Method> = methods.collect();
    [
        (Method::Delete, words::DELETE_PERMANENT),
        (Method::Trash, words::TRASH_WHOLE),
    ]
    .into_iter()
    .filter(|(method, _)| used.contains(method))
    .map(|(_, text)| Line::new(Role::Warning, text.to_owned()))
    .collect()
}
