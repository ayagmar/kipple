//! The question a person answers before anything is removed (J3, spec M3b pinned 1): the
//! plan as it will be authorized, what it removes by each method, what deleting and a
//! trash move mean, and what was left out. Asked on stderr; the answer is read from the
//! input given, a terminal or, in tests, any reader, once what was typed before the
//! question is thrown away.

use std::io::{self, Write};

use kipple_core::{Answer, Confirm, Finding, Method, Policy, RefusedItem, Review, ScanReport};

use super::output::{Shown, Told, left_out};
use crate::Answers;
use crate::human::receipt as words;
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
    /// Shows `review`, asks, and reads whether the answer is yes.
    fn ask(&mut self, review: &Review<'_>) -> io::Result<bool> {
        let ink = self.ink;
        let entries = review.plan.entries();
        for heading in words::plan_heading(review.plan) {
            writeln!(self.out, "{}", ink.paint(Role::Title, &heading))?;
        }
        for entry in entries {
            let size = words::estimate(entry.estimate());
            let line = words::plan_line(entry.method(), &size, entry.path());
            writeln!(self.out, "{}", ink.paint(Role::Text, &line))?;
        }
        let has = |method| entries.iter().any(|entry| entry.method() == method);
        if has(Method::Delete) {
            writeln!(
                self.out,
                "{}",
                ink.paint(Role::Warning, words::DELETE_PERMANENT)
            )?;
        }
        if has(Method::Trash) {
            writeln!(self.out, "{}", ink.paint(Role::Warning, words::TRASH_WHOLE))?;
        }
        let refused = RefusedItem::list(self.scan, review.refused);
        let shown = Shown {
            review: self.review,
            policy: self.policy,
            told: Told::NotYet,
        };
        for line in left_out(&refused, review.dropped, &shown) {
            writeln!(self.out, "{}", ink.paint(line.role, &line.text))?;
        }
        // Only an answer typed once the plan and the question are shown authorizes it.
        self.input.discard_typeahead()?;
        write!(self.out, "{}", words::question(entries.len()))?;
        self.told = Told::InPrompt;
        self.out.flush()?;
        let mut answer = String::new();
        self.input.read_line(&mut answer)?;
        Ok(words::is_yes(&answer))
    }
}
