//! A clean from the home screen (`docs/03-functional-spec.md` §2 J3, spec M3c pinned
//! 4–9 and 16): `enter` plans the selection on a worker once the scan has finished, the
//! review shows that plan and nothing else, `y` (or the size, typed, above
//! `review.confirm_by_typing_above`) runs that same plan, progress follows its events,
//! and the result shows its receipt. The plan is never edited here: `esc` goes back to
//! the tree with the selection as it was, and `enter` plans again.

use std::sync::Arc;

use kipple_core::{
    ApplyEvent, Finished, ItemEvent, ItemId, ItemOutcome, Prepared, RunId, Selection, Total,
};

use super::{App, Effect, Key, Progress};
use crate::human::{receipt, review};
use crate::tui::keymap::{self, Action};

/// Which request for a plan an answer is to: only the latest one opens the review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Generation(pub(crate) u64);

/// How the person confirmed: with the confirm key, or by typing the size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Confirmation {
    Key,
    Size,
}

/// Where a clean from the home screen stands.
#[derive(Debug)]
pub(crate) enum Cleaning {
    /// The worker is planning the selection, for this request.
    Planning(Generation),
    /// The plan is shown, to confirm or leave.
    Reviewing(Review),
    /// Its size is being typed to confirm it.
    Typing(Review, String),
    /// It runs on a worker.
    Applying(Applying),
    /// It ended.
    Ended(Ended),
}

/// A plan shown for review: exactly what runs when it is confirmed.
#[derive(Debug)]
pub(crate) struct Review {
    pub(crate) prepared: Arc<Prepared>,
    /// What it frees, as its heading says it.
    pub(crate) total: Total,
    /// `review.confirm_by_typing_above` when it was planned.
    threshold: Option<u64>,
    /// The first line shown.
    pub(crate) scroll: usize,
}

impl Review {
    /// Whether confirming it takes the size, typed.
    pub(crate) fn needs_typing(&self) -> bool {
        needs_typing(self.threshold, &self.total)
    }

    /// Whether it has anything to run.
    fn runs(&self) -> bool {
        self.prepared.ending().is_none()
    }
}

/// Whether a plan freeing `total` needs its size typed under `threshold`: when it is
/// above it, or may be.
pub(crate) fn needs_typing(threshold: Option<u64>, total: &Total) -> bool {
    threshold.is_some_and(|above| total.exceeds(above))
}

/// Whether the first Ctrl+C has asked the run to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stopping {
    No,
    Asked,
}

/// A clean that runs: its plan, and what its events have said so far.
#[derive(Debug)]
pub(crate) struct Applying {
    pub(crate) review: Review,
    /// The run, once it is stored.
    pub(crate) run: Option<RunId>,
    /// The item that started last and hasn't ended.
    pub(crate) current: Option<ItemId>,
    /// The items that ended, in order.
    pub(crate) ended: Vec<ItemEnded>,
    /// What happened to earlier runs, in words.
    pub(crate) notices: Vec<String>,
    pub(crate) stopping: Stopping,
}

/// An item of a run that ended, and how.
#[derive(Debug)]
pub(crate) struct ItemEnded {
    pub(crate) item: ItemId,
    pub(crate) outcome: ItemOutcome,
}

/// How a clean from the home screen ended.
#[derive(Debug)]
pub(crate) enum Ended {
    /// It finished as `finished` says.
    Ran {
        finished: Box<Finished>,
        notices: Vec<String>,
        scroll: usize,
    },
    /// The worker that applied it failed.
    Failed { notices: Vec<String> },
}

impl App {
    /// `enter`: plans the selection once the scan has finished. Planning runs on a
    /// worker; the review opens when it answers.
    pub(super) fn review(&mut self) {
        let totals = self.totals();
        if totals.selected_count == 0 {
            self.notice =
                Some("Nothing selected: s selects a finding, c the Safe bucket.".to_owned());
            return;
        }
        if self.progress == Progress::Scanning || self.report.is_none() {
            self.notice = Some(review::REVIEW_AFTER_SCAN.to_owned());
            return;
        }
        self.generation = Generation(self.generation.0 + 1);
        self.cleaning = Some(Cleaning::Planning(self.generation));
        self.effect = Some(Effect::Plan {
            generation: self.generation,
            selection: Selection(self.selected.clone()),
        });
    }

    /// The worker answered request `generation` with `prepared`, planned under a config
    /// that asks for the size to be typed above `threshold`. Only the latest request
    /// opens the review.
    pub(super) fn planned(
        &mut self,
        generation: Generation,
        prepared: Prepared,
        threshold: Option<u64>,
    ) {
        if !matches!(self.cleaning, Some(Cleaning::Planning(asked)) if asked == generation) {
            return;
        }
        let total = prepared.plan().estimated_total();
        self.cleaning = Some(Cleaning::Reviewing(Review {
            prepared: Arc::new(prepared),
            total,
            threshold,
            scroll: 0,
        }));
    }

    /// Request `generation` couldn't be planned, for the reason `why`: back to the tree.
    pub(super) fn not_planned(&mut self, generation: Generation, why: String) {
        if matches!(self.cleaning, Some(Cleaning::Planning(asked)) if asked == generation) {
            self.cleaning = None;
            self.notice = Some(why);
        }
    }

    /// The confirmed plan's worker reported `event`.
    pub(super) fn applied(&mut self, event: ApplyEvent) {
        let Some(Cleaning::Applying(applying)) = &mut self.cleaning else {
            return;
        };
        if let Some(notice) = review::notice(&event) {
            applying.notices.push(notice);
        }
        match event {
            ApplyEvent::RunStarted { run, .. } => applying.run = Some(run),
            ApplyEvent::Item {
                event: ItemEvent::Started { item },
                ..
            } => applying.current = Some(item),
            ApplyEvent::Item {
                event: ItemEvent::Ended { item, outcome },
                ..
            } => {
                if applying.current == Some(item) {
                    applying.current = None;
                }
                applying.ended.push(ItemEnded {
                    item,
                    outcome: *outcome,
                });
            }
            ApplyEvent::Reconciled { .. }
            | ApplyEvent::Unreadable { .. }
            | ApplyEvent::RestoreStarted { .. }
            | ApplyEvent::RestoreEnded { .. } => {}
        }
    }

    /// The confirmed plan's run finished as `finished` says.
    pub(super) fn ran(&mut self, finished: Finished) {
        let notices = self.leave_applying();
        self.cleaning = Some(Cleaning::Ended(Ended::Ran {
            finished: Box::new(finished),
            notices,
            scroll: 0,
        }));
    }

    /// The worker that applied failed: the run, if it was stored, is reconciled by the
    /// next command that changes anything.
    pub(super) fn apply_failed(&mut self) {
        let notices = self.leave_applying();
        self.cleaning = Some(Cleaning::Ended(Ended::Failed { notices }));
    }

    /// The planning worker failed.
    pub(super) fn plan_failed(&mut self, generation: Generation) {
        self.not_planned(generation, review::PLANNING_FAILED.to_owned());
    }

    /// Confirming refused before anything ran, for the reason `why`: back to the review.
    pub(super) fn not_applied(&mut self, why: String) {
        if let Some(Cleaning::Applying(applying)) = self.cleaning.take() {
            self.cleaning = Some(Cleaning::Reviewing(applying.review));
            self.notice = Some(why);
        }
    }

    /// The config now asks for the size of the plan to be typed: confirming waits for it.
    pub(super) fn must_type(&mut self) {
        if let Some(Cleaning::Applying(applying)) = self.cleaning.take() {
            self.cleaning = Some(Cleaning::Typing(applying.review, String::new()));
            self.notice = Some(review::TYPE_THE_SIZE.to_owned());
        }
    }

    /// The notices of the apply that ended.
    fn leave_applying(&mut self) -> Vec<String> {
        match self.cleaning.take() {
            Some(Cleaning::Applying(applying)) => applying.notices,
            other => {
                self.cleaning = other;
                Vec::new()
            }
        }
    }

    /// A key while a clean is under way.
    pub(super) fn clean_key(&mut self, key: Key) {
        let Some(cleaning) = self.cleaning.take() else {
            return;
        };
        self.notice = None;
        self.cleaning = match cleaning {
            Cleaning::Planning(generation) => self.planning_key(generation, key),
            Cleaning::Reviewing(review) => self.review_key(review, key),
            Cleaning::Typing(review, typed) => Some(self.typing_key(review, typed, key)),
            Cleaning::Applying(applying) => Some(self.applying_key(applying, key)),
            Cleaning::Ended(ended) => self.ended_key(ended, key),
        };
    }

    /// Keeps the review's or the result's scroll within its last page, so `k` after
    /// scrolling past the end moves the view at once.
    pub(super) fn clamp_clean_scroll(&mut self) {
        let end = crate::tui::ui::clean_end(self);
        match &mut self.cleaning {
            Some(Cleaning::Reviewing(review) | Cleaning::Typing(review, _)) => {
                review.scroll = review.scroll.min(end);
            }
            Some(Cleaning::Ended(Ended::Ran { scroll, .. })) => *scroll = (*scroll).min(end),
            Some(Cleaning::Planning(_) | Cleaning::Applying(_) | Cleaning::Ended(_)) | None => {}
        }
    }

    /// While planning: `esc` stops it and goes back to the tree; quitting stops it too.
    fn planning_key(&mut self, generation: Generation, key: Key) -> Option<Cleaning> {
        if key == Key::Esc {
            self.effect = Some(Effect::StopPlanning);
            self.notice = Some(review::PLANNING_STOPPED.to_owned());
            return None;
        }
        if key == Key::Interrupt
            || self.keymap().action(keymap::Mode::Review, key) == Some(Action::Quit)
        {
            self.quit = true;
        } else {
            self.notice = Some(review::PLANNING.to_owned());
        }
        Some(Cleaning::Planning(generation))
    }

    /// On the review: scroll, confirm, or go back to the tree.
    fn review_key(&mut self, mut review: Review, key: Key) -> Option<Cleaning> {
        if key == Key::Esc {
            return None;
        }
        if key == Key::Interrupt {
            self.quit = true;
            return Some(Cleaning::Reviewing(review));
        }
        match self.keymap().action(keymap::Mode::Review, key) {
            Some(Action::Quit) => self.quit = true,
            Some(Action::MoveDown) => review.scroll = review.scroll.saturating_add(1),
            Some(Action::MoveUp) => review.scroll = review.scroll.saturating_sub(1),
            Some(Action::Confirm) if !review.runs() => {}
            Some(Action::Confirm) if review.needs_typing() => {
                return Some(Cleaning::Typing(review, String::new()));
            }
            Some(Action::Confirm) => return Some(self.confirm(review, Confirmation::Key)),
            Some(_) | None => {}
        }
        Some(Cleaning::Reviewing(review))
    }

    /// Typing the size: `enter` confirms only the size shown; anything else declines and
    /// goes back to the review. `esc` goes back to it too.
    fn typing_key(&mut self, review: Review, mut typed: String, key: Key) -> Cleaning {
        match key {
            Key::Char(c) => typed.push(c),
            Key::Backspace => {
                typed.pop();
            }
            Key::Enter if receipt::is_typed_size(&typed, &review.total) => {
                return self.confirm(review, Confirmation::Size);
            }
            Key::Enter => {
                self.notice = Some(receipt::NOT_THE_SIZE.to_owned());
                return Cleaning::Reviewing(review);
            }
            Key::Esc => return Cleaning::Reviewing(review),
            Key::Interrupt => self.quit = true,
            Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Tab => {}
        }
        Cleaning::Typing(review, typed)
    }

    /// Asks the loop to run `review`'s plan as confirmed `by`.
    fn confirm(&mut self, review: Review, by: Confirmation) -> Cleaning {
        self.effect = Some(Effect::Apply(by));
        Cleaning::Applying(Applying {
            review,
            run: None,
            current: None,
            ended: Vec::new(),
            notices: Vec::new(),
            stopping: Stopping::No,
        })
    }

    /// While it runs: the first Ctrl+C stops it after the current item, the second ends
    /// kipple at once. Every other key says how to stop it.
    fn applying_key(&mut self, mut applying: Applying, key: Key) -> Cleaning {
        match (key, applying.stopping) {
            (Key::Interrupt, Stopping::No) => {
                applying.stopping = Stopping::Asked;
                self.effect = Some(Effect::StopApply);
                self.notice = Some(review::STOPPING.to_owned());
            }
            (Key::Interrupt, Stopping::Asked) => {
                self.effect = Some(Effect::Abort(applying.run.clone()));
            }
            (_, Stopping::Asked) => self.notice = Some(review::STOPPING.to_owned()),
            (_, Stopping::No) => self.notice = Some(review::STOP_KEY.to_owned()),
        }
        Cleaning::Applying(applying)
    }

    /// On the result: scroll, or go back home, which scans again.
    fn ended_key(&mut self, mut ended: Ended, key: Key) -> Option<Cleaning> {
        let action = self.keymap().action(keymap::Mode::Result, key);
        if key == Key::Esc || action == Some(Action::Review) {
            self.forget_scan();
            self.effect = Some(Effect::Rescan);
            return None;
        }
        if key == Key::Interrupt || action == Some(Action::Quit) {
            self.quit = true;
        }
        if let Ended::Ran { scroll, .. } = &mut ended {
            match action {
                Some(Action::MoveDown) => *scroll = scroll.saturating_add(1),
                Some(Action::MoveUp) => *scroll = scroll.saturating_sub(1),
                Some(_) | None => {}
            }
        }
        Some(Cleaning::Ended(ended))
    }
}
