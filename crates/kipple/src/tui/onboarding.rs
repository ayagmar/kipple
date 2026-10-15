//! The first run (`docs/03-functional-spec.md` §2 J0): four short steps before the home
//! screen, shown only when there is no config file at the default location. The scan
//! runs meanwhile, so the first step shows what each level would put in Safe and Review
//! on this machine. Finishing writes the answers in one go, so it runs once; quitting
//! writes nothing.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use kipple_core::{Finding, Level, Method, Policy, ScanOutcome, Verdict};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::app::{App, Key, Progress};
use super::view::{Look, level_about, title_case};
use crate::config::{Edit, Loaded, Source, is_config_path, same_config_path};
use crate::human::{bytes, escaped};

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
    const fn number(self) -> usize {
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
    /// The line the cursor is on in step 2: a root, or "add another" after them.
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
        }
    }

    /// Applies `key`: enter answers the step and goes on, tab skips it, esc goes back,
    /// q quits.
    pub(crate) fn key(&mut self, key: Key) -> Outcome {
        self.notice = None;
        if self.typing.is_some() && key != Key::Interrupt {
            self.type_root(key);
            return Outcome::Stay;
        }
        match key {
            Key::Char('q') | Key::Interrupt => Outcome::Quit,
            Key::Esc => {
                self.step = self.step.back();
                Outcome::Stay
            }
            Key::Tab => {
                self.answer(false);
                self.advance()
            }
            Key::Enter if self.step == Step::Roots && self.on_add() => {
                self.typing = Some(String::new());
                Outcome::Stay
            }
            Key::Enter => {
                self.answer(true);
                self.advance()
            }
            Key::Char('j') | Key::Down => {
                self.move_by(1);
                Outcome::Stay
            }
            Key::Char('k') | Key::Up => {
                self.move_by(-1);
                Outcome::Stay
            }
            Key::Char(' ') if self.step == Step::Roots => {
                self.toggle_root();
                Outcome::Stay
            }
            Key::Char(_)
            | Key::Left
            | Key::Right
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Backspace => Outcome::Stay,
        }
    }

    /// Records the answer to this step, or that it was skipped.
    fn answer(&mut self, given: bool) {
        match self.step {
            Step::Level => self.chosen_level = given.then_some(self.level),
            Step::Roots => {
                self.chosen_roots = given.then(|| {
                    self.roots
                        .iter()
                        .filter(|root| root.checked)
                        .map(|root| root.path.clone())
                        .collect()
                });
            }
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

    const fn on_add(&self) -> bool {
        self.root_cursor >= self.roots.len()
    }

    fn move_by(&mut self, by: isize) {
        match self.step {
            Step::Level => self.level = step_through(Level::ALL, self.level, by),
            Step::Roots => {
                let last = self.roots.len();
                self.root_cursor = self.root_cursor.saturating_add_signed(by).min(last);
            }
            Step::Method => self.method = step_through(Method::ALL, self.method, by),
            Step::Tips if by < 0 => self.scroll = self.scroll.saturating_sub(1),
            Step::Tips => self.scroll = self.scroll.saturating_add(1),
        }
    }

    fn toggle_root(&mut self) {
        match self.roots.get_mut(self.root_cursor) {
            Some(root) => root.checked = !root.checked,
            None => self.typing = Some(String::new()),
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
                    .any(|root| same_config_path(&root.path, Path::new(&typed), home));
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
    let done = format!("Your answers are saved in {file}. Settings (,) changes them.");
    app.save(edits, done);
}

/// Draws the step `onboarding` is on, with `app`'s scan for the numbers and tips.
pub(crate) fn render(onboarding: &Onboarding, app: &App, look: Look, frame: &mut Frame<'_>) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    frame.render_widget(head(onboarding, app, look), header);
    let lines = match onboarding.step {
        Step::Level => level_lines(onboarding, app, look),
        Step::Roots => root_lines(onboarding, look),
        Step::Method => method_lines(onboarding, look),
        Step::Tips => tip_lines(app, look),
    };
    let scroll = if onboarding.step == Step::Tips {
        onboarding.scroll
    } else {
        0
    };
    let block = Block::new()
        .borders(Borders::TOP)
        .border_set(look.border())
        .border_style(look.dim());
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, body);
    frame.render_widget(keys(onboarding, look), footer);
}

fn head(onboarding: &Onboarding, app: &App, look: Look) -> Paragraph<'static> {
    let state = match app.progress {
        Progress::Scanning => look.ellipsis("scanning"),
        Progress::Done { outcome, .. } if outcome != ScanOutcome::Completed => {
            "scan stopped early".to_owned()
        }
        Progress::Done { .. } => "scan finished".to_owned(),
    };
    Paragraph::new(Line::from(vec![
        Span::styled(" Welcome to kipple", bold()),
        Span::raw(format!(
            "{}step {} of 4{}",
            look.dot(),
            onboarding.step.number(),
            look.dot()
        )),
        Span::styled(state, look.dim()),
    ]))
}

const fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn question(text: &str, about: &str, look: Look) -> Vec<Line<'static>> {
    vec![
        Line::styled(text.to_owned(), bold()),
        Line::styled(about.to_owned(), look.dim()),
        Line::from(""),
    ]
}

/// A choice line: a marker and reversed when the cursor is on it.
fn choice(focused: bool, text: &str) -> Line<'static> {
    let line = Line::from(format!("{}{text}", if focused { "> " } else { "  " }));
    if focused {
        line.style(Style::new().add_modifier(Modifier::REVERSED))
    } else {
        line
    }
}

fn level_lines(onboarding: &Onboarding, app: &App, look: Look) -> Vec<Line<'static>> {
    let mut lines = question(
        "How careful should kipple be?",
        "Each level shows what it would offer on this machine. It changes nothing.",
        look,
    );
    let findings = app.rows.iter().map(|row| &row.finding);
    for &level in Level::ALL {
        let totals = level_totals(findings.clone(), &app.policy, level);
        let mut name = title_case(level.name());
        if level == Level::Balanced {
            name.push_str(" (the default)");
        }
        lines.push(choice(level == onboarding.level, &name));
        lines.push(Line::styled(
            format!("    {}", level_about(level)),
            look.dim(),
        ));
        lines.push(Line::from(format!(
            "    {} safe to clean, {} to review",
            bytes(totals.safe),
            bytes(totals.review)
        )));
    }
    lines.push(Line::from(""));
    let note = match app.progress {
        Progress::Scanning => "The numbers grow while the scan runs.",
        Progress::Done { .. } => "The scan has finished: these are the totals.",
    };
    lines.push(Line::styled(note, look.dim()));
    lines
}

fn root_lines(onboarding: &Onboarding, look: Look) -> Vec<Line<'static>> {
    let mut lines = question(
        "Where are your projects?",
        "kipple looks for build output and dependencies below these folders.",
        look,
    );
    if onboarding.roots.is_empty() {
        lines.push(Line::styled(
            "None of the usual project folders exist (~/projects, ~/code, ~/src and others).",
            look.dim(),
        ));
    }
    for (index, root) in onboarding.roots.iter().enumerate() {
        let mark = if root.checked { "[x]" } else { "[ ]" };
        let path = escaped(&root.path.display().to_string());
        lines.push(choice(
            index == onboarding.root_cursor,
            &format!("{mark} {path}"),
        ));
    }
    let add = onboarding.typing.as_ref().map_or_else(
        || "+ add another".to_owned(),
        |typed| format!("+ {}_", escaped(typed)),
    );
    lines.push(choice(onboarding.on_add(), &add));
    if let Some(notice) = &onboarding.notice {
        lines.push(Line::from(""));
        lines.push(Line::from(notice.clone()));
    }
    lines
}

fn method_lines(onboarding: &Onboarding, look: Look) -> Vec<Line<'static>> {
    let mut lines = question(
        "Delete or move to Trash by default?",
        "This version only stores the answer: nothing is removed yet.",
        look,
    );
    for &method in Method::ALL {
        let about = match method {
            Method::Delete => "frees the space at once",
            Method::Trash => "can be put back, but frees nothing until the Trash is emptied",
        };
        lines.push(choice(
            method == onboarding.method,
            &format!("{:<8}{about}", title_case(method.name())),
        ));
    }
    lines
}

fn tip_lines(app: &App, look: Look) -> Vec<Line<'static>> {
    let mut lines = question(
        "Prevention tips",
        "Settings of your tools that keep less around. kipple shows them and never changes them.",
        look,
    );
    let tips: Vec<_> = app.advice.values().flatten().collect();
    if tips.is_empty() {
        let none = match app.progress {
            Progress::Scanning => "Still scanning: tips appear once the tools are found.",
            Progress::Done { .. } => "kipple knows no retention setting for the tools it found.",
        };
        lines.push(Line::from(none));
    }
    for tip in tips {
        lines.push(Line::styled(escaped(&tip.title), bold()));
        lines.push(Line::from(escaped(&tip.text)));
        for snippet in &tip.snippets {
            lines.push(Line::styled(format!("  {}", escaped(snippet)), look.dim()));
        }
        lines.push(Line::from(""));
    }
    lines
}

fn keys(onboarding: &Onboarding, look: Look) -> Paragraph<'static> {
    let mut text = String::from(" ");
    if onboarding.typing.is_some() {
        text.push_str("type a path, enter adds it, esc cancels");
    } else {
        let enter = match onboarding.step {
            Step::Tips => "enter finish",
            Step::Level | Step::Roots | Step::Method => "enter choose",
        };
        text.push_str(enter);
        if onboarding.step == Step::Roots {
            write!(text, "{}space check", look.dot()).ok();
        }
        if onboarding.step == Step::Tips {
            write!(text, "{}j k scroll", look.dot()).ok();
        }
        write!(text, "{}tab skip", look.dot()).ok();
        if onboarding.step != Step::Level {
            write!(text, "{}esc back", look.dot()).ok();
        }
        write!(text, "{}q quit, saving nothing", look.dot()).ok();
    }
    Paragraph::new(Line::styled(text, look.dim()))
}
