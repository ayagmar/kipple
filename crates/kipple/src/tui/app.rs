//! The home screen's state machine (`docs/05-architecture.md` §4): [`App::update`] takes
//! one message at a time, and nothing else changes the state, so every transition is
//! testable without a terminal. Selecting only marks findings: nothing here removes,
//! moves or runs anything. Changing a setting asks the loop for an [`Effect`], a config
//! write, and the screen changes only once the loop says it was written.

mod keep;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use kipple_core::{
    Advice, Caution, Finding, FindingId, IntegrationId, Lens, Policy, RuleId, RuleText, ScanEvent,
    ScanOutcome, ScanReport, Verdict,
};

pub(crate) use self::keep::{Effect, Saved};
use super::filter;
use super::settings::{Action, Panel};
use crate::config::Config;
use crate::json::{OmissionOut, SizeOut};
use crate::setup::Builtin;

/// A key, as the home screen reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character.
    Char(char),
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Enter.
    Enter,
    /// Escape.
    Esc,
    /// Backspace.
    Backspace,
    /// Tab.
    Tab,
    /// Ctrl-C.
    Interrupt,
}

/// Everything that changes the home screen.
#[derive(Debug)]
pub(crate) enum Msg {
    /// A key was pressed.
    Key(Key),
    /// The terminal is now this size.
    Resize { width: u16, height: u16 },
    /// The scan reported something.
    Scan(Box<ScanEvent>),
    /// The scan returned its report, this long after it started.
    Finished {
        report: Box<ScanReport>,
        elapsed: Duration,
    },
    /// The config was written; this is what it says now.
    Saved(Box<Saved>),
    /// The config couldn't be written, for this reason. Nothing changed.
    NotSaved(String),
}

/// Where a finding stands: its bucket, or still being measured. Nothing is Safe until
/// kipple knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Shelf {
    Safe,
    Review,
    Protected,
    Measuring,
}

impl Shelf {
    pub(crate) const SORTED: [Self; 3] = [Self::Safe, Self::Review, Self::Protected];

    const fn of(verdict: Option<&Verdict>) -> Self {
        match verdict {
            Some(Verdict::Safe) => Self::Safe,
            Some(Verdict::Review(_)) => Self::Review,
            Some(Verdict::Protected(_)) => Self::Protected,
            None => Self::Measuring,
        }
    }
}

/// One line of the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Node {
    /// A bucket heading.
    Shelf(Shelf),
    /// A lens within a bucket.
    Group(Shelf, Lens),
    /// A finding.
    Finding(FindingId),
    /// The heading of what the scan couldn't cover.
    Gaps,
    /// One thing the scan couldn't cover, by its index.
    Gap(usize),
    /// A notice of the report, by its index: something to know that doesn't make the
    /// report partial.
    Notice(usize),
}

/// A visible line and how deep it sits in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) node: Node,
    pub(crate) depth: u8,
}

/// What the home screen is doing with the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Browse,
    /// Typing the filter.
    Filter,
    Help,
    /// Typing the note for keeping this finding.
    Note(FindingId),
    /// The settings panel.
    Settings,
}

/// How far the scan has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Progress {
    Scanning,
    Done {
        outcome: ScanOutcome,
        elapsed: Duration,
    },
}

/// A finding with what the home screen shows for it.
#[derive(Debug)]
pub(crate) struct Row {
    pub(crate) finding: Finding,
    pub(crate) lens: Lens,
    pub(crate) verdict: Option<Verdict>,
    pub(crate) size: Option<SizeOut>,
    /// Its rule's title and its path, lowercased, for the filter.
    haystack: String,
}

impl Row {
    pub(crate) const fn shelf(&self) -> Shelf {
        Shelf::of(self.verdict.as_ref())
    }

    /// What it takes on disk: allocated storage where known, else apparent size.
    pub(crate) fn bytes(&self) -> u64 {
        self.size
            .as_ref()
            .map_or(0, |size| size.allocated.unwrap_or(size.apparent))
    }

    /// What removing it frees at once: only the storage no other path holds.
    fn reclaim(&self) -> u64 {
        self.size.as_ref().map_or(0, |size| {
            size.unique_reclaim
                .or(size.allocated)
                .unwrap_or(size.apparent)
        })
    }

    /// Whether it may be selected: Safe or Review, never Protected or unmeasured.
    const fn eligible(&self) -> bool {
        matches!(self.shelf(), Shelf::Safe | Shelf::Review)
    }

    /// Whether it changed within its rule's recent window, which keeps it out of every
    /// group shortcut.
    fn recent(&self) -> bool {
        matches!(&self.verdict, Some(Verdict::Review(cautions)) if cautions.contains(&Caution::Recent))
    }
}

/// Something the scan couldn't cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Gap {
    pub(crate) integration: String,
    pub(crate) message: String,
}

/// The rules' texts and the integrations' lenses, fixed for the session.
#[derive(Debug, Default)]
pub(crate) struct Catalog {
    pub(crate) rules: BTreeMap<RuleId, RuleText>,
    pub(crate) lenses: BTreeMap<IntegrationId, Lens>,
}

impl Catalog {
    pub(crate) fn new(builtin: &Builtin) -> Self {
        let rules = builtin
            .rules()
            .map(|(_, rule)| (rule.id.clone(), rule.text.clone()))
            .collect();
        Self {
            rules,
            lenses: builtin.lenses(),
        }
    }

    pub(crate) fn title<'a>(&'a self, rule: &'a RuleId) -> &'a str {
        self.rules.get(rule).map_or(&rule.0, |text| &text.title)
    }
}

/// Sizes summed over the findings that count for each total
/// (`docs/03-functional-spec.md` §2 J1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    pub(crate) discovered: u64,
    pub(crate) eligible: u64,
    pub(crate) selected: u64,
    pub(crate) selected_count: usize,
    pub(crate) reclaim: u64,
}

/// The home screen.
#[derive(Debug)]
pub(crate) struct App {
    /// What every row's bucket comes from: the config's level, rules and paths.
    pub(crate) policy: Policy,
    /// The config file, when its directory is known.
    pub(crate) file: Option<PathBuf>,
    /// What the config file says.
    pub(crate) config: Config,
    /// The project roots scanned, as configured or by default.
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) catalog: Catalog,
    pub(crate) rows: Vec<Row>,
    by_id: HashMap<FindingId, usize>,
    pub(crate) gaps: Vec<Gap>,
    /// What the report says beside the gaps, such as processes left out of the activity
    /// check (D-050). Not counted as not covered.
    pub(crate) notices: Vec<String>,
    pub(crate) advice: BTreeMap<IntegrationId, Vec<Advice>>,
    pub(crate) progress: Progress,
    collapsed: BTreeSet<Node>,
    pub(crate) selected: BTreeSet<FindingId>,
    /// The selected findings a shortcut (`a`, `c`) added, with the bucket each was in.
    shortcut: BTreeMap<FindingId, Shelf>,
    pub(crate) filter: String,
    pub(crate) lens: Option<Lens>,
    pub(crate) mode: Mode,
    pub(crate) notice: Option<String>,
    /// The note being typed for `p`.
    pub(crate) note: String,
    pub(crate) panel: Panel,
    /// What the loop is asked to do next.
    effect: Option<Effect>,
    /// What to say once the config write asked for has succeeded.
    pending: Option<String>,
    cursor: Option<Node>,
    /// Where the cursor was, for when its line goes away.
    hint: usize,
    pub(crate) scroll: usize,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) quit: bool,
}

impl App {
    pub(crate) fn new(catalog: Catalog, width: u16, height: u16) -> Self {
        Self {
            policy: Policy::default(),
            file: None,
            config: Config::default(),
            roots: Vec::new(),
            catalog,
            rows: Vec::new(),
            by_id: HashMap::new(),
            gaps: Vec::new(),
            notices: Vec::new(),
            advice: BTreeMap::new(),
            progress: Progress::Scanning,
            collapsed: BTreeSet::new(),
            selected: BTreeSet::new(),
            shortcut: BTreeMap::new(),
            filter: String::new(),
            lens: None,
            mode: Mode::Browse,
            notice: None,
            note: String::new(),
            panel: Panel::default(),
            effect: None,
            pending: None,
            cursor: None,
            hint: 0,
            scroll: 0,
            width,
            height,
            quit: false,
        }
    }

    /// Applies `msg`.
    pub(crate) fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Key(key) => self.key(key),
            Msg::Resize { width, height } => {
                self.width = width;
                self.height = height;
                self.settle();
            }
            Msg::Scan(event) => self.scan_event(*event),
            Msg::Finished { report, elapsed } => self.finished(*report, elapsed),
            Msg::Saved(saved) => self.saved(*saved),
            Msg::NotSaved(error) => self.not_saved(&error),
        }
    }

    /// The finding `id`, if it was found.
    pub(crate) fn row(&self, id: FindingId) -> Option<&Row> {
        self.by_id.get(&id).and_then(|&index| self.rows.get(index))
    }

    pub(crate) fn is_collapsed(&self, node: Node) -> bool {
        self.collapsed.contains(&node)
    }

    /// The tree as it is shown now: filtered, grouped and sorted, without what is
    /// collapsed.
    pub(crate) fn lines(&self) -> Vec<Line> {
        let mut groups: BTreeMap<(Node, Node), Vec<&Row>> = BTreeMap::new();
        for top in Self::tops() {
            groups.entry((top, top)).or_default();
        }
        for row in self.rows.iter().filter(|row| self.shown(row)) {
            groups.entry(Self::place(row)).or_default().push(row);
        }
        let mut lines = Vec::new();
        let mut last_top = None;
        for ((top, group), mut rows) in groups {
            if last_top != Some(top) {
                lines.push(Line {
                    node: top,
                    depth: 0,
                });
                last_top = Some(top);
            }
            if group == top || self.is_collapsed(top) {
                continue;
            }
            lines.push(Line {
                node: group,
                depth: 1,
            });
            if self.is_collapsed(group) {
                continue;
            }
            rows.sort_by(|a, b| (b.bytes(), &a.finding.path).cmp(&(a.bytes(), &b.finding.path)));
            lines.extend(rows.iter().map(|row| Line {
                node: Node::Finding(row.finding.id),
                depth: 2,
            }));
        }
        self.gap_lines(&mut lines);
        lines.extend((0..self.notices.len()).map(|index| Line {
            node: Node::Notice(index),
            depth: 0,
        }));
        lines
    }

    /// The findings under `node`, filtered as shown, collapsed or not.
    pub(crate) fn under(&self, node: Node) -> impl Iterator<Item = &Row> {
        self.rows
            .iter()
            .filter(move |row| self.shown(row) && Self::holds(node, row))
    }

    /// The line the cursor is on, as an index into `lines`.
    pub(crate) fn cursor_index(&self, lines: &[Line]) -> usize {
        self.cursor
            .and_then(|node| lines.iter().position(|line| line.node == node))
            .unwrap_or_else(|| self.hint.min(lines.len().saturating_sub(1)))
    }

    /// The four totals of the footer.
    pub(crate) fn totals(&self) -> Totals {
        let mut totals = Totals::default();
        for row in &self.rows {
            totals.discovered += row.bytes();
            if row.eligible() {
                totals.eligible += row.bytes();
            }
            if self.selected.contains(&row.finding.id) {
                totals.selected += row.bytes();
                totals.selected_count += 1;
                totals.reclaim += row.reclaim();
            }
        }
        totals
    }

    /// The headings shown even when nothing is under them, so an empty bucket reads as
    /// empty rather than missing.
    fn tops() -> impl Iterator<Item = Node> {
        Shelf::SORTED.into_iter().map(Node::Shelf)
    }

    /// The heading and group `row` sits under.
    const fn place(row: &Row) -> (Node, Node) {
        (Node::Shelf(row.shelf()), Node::Group(row.shelf(), row.lens))
    }

    fn holds(node: Node, row: &Row) -> bool {
        let (top, group) = Self::place(row);
        match node {
            Node::Shelf(_) => node == top,
            Node::Group(..) => node == group,
            Node::Finding(id) => id == row.finding.id,
            Node::Gaps | Node::Gap(_) | Node::Notice(_) => false,
        }
    }

    fn shown(&self, row: &Row) -> bool {
        self.lens.is_none_or(|lens| lens == row.lens)
            && filter::matches(&self.filter, &row.haystack)
    }

    fn gap_lines(&self, lines: &mut Vec<Line>) {
        if self.gaps.is_empty() {
            return;
        }
        lines.push(Line {
            node: Node::Gaps,
            depth: 0,
        });
        if self.is_collapsed(Node::Gaps) {
            return;
        }
        lines.extend((0..self.gaps.len()).map(|index| Line {
            node: Node::Gap(index),
            depth: 1,
        }));
    }

    fn scan_event(&mut self, event: ScanEvent) {
        match event {
            ScanEvent::FindingAdded { finding } => self.add(finding),
            ScanEvent::FindingUpdated { id, size, facts } => {
                if let Some(row) = self.by_id.get(&id).and_then(|&i| self.rows.get_mut(i)) {
                    row.size = size.as_ref().map(SizeOut::from);
                    row.finding.size = size;
                    row.finding.facts = Some(facts);
                    row.verdict = row.finding.verdict(&self.policy);
                }
            }
            ScanEvent::IntegrationCompleted {
                integration,
                omissions,
            } => self.gaps.extend(omissions.iter().map(|omission| Gap {
                integration: integration.0.to_owned(),
                message: OmissionOut::from(omission).message,
            })),
            ScanEvent::ScanStarted { .. }
            | ScanEvent::RootOpened { .. }
            | ScanEvent::ScanCompleted { .. } => {}
        }
    }

    fn add(&mut self, finding: Finding) {
        let lens = self
            .catalog
            .lenses
            .get(&finding.integration)
            .copied()
            .unwrap_or(Lens::System);
        let haystack = format!(
            "{} {}",
            self.catalog.title(&finding.rule),
            finding.path.display()
        )
        .to_lowercase();
        self.by_id.insert(finding.id, self.rows.len());
        self.rows.push(Row {
            finding,
            lens,
            verdict: None,
            size: None,
            haystack,
        });
    }

    fn finished(&mut self, report: ScanReport, elapsed: Duration) {
        self.notices = notices(&report);
        self.advice = report
            .integrations
            .into_iter()
            .filter(|integration| !integration.advice.is_empty())
            .map(|integration| (integration.id, integration.advice))
            .collect();
        self.progress = Progress::Done {
            outcome: report.outcome,
            elapsed,
        };
    }

    fn key(&mut self, key: Key) {
        if key == Key::Interrupt {
            self.quit = true;
            return;
        }
        match self.mode {
            Mode::Help => self.mode = Mode::Browse,
            Mode::Filter => self.filter_key(key),
            Mode::Note(id) => self.note_key(id, key),
            Mode::Settings => {
                self.notice = None;
                self.settings_key(key);
            }
            Mode::Browse => {
                self.notice = None;
                self.browse_key(key);
            }
        }
        self.settle();
    }

    fn settings_key(&mut self, key: Key) {
        let mut panel = std::mem::take(&mut self.panel);
        let action = panel.key(key, self);
        self.panel = panel;
        match action {
            Action::Nothing => {}
            Action::Close => self.mode = Mode::Browse,
            Action::Save { edits, done } => self.save(edits, done),
            Action::Say(text) => self.notice = Some(text),
        }
    }

    fn filter_key(&mut self, key: Key) {
        match key {
            Key::Char(c) => self.filter.push(c),
            Key::Backspace => {
                self.filter.pop();
            }
            Key::Enter => self.mode = Mode::Browse,
            Key::Esc => {
                self.filter.clear();
                self.mode = Mode::Browse;
            }
            Key::Up => self.step(-1),
            Key::Down => self.step(1),
            Key::Left
            | Key::Right
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Tab
            | Key::Interrupt => {}
        }
    }

    fn browse_key(&mut self, key: Key) {
        match key {
            Key::Char('q') => self.quit = true,
            Key::Char('j') | Key::Down => self.step(1),
            Key::Char('k') | Key::Up => self.step(-1),
            Key::PageDown => self.step(self.page()),
            Key::PageUp => self.step(-self.page()),
            Key::Char('g') | Key::Home => self.jump(0),
            Key::Char('G') | Key::End => self.jump(usize::MAX),
            Key::Char('h') | Key::Left => self.collapse(),
            Key::Char('l') | Key::Right => self.expand(),
            Key::Char('/') => self.mode = Mode::Filter,
            Key::Char('?') => self.mode = Mode::Help,
            Key::Char('s') => self.toggle(),
            Key::Char('a') => self.add_group(),
            Key::Char('c') => self.add_safe(),
            Key::Char('p') => self.keep_finding(),
            Key::Char('P') => self.keep_rule(),
            Key::Char(',') => self.mode = Mode::Settings,
            Key::Enter => self.review(),
            Key::Tab => self.next_lens(),
            Key::Esc => self.filter.clear(),
            Key::Char(_) | Key::Backspace | Key::Interrupt => {}
        }
    }

    /// The focused line's node.
    fn focus(&self) -> Option<Node> {
        let lines = self.lines();
        lines.get(self.cursor_index(&lines)).map(|line| line.node)
    }

    fn step(&mut self, by: isize) {
        let lines = self.lines();
        let at = self.cursor_index(&lines);
        self.jump(at.saturating_add_signed(by));
    }

    fn jump(&mut self, to: usize) {
        let lines = self.lines();
        let to = to.min(lines.len().saturating_sub(1));
        self.cursor = lines.get(to).map(|line| line.node);
        self.hint = to;
    }

    fn page(&self) -> isize {
        let rows = super::view::list_rows(self.width, self.height);
        isize::try_from(rows.saturating_sub(1).max(1)).unwrap_or(1)
    }

    fn collapse(&mut self) {
        let Some(node) = self.focus() else {
            return;
        };
        if heading(node) && !self.collapsed.contains(&node) {
            self.collapsed.insert(node);
            return;
        }
        // Already collapsed, or a leaf: go to the heading above it.
        let lines = self.lines();
        let at = self.cursor_index(&lines);
        let depth = lines.get(at).map_or(0, |line| line.depth);
        let parent = lines.iter().take(at).rposition(|line| line.depth < depth);
        if let Some(parent) = parent {
            self.jump(parent);
        }
    }

    fn expand(&mut self) {
        if let Some(node) = self.focus() {
            self.collapsed.remove(&node);
        }
    }

    fn toggle(&mut self) {
        let Some(Node::Finding(id)) = self.focus() else {
            self.notice =
                Some("Move to a finding to select it, or press a to add a group.".to_owned());
            return;
        };
        let Some(row) = self.row(id) else {
            return;
        };
        if !row.eligible() {
            self.notice = Some(not_selectable(row));
            return;
        }
        // Chosen by hand now, so no longer a shortcut's.
        self.shortcut.remove(&id);
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
    }

    /// Adds every eligible finding of the focused group, or of the focused finding's
    /// group. Never adds a Protected, unmeasured or recent one.
    fn add_group(&mut self) {
        let Some(focus) = self.focus() else {
            return;
        };
        let node = match focus {
            Node::Finding(id) => match self.row(id) {
                Some(row) => Self::place(row).1,
                None => return,
            },
            other @ (Node::Shelf(_)
            | Node::Group(..)
            | Node::Gaps
            | Node::Gap(_)
            | Node::Notice(_)) => other,
        };
        let ids: Vec<(FindingId, Shelf)> = self
            .under(node)
            .filter(|row| row.eligible() && !row.recent())
            .map(|row| (row.finding.id, row.shelf()))
            .collect();
        let added = self.add_by_shortcut(&ids);
        self.notice = Some(match ids.len() {
            0 => "Nothing here can be selected.".to_owned(),
            _ => format!("Added {added} of {} findings in this group.", ids.len()),
        });
    }

    /// Selects the whole Safe bucket as it stands at this keypress, never Review, then
    /// says what review would do.
    fn add_safe(&mut self) {
        let safe: Vec<(FindingId, Shelf)> = self
            .rows
            .iter()
            .filter(|row| row.shelf() == Shelf::Safe)
            .map(|row| (row.finding.id, Shelf::Safe))
            .collect();
        if safe.is_empty() {
            self.notice = Some("Nothing is Safe to clean yet.".to_owned());
            return;
        }
        self.add_by_shortcut(&safe);
        self.review();
    }

    /// Selects `ids`, remembering the bucket of each one this adds. How many it added.
    fn add_by_shortcut(&mut self, ids: &[(FindingId, Shelf)]) -> usize {
        let mut added = 0;
        for &(id, shelf) in ids {
            if self.selected.insert(id) {
                self.shortcut.insert(id, shelf);
                added += 1;
            }
        }
        added
    }

    /// Review and cleaning are not built yet (M3): say what is selected and that
    /// nothing changed.
    fn review(&mut self) {
        let totals = self.totals();
        self.notice = Some(if totals.selected_count == 0 {
            "Nothing selected: s selects a finding, c the Safe bucket.".to_owned()
        } else {
            format!(
                "{} selected ({}). Review and cleaning come in a later version: nothing was changed.",
                totals.selected_count,
                crate::human::bytes(totals.selected)
            )
        });
    }

    const fn next_lens(&mut self) {
        self.lens = match self.lens {
            None => Some(Lens::Agents),
            Some(Lens::Agents) => Some(Lens::Dev),
            Some(Lens::Dev) => Some(Lens::System),
            Some(Lens::System) => None,
        };
    }

    /// Keeps the cursor on a line that exists and in view.
    fn settle(&mut self) {
        let lines = self.lines();
        let at = self.cursor_index(&lines);
        self.cursor = lines.get(at).map(|line| line.node);
        self.hint = at;
        let rows = super::view::list_rows(self.width, self.height);
        self.scroll = super::view::offset(self.scroll, at, rows, lines.len());
    }
}

/// Whether `node` is a heading, which folds.
const fn heading(node: Node) -> bool {
    !matches!(node, Node::Finding(_) | Node::Gap(_) | Node::Notice(_))
}

/// The notices `report` carries, each in words.
fn notices(report: &ScanReport) -> Vec<String> {
    crate::human::unchecked_system(report.unchecked_system)
        .into_iter()
        .collect()
}

/// Why `row` can't be selected.
fn not_selectable(row: &Row) -> String {
    match &row.verdict {
        Some(verdict @ Verdict::Protected(_)) => format!(
            "Protected ({}): kipple never selects it.",
            crate::human::reasons(verdict)
        ),
        _ => "Still measuring: it can be selected once kipple knows its bucket.".to_owned(),
    }
}

#[cfg(test)]
mod tests;
