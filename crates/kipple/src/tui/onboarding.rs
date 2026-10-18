//! The first run (`docs/03-functional-spec.md` §2 J0): four short steps before the home
//! screen, shown only when there is no config file at the default location. The scan
//! runs meanwhile, so the first step shows what each level would put in Safe and Review
//! on this machine. Finishing writes the answers in one go, so it runs once; quitting
//! writes nothing.

use std::path::{Path, PathBuf};

use kipple_core::{Finding, Level, Method, Policy, Suggestion, Verdict};

use super::app::{App, Key};
use super::keymap::{Action, Keymap, Mode};
use super::settings::Found;
use crate::config::{Edit, Loaded, Source, is_config_path, same_config_path, within_config_path};
use crate::human::escaped;

/// Whether the first run is due: no config file at the default location, in a known
/// config directory. A file named by `--config` or `KIPPLE_CONFIG` never starts it.
#[must_use]
pub fn due(loaded: &Loaded) -> bool {
    !loaded.exists && loaded.location.source == Source::Default && loaded.location.path.is_some()
}

/// What a level would put in Safe and in Review: bytes on disk, allocated where known.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LevelTotals {
    /// Safe to clean.
    pub safe: u64,
    /// To review.
    pub review: u64,
}

/// The Safe and Review totals of the measured `findings` at `level`, every other setting
/// of `policy` kept, each judged by [`Finding::verdict`] as everywhere else.
#[must_use]
pub fn level_totals<'a>(
    findings: impl IntoIterator<Item = &'a Finding>,
    policy: &Policy,
    level: Level,
) -> LevelTotals {
    let policy = Policy {
        level,
        ..policy.clone()
    };
    let mut totals = LevelTotals::default();
    for finding in findings {
        let size = finding
            .size
            .as_ref()
            .map_or(0, |size| size.allocated.map_or(size.apparent.0, |a| a.0));
        match finding.verdict(&policy) {
            Some(Verdict::Safe) => totals.safe += size,
            Some(Verdict::Review(_)) => totals.review += size,
            Some(Verdict::Protected(_)) | None => {}
        }
    }
    totals
}

/// The four steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Level,
    Roots,
    Method,
    Tips,
}

impl Step {
    pub(crate) const fn number(self) -> usize {
        match self {
            Self::Level => 1,
            Self::Roots => 2,
            Self::Method => 3,
            Self::Tips => 4,
        }
    }

    const fn next(self) -> Option<Self> {
        match self {
            Self::Level => Some(Self::Roots),
            Self::Roots => Some(Self::Method),
            Self::Method => Some(Self::Tips),
            Self::Tips => None,
        }
    }

    const fn back(self) -> Self {
        match self {
            Self::Level | Self::Roots => Self::Level,
            Self::Method => Self::Roots,
            Self::Tips => Self::Method,
        }
    }
}

/// A project root offered in step 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Root {
    pub(crate) path: PathBuf,
    pub(crate) checked: bool,
}

/// A root kipple suggests in step 2 (D-053), unchecked until the user checks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Offer {
    pub(crate) suggestion: Suggestion,
    pub(crate) checked: bool,
}

/// What a key asks of the loop.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Stay,
    /// Close kipple, writing nothing.
    Quit,
    /// Write these edits and open the home screen.
    Finish(Vec<Edit>),
}

/// Where the first run stands, and the answers given so far. A step skipped has no
/// answer.
#[derive(Debug)]
pub(crate) struct Onboarding {
    pub(crate) step: Step,
    /// The level the cursor is on in step 1.
    pub(crate) level: Level,
    /// The project roots kipple found, then any added, each checked or not.
    pub(crate) roots: Vec<Root>,
    /// The roots kipple suggests, below "add another".
    pub(crate) offers: Vec<Offer>,
    /// The line the cursor is on in step 2: a root, "add another" after them, then a
    /// suggestion.
    pub(crate) root_cursor: usize,
    /// The root being typed, if any.
    pub(crate) typing: Option<String>,
    /// The method the cursor is on in step 3.
    pub(crate) method: Method,
    /// How far step 4 is scrolled.
    pub(crate) scroll: u16,
    /// What the last key couldn't do, and why.
    pub(crate) notice: Option<String>,
    /// The roots kipple found, to tell whether they changed.
    found: Vec<PathBuf>,
    /// The home directory `~` stands for, when it is known.
    home: Option<PathBuf>,
    chosen_level: Option<Level>,
    chosen_roots: Option<Vec<PathBuf>>,
    chosen_method: Option<Method>,
    /// The keys, as the config had them when the first run opened.
    pub(crate) keymap: Keymap,
}

impl Onboarding {
    /// Starts at step 1 with Balanced, the roots `app` scans and Delete preselected.
    pub(crate) fn new(app: &App) -> Self {
        let roots = app
            .roots
            .iter()
            .map(|path| Root {
                path: path.clone(),
                checked: true,
            })
            .collect();
        Self {
            step: Step::Level,
            level: Level::Balanced,
            roots,
            offers: Vec::new(),
            root_cursor: 0,
            typing: None,
            method: Method::Delete,
            scroll: 0,
            notice: None,
            found: app.roots.clone(),
            home: app.home.clone(),
            chosen_level: None,
            chosen_roots: None,
            chosen_method: None,
            keymap: app.keymap().clone(),
        }
    }

    /// Lists the suggestions of the search that just ended, each unchecked.
    pub(crate) fn suggested(&mut self, found: &Found) {
        self.offers = found
            .suggestions
            .iter()
            .map(|suggestion| Offer {
                suggestion: suggestion.clone(),
                checked: false,
            })
            .collect();
    }

    /// Applies `key`: by default enter answers the step and goes on, tab skips it, q
    /// quits; esc always goes back and Ctrl-C always quits.
    pub(crate) fn key(&mut self, key: Key) -> Outcome {
        self.notice = None;
        if self.typing.is_some() && key != Key::Interrupt {
            self.type_root(key);
            return Outcome::Stay;
        }
        match key {
            Key::Interrupt => return Outcome::Quit,
            Key::Esc => {
                self.step = self.step.back();
                return Outcome::Stay;
            }
            Key::Char(_)
            | Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Enter
            | Key::Backspace
            | Key::Tab => {}
        }
        self.keymap
            .action(Mode::Onboarding, key)
            .map_or(Outcome::Stay, |action| self.act(action))
    }

    fn act(&mut self, action: Action) -> Outcome {
        match action {
            Action::Quit => return Outcome::Quit,
            Action::Skip => {
                self.answer(false);
                return self.advance();
            }
            Action::Choose if self.step == Step::Roots && self.on_add() => {
                self.typing = Some(String::new());
            }
            Action::Choose => {
                self.answer(true);
                return self.advance();
            }
            Action::MoveDown => self.move_by(1),
            Action::MoveUp => self.move_by(-1),
            Action::Check if self.step == Step::Roots => self.toggle_root(),
            Action::Check
            | Action::PageDown
            | Action::PageUp
            | Action::First
            | Action::Last
            | Action::Fold
            | Action::Unfold
            | Action::Filter
            | Action::Lens
            | Action::Select
            | Action::SelectGroup
            | Action::SelectSafe
            | Action::Keep
            | Action::KeepRule
            | Action::Sort
            | Action::DetailDown
            | Action::DetailUp
            | Action::Copy
            | Action::Settings
            | Action::Review
            | Action::Help
            | Action::ChangeBack
            | Action::Change
            | Action::Remove => {}
        }
        Outcome::Stay
    }

    /// Records the answer to this step, or that it was skipped.
    fn answer(&mut self, given: bool) {
        match self.step {
            Step::Level => self.chosen_level = given.then_some(self.level),
            Step::Roots => self.chosen_roots = given.then(|| self.checked_roots()),
            Step::Method => self.chosen_method = given.then_some(self.method),
            Step::Tips => {}
        }
    }

    fn advance(&mut self) -> Outcome {
        match self.step.next() {
            Some(next) => {
                self.step = next;
                Outcome::Stay
            }
            None => Outcome::Finish(self.edits()),
        }
    }

    /// What finishing writes: always the level, Balanced when step 1 was skipped, so the
    /// first run happens once; the roots only when they differ from those kipple found,
    /// so the defaults stay live otherwise; the method when one was chosen.
    pub(crate) fn edits(&self) -> Vec<Edit> {
        let mut edits = vec![Edit::Level(self.chosen_level.unwrap_or(Level::Balanced))];
        if let Some(roots) = self.chosen_roots.as_ref().filter(|r| **r != self.found) {
            edits.push(Edit::ProjectRoots(roots.clone()));
        }
        if let Some(method) = self.chosen_method {
            edits.push(Edit::Method(method));
        }
        edits
    }

    /// The checked roots, then the checked suggestions that aren't among them or inside
    /// one of them.
    fn checked_roots(&self) -> Vec<PathBuf> {
        let mut chosen: Vec<PathBuf> = self
            .roots
            .iter()
            .filter(|root| root.checked)
            .map(|root| root.path.clone())
            .collect();
        let home = self.home.as_deref();
        for offer in self.offers.iter().filter(|offer| offer.checked) {
            let path = &offer.suggestion.path;
            if !chosen
                .iter()
                .any(|root| within_config_path(path, root, home))
            {
                chosen.push(path.clone());
            }
        }
        chosen
    }

    pub(crate) const fn on_add(&self) -> bool {
        self.root_cursor == self.roots.len()
    }

    fn move_by(&mut self, by: isize) {
        match self.step {
            Step::Level => self.level = step_through(Level::ALL, self.level, by),
            Step::Roots => {
                let last = self.roots.len() + self.offers.len();
                self.root_cursor = self.root_cursor.saturating_add_signed(by).min(last);
            }
            Step::Method => self.method = step_through(Method::ALL, self.method, by),
            Step::Tips if by < 0 => self.scroll = self.scroll.saturating_sub(1),
            Step::Tips => self.scroll = self.scroll.saturating_add(1),
        }
    }

    fn toggle_root(&mut self) {
        let at = self.root_cursor;
        if let Some(root) = self.roots.get_mut(at) {
            root.checked = !root.checked;
        } else if let Some(offer) = at
            .checked_sub(self.roots.len() + 1)
            .and_then(|index| self.offers.get_mut(index))
        {
            offer.checked = !offer.checked;
        } else {
            self.typing = Some(String::new());
        }
    }

    fn type_root(&mut self, key: Key) {
        let Some(typed) = &mut self.typing else {
            return;
        };
        match key {
            Key::Char(c) => typed.push(c),
            Key::Backspace => {
                typed.pop();
            }
            Key::Esc => self.typing = None,
            Key::Enter => {
                let typed = typed.trim().to_owned();
                let home = self.home.as_deref();
                let listed = self
                    .roots
                    .iter()
                    .map(|root| &root.path)
                    .chain(self.offers.iter().map(|offer| &offer.suggestion.path))
                    .any(|path| same_config_path(path, Path::new(&typed), home));
                if typed.is_empty() {
                    self.typing = None;
                } else if listed {
                    self.typing = None;
                    self.notice = Some(format!("{} is already in the list.", escaped(&typed)));
                } else if is_config_path(&typed) {
                    self.typing = None;
                    self.roots.push(Root {
                        path: PathBuf::from(typed),
                        checked: true,
                    });
                    self.root_cursor = self.roots.len() - 1;
                } else {
                    self.notice =
                        Some("A project root is an absolute path or starts with ~.".to_owned());
                }
            }
            Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Tab
            | Key::Interrupt => {}
        }
    }
}

/// The value `by` steps from `at` in `all`, stopping at either end.
fn step_through<T: Copy + PartialEq>(all: &[T], at: T, by: isize) -> T {
    let index = all.iter().position(|&v| v == at).unwrap_or(0);
    let to = index
        .saturating_add_signed(by)
        .min(all.len().saturating_sub(1));
    all.get(to).copied().unwrap_or(at)
}

/// Asks the home screen to write `edits` and say where they went once they are written.
pub(crate) fn finish(app: &mut App, edits: Vec<Edit>) {
    let file = app.file.as_ref().map_or_else(
        || "the config file".to_owned(),
        |file| escaped(&file.display().to_string()),
    );
    let done = format!(
        "Your answers are saved in {file}. {} changes them.",
        app.settings_hint()
    );
    app.save(edits, done);
}
