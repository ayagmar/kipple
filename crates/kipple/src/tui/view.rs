//! The home screen's renderer: a pure function of the [`App`] state and the [`Look`]
//! into a ratatui frame (`docs/03-functional-spec.md` §5). A finding's state is always
//! a word; colour only repeats it.

use std::path::Path;

use kipple_core::{Lens, Level, ScanOutcome, Verdict};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::app::{App, Line, Mode, Node, Progress, Row, Shelf};
use super::settings::{self, Item};
use crate::human::{self, bytes, escaped};

/// A terminal at least this wide shows the detail pane beside the tree; a narrower one
/// shows it below.
const WIDE: u16 = 100;
/// The size column, wide enough for `>= 1023.9 GiB`.
const SIZE: u16 = 13;
/// The state column, wide enough for its longest text: the gaps hint
/// ` so this report is partial`. The longest row state is ` unknown: what is lost +3`.
const STATE: u16 = 26;

/// How the screen is drawn: with colour unless `NO_COLOR` is set, and with Unicode
/// symbols unless `--ascii` is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    /// Whether to use colour.
    pub color: bool,
    /// Whether to draw with ASCII only.
    pub ascii: bool,
}

const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

impl Look {
    pub(crate) const fn border(self) -> border::Set<'static> {
        if self.ascii {
            ASCII_BORDER
        } else {
            border::PLAIN
        }
    }

    pub(crate) const fn dot(self) -> &'static str {
        if self.ascii { " - " } else { " · " }
    }

    const fn fold(self, collapsed: bool) -> &'static str {
        match (self.ascii, collapsed) {
            (false, false) => "▾ ",
            (false, true) => "▸ ",
            (true, false) => "v ",
            (true, true) => "> ",
        }
    }

    pub(crate) fn ellipsis(self, text: &str) -> String {
        format!("{text}{}", if self.ascii { "..." } else { "…" })
    }

    const fn mark(self) -> &'static str {
        if self.ascii { "*" } else { "●" }
    }

    const fn fg(self, color: Color) -> Style {
        if self.color {
            Style::new().fg(color)
        } else {
            Style::new()
        }
    }

    const fn tint(self, which: Shelf) -> Style {
        self.fg(match which {
            Shelf::Safe => Color::Green,
            Shelf::Review => Color::Yellow,
            Shelf::Protected => Color::Blue,
            Shelf::Measuring => Color::DarkGray,
        })
    }

    pub(crate) const fn dim(self) -> Style {
        if self.color {
            Style::new().fg(Color::DarkGray)
        } else {
            Style::new()
        }
    }
}

/// Where each part of the screen goes.
struct Areas {
    header: Rect,
    /// The tree and the detail pane together, where the settings panel goes.
    body: Rect,
    list: Rect,
    detail: Rect,
    footer: Rect,
    wide: bool,
}

fn areas(area: Rect) -> Areas {
    let [header, body, footer] = Split::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(area);
    let wide = area.width >= WIDE;
    let [list, detail] = if wide {
        Split::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(body)
    } else {
        Split::vertical([Constraint::Fill(1), Constraint::Fill(1)]).areas(body)
    };
    Areas {
        header,
        body,
        list,
        detail,
        footer,
        wide,
    }
}

/// How many tree lines fit on a `width` by `height` screen.
pub(crate) fn list_rows(width: u16, height: u16) -> usize {
    usize::from(areas(Rect::new(0, 0, width, height)).list.height)
}

/// The first tree line to show: the previous one, moved just enough to keep line `at`
/// in view, so the tree doesn't jump while sizes stream in.
pub(crate) const fn offset(scroll: usize, at: usize, rows: usize, len: usize) -> usize {
    if rows == 0 {
        return 0;
    }
    let max = len.saturating_sub(rows);
    let scroll = if scroll < max { scroll } else { max };
    if at < scroll {
        at
    } else if at >= scroll + rows {
        at + 1 - rows
    } else {
        scroll
    }
}

/// Draws `app` into `frame`.
pub(crate) fn render(app: &App, look: Look, frame: &mut Frame<'_>) {
    let areas = areas(frame.area());
    let lines = app.lines();
    let tree = Tree {
        at: app.cursor_index(&lines),
        lines: &lines,
    };
    frame.render_widget(header(app, look), areas.header);
    frame.render_widget(footer(app, look), areas.footer);
    if app.mode == Mode::Settings {
        render_settings(app, look, areas.body, frame);
        return;
    }
    render_list(app, look, &tree, areas.list, frame);
    let focus = lines.get(tree.at).map(|line| line.node);
    let borders = if areas.wide {
        Borders::LEFT
    } else {
        Borders::TOP
    };
    let block = Block::new()
        .borders(borders)
        .border_set(look.border())
        .border_style(look.dim());
    let detail = Paragraph::new(focus.map_or_else(Vec::new, |node| detail(app, look, node)))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, areas.detail);
    if app.mode == Mode::Help {
        render_help(look, frame);
    }
}

fn header(app: &App, look: Look) -> Paragraph<'static> {
    let state = match app.progress {
        Progress::Scanning => look.ellipsis("scanning"),
        Progress::Done { outcome, .. } if outcome != ScanOutcome::Completed => {
            "scan stopped early".to_owned()
        }
        Progress::Done { elapsed, .. } => {
            format!(
                "scanned in {}.{} s",
                elapsed.as_secs(),
                elapsed.subsec_millis() / 100
            )
        }
    };
    let mut spans = vec![Span::styled(
        "kipple",
        Style::new().add_modifier(Modifier::BOLD),
    )];
    for (index, &level) in Level::ALL.iter().enumerate() {
        spans.push(Span::raw(if index == 0 { "  " } else { look.dot() }));
        let style = if level == app.policy.level {
            look.fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            look.dim()
        };
        spans.push(Span::styled(title_case(level.name()), style));
    }
    spans.push(Span::raw(format!(
        " (level: {})",
        title_case(app.policy.level.name())
    )));
    spans.push(Span::raw(look.dot()));
    spans.push(Span::raw(state));
    if !app.gaps.is_empty() {
        spans.push(Span::raw(look.dot()));
        spans.push(Span::styled(
            format!("partial: {} not covered", app.gaps.len()),
            look.fg(Color::Yellow),
        ));
    }
    if let Some(lens) = app.lens {
        spans.push(Span::raw(look.dot()));
        spans.push(Span::raw(format!("lens: {}", lens_title(lens))));
    }
    if !app.filter.is_empty() && app.mode != Mode::Filter {
        spans.push(Span::raw(look.dot()));
        spans.push(Span::raw(format!("filter: {}", escaped(&app.filter))));
    }
    Paragraph::new(Text::from(spans))
}

/// The visible tree and the cursor's line in it.
struct Tree<'a> {
    lines: &'a [Line],
    at: usize,
}

fn render_list(app: &App, look: Look, tree: &Tree<'_>, area: Rect, frame: &mut Frame<'_>) {
    let (lines, at) = (tree.lines, tree.at);
    let rows = usize::from(area.height);
    let first = offset(app.scroll, at, rows, lines.len());
    let shown = lines.iter().enumerate().skip(first).take(rows);
    for (y, (index, line)) in (area.y..).zip(shown) {
        let row = Rect::new(area.x, y, area.width, 1);
        let [label, size, state] = Split::horizontal([
            Constraint::Fill(1),
            Constraint::Length(SIZE),
            Constraint::Length(STATE),
        ])
        .areas(row);
        let cells = cells(app, look, *line);
        let base = if index == at {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        frame.render_widget(Paragraph::new(cells.label).style(base), label);
        frame.render_widget(Paragraph::new(cells.size).style(base).right_aligned(), size);
        frame.render_widget(Paragraph::new(cells.state).style(base), state);
    }
}

/// One tree line's three columns.
struct Cells {
    label: Text<'static>,
    size: String,
    state: Text<'static>,
}

fn cells(app: &App, look: Look, line: Line) -> Cells {
    let indent = "  ".repeat(usize::from(line.depth));
    let fold = |node| look.fold(app.is_collapsed(node));
    match line.node {
        Node::Finding(id) => app
            .row(id)
            .map_or_else(empty_cells, |row| finding_cells(app, look, row, &indent)),
        Node::Shelf(shelf) => {
            let (count, total) = sum(app, line.node);
            Cells {
                label: Text::from(vec![
                    Span::raw(format!("  {indent}{}", fold(line.node))),
                    Span::styled(
                        format!("{} ({count})", shelf_title(shelf)),
                        look.tint(shelf).add_modifier(Modifier::BOLD),
                    ),
                ]),
                size: bytes(total),
                state: Text::styled(format!(" {}", shelf_hint(shelf)), look.dim()),
            }
        }
        Node::Group(_, lens) => {
            let (count, total) = sum(app, line.node);
            Cells {
                label: Text::from(vec![
                    Span::raw(format!("  {indent}{}", fold(line.node))),
                    Span::raw(format!("{} ({count})", lens_title(lens))),
                ]),
                size: bytes(total),
                state: Text::from(""),
            }
        }
        Node::Gaps => Cells {
            label: Text::from(vec![
                Span::raw(format!("  {indent}{}", fold(line.node))),
                Span::styled(
                    format!("Not covered ({})", app.gaps.len()),
                    look.fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
            ]),
            size: String::new(),
            state: Text::styled(" so this report is partial", look.dim()),
        },
        Node::Gap(index) => Cells {
            label: Text::raw(format!(
                "  {indent}{}",
                app.gaps
                    .get(index)
                    .map_or_else(String::new, |gap| escaped(&gap.message))
            )),
            size: String::new(),
            state: Text::from(""),
        },
    }
}

fn finding_cells(app: &App, look: Look, row: &Row, indent: &str) -> Cells {
    let selected = app.selected.contains(&row.finding.id);
    let mark = if selected {
        Span::styled(format!("{} ", look.mark()), look.fg(Color::Cyan))
    } else {
        Span::raw("  ")
    };
    Cells {
        label: Text::from(vec![
            mark,
            Span::raw(format!("{indent}{}", escaped(&tail(&row.finding.path)))),
            Span::styled(
                format!("  {}", app.catalog.title(&row.finding.rule)),
                look.dim(),
            ),
        ]),
        size: row
            .size
            .as_ref()
            .map_or_else(String::new, |size| human::size(Some(size))),
        state: Text::styled(format!(" {}", state(row)), look.tint(row.shelf())),
    }
}

fn empty_cells() -> Cells {
    Cells {
        label: Text::from(""),
        size: String::new(),
        state: Text::from(""),
    }
}

/// A row's state word: the first reason it isn't Safe, and how many more there are.
fn state(row: &Row) -> String {
    let reasons: Vec<&str> = match &row.verdict {
        None => return "measuring".to_owned(),
        Some(Verdict::Safe) => return String::new(),
        Some(Verdict::Review(cautions)) => cautions.iter().map(|&c| human::caution(c)).collect(),
        Some(Verdict::Protected(holds)) => holds.iter().map(|&h| human::hold(h)).collect(),
    };
    match reasons.as_slice() {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first, rest @ ..] => format!("{first} +{}", rest.len()),
    }
}

/// How many findings are under `node`, and their size.
fn sum(app: &App, node: Node) -> (usize, u64) {
    app.under(node).fold((0, 0), |(count, total), row| {
        (count + 1, total + row.bytes())
    })
}

/// The last two components of `path`: the project and the artifact, for example
/// `web/node_modules`.
fn tail(path: &Path) -> String {
    let parts: Vec<_> = path.components().rev().take(2).collect();
    let parts: Vec<String> = parts
        .iter()
        .rev()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

pub(crate) const fn shelf_title(shelf: Shelf) -> &'static str {
    match shelf {
        Shelf::Safe => "Safe to clean",
        Shelf::Review => "Review",
        Shelf::Protected => "Protected",
        Shelf::Measuring => "Measuring",
    }
}

const fn shelf_hint(shelf: Shelf) -> &'static str {
    match shelf {
        Shelf::Safe => "passes every check",
        Shelf::Review => "loses more, or recent",
        Shelf::Protected => "not eligible",
        Shelf::Measuring => "not sorted yet",
    }
}

const fn lens_title(lens: Lens) -> &'static str {
    match lens {
        Lens::Agents => "Agents",
        Lens::Dev => "Dev",
        Lens::System => "System",
    }
}

pub(crate) fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn footer(app: &App, look: Look) -> Paragraph<'static> {
    let totals = app.totals();
    let first = vec![
        Span::raw(format!("found {}", bytes(totals.discovered))),
        Span::raw(look.dot()),
        Span::raw(format!("eligible {}", bytes(totals.eligible))),
        Span::raw(look.dot()),
        Span::raw(format!(
            "selected {} ({})",
            totals.selected_count,
            bytes(totals.selected)
        )),
        Span::raw(look.dot()),
        Span::raw(format!("frees {}", bytes(totals.reclaim))),
    ];
    let typing = |prompt: String, text: &str, hint: &'static str| {
        Text::from(vec![
            Span::raw(format!("{prompt}{}", escaped(text))),
            Span::styled("_", Style::new().add_modifier(Modifier::SLOW_BLINK)),
            Span::styled(hint, look.dim()),
        ])
    };
    let second = match (app.mode, &app.notice, &app.panel.typing) {
        (Mode::Filter, _, _) => typing(
            "/".to_owned(),
            &app.filter,
            "  enter keeps it, esc clears it",
        ),
        (Mode::Note(id), _, _) => {
            let what = app
                .row(id)
                .map_or_else(String::new, |row| escaped(&tail(&row.finding.path)));
            typing(
                format!("Keep {what}. Note (optional): "),
                &app.note,
                "  enter keeps, esc cancels",
            )
        }
        (Mode::Settings, _, Some(typed)) => typing(
            "Project root to add (absolute or ~/...): ".to_owned(),
            typed,
            "  enter adds, esc cancels",
        ),
        (_, Some(notice), _) => Text::styled(notice.clone(), look.fg(Color::Cyan)),
        (Mode::Settings, None, None) => Text::styled(
            "j/k move  h/l change  enter change or add  d remove  esc back",
            look.dim(),
        ),
        (_, None, _) => Text::styled(
            "j/k move  / filter  s select  c safe  p keep  , settings  ? help  q quit",
            look.dim(),
        ),
    };
    Paragraph::new(vec![Text::from(first), second])
}

fn detail(app: &App, look: Look, node: Node) -> Vec<Text<'static>> {
    match node {
        Node::Finding(id) => app
            .row(id)
            .map_or_else(Vec::new, |row| finding_detail(app, look, row)),
        Node::Shelf(shelf) | Node::Group(shelf, _) => shelf_detail(app, look, node, shelf),
        Node::Gaps => vec![
            heading("Not covered"),
            Text::from(
                "The scan couldn't read everything, so this report is partial. An empty bucket doesn't mean there is nothing to clean.",
            ),
        ],
        Node::Gap(index) => app.gaps.get(index).map_or_else(Vec::new, |gap| {
            let mut lines = vec![heading("Not covered")];
            if let Some(integration) = &gap.integration {
                lines.push(labelled("Integration", integration.clone()));
            }
            lines.push(labelled("What happened", escaped(&gap.message)));
            if gap.integration.is_none() {
                lines.push(Text::from(
                    "kipple couldn't see what they use, so an item in your home may be in use by one of them. Items outside your home count them and stay Protected.",
                ));
            }
            lines
        }),
    }
}

fn heading(text: &str) -> Text<'static> {
    Text::styled(text.to_owned(), Style::new().add_modifier(Modifier::BOLD))
}

fn labelled(label: &str, text: String) -> Text<'static> {
    Text::from(vec![
        Span::styled(
            format!("{label:<17} "),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::raw(text),
    ])
}

fn finding_detail(app: &App, look: Look, row: &Row) -> Vec<Text<'static>> {
    let finding = &row.finding;
    let text = app.catalog.rules.get(&finding.rule);
    let facts = human::facts(finding, &app.policy);
    let mut lines = vec![
        Text::from(vec![
            Span::styled(
                app.catalog.title(&finding.rule).to_owned(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw(look.dot()),
            verdict_span(look, row, app.policy.level_of(&row.finding.rule)),
        ]),
        Text::raw(escaped(&finding.path.display().to_string())),
    ];
    if let Some(text) = text {
        let what = if text.description.is_empty() {
            &text.title
        } else {
            &text.description
        };
        lines.push(labelled("What it is", escaped(what)));
        lines.push(labelled("Why it's here", escaped(&text.why)));
    }
    let level = app.policy.level_of(&row.finding.rule);
    lines.push(labelled(why_label(row.shelf()), why(row, level)));
    if let Some(text) = text {
        lines.push(labelled("What you lose", escaped(&text.loss)));
    }
    // How it comes back first, as the spec orders the pane. The verified loss is said
    // in the rule's own words above.
    let (back, rest): (Vec<_>, Vec<_>) = facts
        .into_iter()
        .filter(|(label, _)| *label != "You lose")
        .partition(|(label, _)| *label == "Comes back");
    for (_, value) in back {
        lines.push(labelled("How it comes back", value));
    }
    for (label, value) in rest {
        lines.push(labelled(label, value));
    }
    if let Some(size) = &row.size {
        lines.push(labelled("Size", human::size(Some(size))));
    }
    lines.push(labelled("Rule", finding.rule.0.clone()));
    for advice in app.advice.get(&finding.integration).into_iter().flatten() {
        lines.push(labelled("Advice", escaped(&advice.title)));
        lines.extend(
            advice
                .snippets
                .iter()
                .map(|s| Text::raw(format!("  {}", escaped(s)))),
        );
    }
    lines
}

fn verdict_span(look: Look, row: &Row, level: Level) -> Span<'static> {
    let text = row.verdict.as_ref().map_or_else(
        || "measuring".to_owned(),
        |verdict| {
            format!(
                "{} at {}",
                human::bucket(verdict.bucket()),
                title_case(level.name())
            )
        },
    );
    Span::styled(text, look.tint(row.shelf()).add_modifier(Modifier::BOLD))
}

const fn why_label(shelf: Shelf) -> &'static str {
    match shelf {
        Shelf::Safe => "Why it's safe",
        Shelf::Review => "Why review it",
        Shelf::Protected => "Why it's held",
        Shelf::Measuring => "Status",
    }
}

fn why(row: &Row, level: Level) -> String {
    match &row.verdict {
        None => "still being measured and checked".to_owned(),
        Some(Verdict::Safe) => format!(
            "passes every check at {}: recognised, comes back, untracked, no executable running inside, not recent",
            title_case(level.name())
        ),
        Some(verdict) => human::reasons(verdict),
    }
}

fn shelf_detail(app: &App, look: Look, node: Node, shelf: Shelf) -> Vec<Text<'static>> {
    let (count, total) = sum(app, node);
    let about = match shelf {
        Shelf::Safe => {
            "Passes every check at this level: kipple recognised it, knows how it comes back, nothing inside is tracked by git, no running program shows it in use, and it hasn't changed recently. c selects all of it."
        }
        Shelf::Review => {
            "Eligible, but it loses more than this level allows, costs a lot to bring back, or changed recently. Each row says which."
        }
        Shelf::Protected => {
            "Not eligible: blocked, unknown, report only or advice only. Each row says why. kipple never selects these."
        }
        Shelf::Measuring => {
            "Still being measured and checked. Nothing counts as Safe until kipple knows."
        }
    };
    vec![
        Text::styled(
            shelf_title(shelf),
            look.tint(shelf).add_modifier(Modifier::BOLD),
        ),
        Text::raw(format!("{}, {}", human::findings(count), bytes(total))),
        Text::from(""),
        Text::raw(about),
    ]
}

/// The settings panel in place of the tree and the detail pane. Every line says its
/// value in words, so the cursor and the current level read without colour.
fn render_settings(app: &App, look: Look, area: Rect, frame: &mut Frame<'_>) {
    let items = settings::items(app);
    let mut panel = PanelLines {
        focus: app.panel.focus(&items),
        lines: Vec::new(),
        at: 0,
    };
    settings_head(app, look, &mut panel);
    settings_lists(app, look, &mut panel);
    let block = Block::new()
        .borders(Borders::TOP)
        .border_set(look.border())
        .border_style(look.dim())
        .title(" Settings ");
    let rows = usize::from(area.height.saturating_sub(1));
    let first = offset(0, panel.at, rows, panel.lines.len());
    let shown: Vec<Text<'static>> = panel.lines.into_iter().skip(first).collect();
    frame.render_widget(Paragraph::new(shown).block(block), area);
}

/// The settings panel's lines as they are built, and which one the cursor is on.
struct PanelLines {
    focus: Option<Item>,
    lines: Vec<Text<'static>>,
    at: usize,
}

impl PanelLines {
    /// A line with `label` in the first column; reversed when it is `item` and focused.
    fn push(&mut self, item: Option<&Item>, label: &str, value: Vec<Span<'static>>) {
        let mut spans = vec![Span::styled(
            format!("{label:<17} "),
            Style::new().add_modifier(Modifier::BOLD),
        )];
        spans.extend(value);
        let mut line = Text::from(spans);
        if item.is_some() && item == self.focus.as_ref() {
            line = line.style(Style::new().add_modifier(Modifier::REVERSED));
            self.at = self.lines.len();
        }
        self.lines.push(line);
    }
}

/// The config file, the level, the project roots and the method.
fn settings_head(app: &App, look: Look, panel: &mut PanelLines) {
    let file = app.file.as_ref().map_or_else(
        || "unknown, so nothing can be saved".to_owned(),
        |file| escaped(&file.display().to_string()),
    );
    panel.push(None, "Config file", vec![Span::styled(file, look.dim())]);
    panel.push(None, "", Vec::new());
    panel.push(Some(&Item::Level), "Level", level_spans(app, look));
    let about = Span::styled(level_about(app.policy.level), look.dim());
    panel.push(None, "", vec![about]);
    for (index, root) in app.roots.iter().enumerate() {
        let label = if index == 0 { "Project roots" } else { "" };
        let root = escaped(&root.display().to_string());
        panel.push(Some(&Item::Root(index)), label, vec![Span::raw(root)]);
    }
    let add = if app.roots.is_empty() {
        "Project roots"
    } else {
        ""
    };
    panel.push(Some(&Item::AddRoot), add, vec![Span::raw("+ add another")]);
    let method = vec![
        Span::raw(settings::method(app).name()),
        Span::styled("  stored only: nothing is removed yet", look.dim()),
    ];
    panel.push(Some(&Item::Method), "Default method", method);
}

/// The kept paths and the rules turned off, each undone with `d`.
fn settings_lists(app: &App, look: Look, panel: &mut PanelLines) {
    for (index, (path, note)) in app.config.keep.iter().enumerate() {
        let label = if index == 0 { "Kept" } else { "" };
        let mut value = vec![Span::raw(escaped(path))];
        if let Some(note) = note {
            value.push(Span::styled(format!("  {}", escaped(note)), look.dim()));
        }
        panel.push(Some(&Item::Kept(index)), label, value);
    }
    if app.config.keep.is_empty() {
        let none = Span::styled("nothing yet: p keeps a finding", look.dim());
        panel.push(None, "Kept", vec![none]);
    }
    let disabled = settings::disabled(app);
    for (index, rule) in disabled.iter().enumerate() {
        let label = if index == 0 { "Rules turned off" } else { "" };
        let value = vec![
            Span::raw(app.catalog.title(rule).to_owned()),
            Span::styled(format!("  {rule}"), look.dim()),
        ];
        panel.push(Some(&Item::Disabled(rule.clone())), label, value);
    }
    if disabled.is_empty() {
        let none = Span::styled("none: P turns a finding's rule off", look.dim());
        panel.push(None, "Rules turned off", vec![none]);
    }
}

/// The three levels, the current one in brackets.
fn level_spans(app: &App, look: Look) -> Vec<Span<'static>> {
    Level::ALL
        .iter()
        .map(|&level| {
            let name = title_case(level.name());
            if level == app.policy.level {
                Span::styled(
                    format!("[{name}] "),
                    look.fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw(format!(" {name}  "))
            }
        })
        .collect()
}

/// What `level` puts in Safe (`docs/04-safety-model.md` §2a).
pub(crate) const fn level_about(level: Level) -> &'static str {
    match level {
        Level::Careful => "Safe: build output and regenerated caches; recent: 14 days",
        Level::Balanced => "Safe: rebuilt, reinstalled, re-downloaded; recent: 7 days",
        Level::Thorough => "Safe: Balanced plus what costs a rollback; recent: 2 days",
    }
}

fn render_help(look: Look, frame: &mut Frame<'_>) {
    let keys = [
        ("j k  ↓ ↑", "move"),
        ("h l  ← →", "fold and unfold a group"),
        ("g G", "first and last line"),
        ("/", "filter by name or path; enter keeps it, esc clears it"),
        ("tab", "show one lens: Agents, Dev, System, then all"),
        ("s", "select or unselect the finding"),
        ("a", "add the group's Safe and Review findings"),
        ("c", "select the whole Safe bucket"),
        ("p", "keep the finding for good, with an optional note"),
        ("P", "keep every finding of its rule: turn the rule off"),
        (",", "settings: level, project roots, method, what is kept"),
        ("enter", "show what is selected"),
        ("?", "this help"),
        ("q", "quit"),
    ];
    let mut lines: Vec<Text<'static>> = keys
        .iter()
        .map(|(key, what)| {
            let key = if look.ascii {
                key.replace('↓', "dn")
                    .replace('↑', "up")
                    .replace('←', "lt")
                    .replace('→', "rt")
            } else {
                (*key).to_owned()
            };
            Text::from(vec![
                Span::styled(
                    format!("{key:<10} "),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw(*what),
            ])
        })
        .collect();
    lines.push(Text::from(""));
    lines.push(Text::styled(
        "This version only finds and explains: selecting removes nothing.",
        look.dim(),
    ));
    let area = frame.area();
    // Full width when narrow, so nothing shows beside it.
    let width = if area.width < WIDE { area.width } else { 72 };
    let height = area.height.saturating_sub(2).min(19);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let block = Block::bordered().border_set(look.border()).title(" Keys ");
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}
