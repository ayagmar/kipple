//! The home screen's renderer: a pure function of the [`App`] state and the [`Look`]
//! into a ratatui frame (`docs/03-functional-spec.md` §5). A finding's state is always
//! a word; colour only repeats it.

use std::collections::BTreeSet;
use std::path::Path;

use kipple_core::{Lens, Level, ScanOutcome, Verdict};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::app::{App, Line, Mode, Node, Progress, ProjectId, Row, Shelf};
use super::settings::{self, Item};
use crate::human::{self, bytes, escaped, homed};

/// A terminal at least this wide shows the detail pane beside the tree; a narrower one
/// shows it below.
const WIDE: u16 = 100;
/// The size column, wide enough for `>= 1023.9 GiB`.
const SIZE: u16 = 13;
/// The size bar column from [`WIDE`] columns: a space, then the bar.
const BAR: u16 = 7;
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

    /// A bar `eighths` eighths of a cell long, padded to `cells` cells.
    fn bar(self, eighths: u64, cells: usize) -> String {
        const PARTS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
        let full = usize::try_from(eighths / 8).unwrap_or(cells).min(cells);
        let part = usize::try_from(eighths % 8).unwrap_or(0);
        let mut bar = if self.ascii {
            // Whole cells only, rounded to the nearest.
            "#".repeat((full + usize::from(part >= 4)).min(cells))
        } else {
            let mut bar = "█".repeat(full);
            if full < cells {
                bar.push_str(PARTS.get(part).copied().unwrap_or(""));
            }
            bar
        };
        let drawn = bar.chars().count();
        bar.push_str(&" ".repeat(cells.saturating_sub(drawn)));
        bar
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
        clashing: app.clashing(),
        scale: areas.wide.then(|| scale(app, &lines)),
    };
    frame.render_widget(header(app, look, areas.header.width), areas.header);
    frame.render_widget(footer(app, look), areas.footer);
    if app.mode == Mode::Settings {
        render_settings(app, look, areas.body, frame);
        return;
    }
    render_list(app, look, &tree, areas.list, frame);
    let focus = lines.get(tree.at).map(|line| line.node);
    render_detail(app, look, focus, &areas, frame);
    if app.mode == Mode::Help {
        render_help(look, frame);
    }
}

fn header(app: &App, look: Look, width: u16) -> Paragraph<'static> {
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
        let name = title_case(level.name());
        // The brackets mark the level without colour.
        spans.push(if level == app.policy.level {
            Span::styled(
                format!("[{name}]"),
                look.fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(name, look.dim())
        });
    }
    spans.push(Span::raw(look.dot()));
    spans.push(Span::raw(state));
    spans.push(Span::raw(look.dot()));
    // A sort order other than size changes how the list reads, and the header is the only
    // lasting sign of it, so it comes first whenever it fits whole. The roots, fixed
    // config also listed in settings, get what is left.
    let used: usize = spans.iter().map(Span::width).sum();
    let room = usize::from(width).saturating_sub(used);
    let order = app.order.title().filter(|order| cols(order) <= room);
    let reserved = order.map_or(0, |order| cols(order) + cols(look.dot()));
    let roots = roots(app, look, room.saturating_sub(reserved));
    if let Some(roots) = &roots {
        spans.push(Span::raw(roots.clone()));
    }
    if let Some(order) = order {
        if roots.is_some() {
            spans.push(Span::raw(look.dot()));
        }
        spans.push(Span::raw(order));
    }
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

/// The project roots in at most `room` columns: the first one and how many more,
/// the first one shortened from the left, or only how many. `None` when not even that
/// fits.
fn roots(app: &App, look: Look, room: usize) -> Option<String> {
    let fits = |text: String| (cols(&text) <= room).then_some(text);
    let Some((first, rest)) = app.roots.split_first() else {
        return fits("no project roots".to_owned());
    };
    let first = escaped(&homed(first, app.home.as_deref()));
    let more = if rest.is_empty() {
        String::new()
    } else {
        format!(" +{}", rest.len())
    };
    if let Some(full) = fits(format!("{first}{more}")) {
        return Some(full);
    }
    let ellipsis = look.ellipsis("");
    let keep = room.saturating_sub(cols(&more) + cols(&ellipsis));
    // Fewer than a few characters of a path say nothing; the count says more.
    if keep >= 4 {
        let mut taken = 0;
        let start = first
            .char_indices()
            .rev()
            .find_map(|(index, c)| {
                taken += cols(c.encode_utf8(&mut [0; 4]));
                (taken > keep).then_some(index + c.len_utf8())
            })
            .unwrap_or(0);
        let end = first.get(start..).unwrap_or_default();
        return fits(format!("{ellipsis}{end}{more}"));
    }
    fits(match app.roots.len() {
        1 => "1 root".to_owned(),
        count => format!("{count} roots"),
    })
}

/// How many columns `text` takes.
fn cols(text: &str) -> usize {
    Span::raw(text).width()
}

/// The visible tree and the cursor's line in it.
struct Tree<'a> {
    lines: &'a [Line],
    at: usize,
    /// The projects whose lines show their path from home.
    clashing: BTreeSet<ProjectId>,
    /// What a full size bar stands for, when there are bars.
    scale: Option<u64>,
}

fn render_list(app: &App, look: Look, tree: &Tree<'_>, area: Rect, frame: &mut Frame<'_>) {
    let (lines, at) = (tree.lines, tree.at);
    let rows = usize::from(area.height);
    let first = offset(app.scroll, at, rows, lines.len());
    let shown = lines.iter().enumerate().skip(first).take(rows);
    // Reserved even while nothing is measured, so no column moves as sizes stream in.
    let bar = if tree.scale.is_some() { BAR } else { 0 };
    for (y, (index, line)) in (area.y..).zip(shown) {
        let row = Rect::new(area.x, y, area.width, 1);
        let [label, size, bars, state] = Split::horizontal([
            Constraint::Fill(1),
            Constraint::Length(SIZE),
            Constraint::Length(bar),
            Constraint::Length(STATE),
        ])
        .areas(row);
        let cells = cells(app, look, tree, *line);
        let base = if index == at {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        frame.render_widget(Paragraph::new(cells.label).style(base), label);
        frame.render_widget(Paragraph::new(cells.size).style(base).right_aligned(), size);
        frame.render_widget(Paragraph::new(cells.bar).style(base), bars);
        frame.render_widget(Paragraph::new(cells.state).style(base), state);
    }
}

/// The largest size among the measured findings in `lines`: what a full bar stands for.
fn scale(app: &App, lines: &[Line]) -> u64 {
    lines
        .iter()
        .filter_map(|line| match line.node {
            Node::Finding(id) => app.row(id).filter(|row| measured(row)),
            Node::Shelf(_)
            | Node::Group(..)
            | Node::Project(..)
            | Node::Gaps
            | Node::Gap(_)
            | Node::Notice(_) => None,
        })
        .map(Row::bytes)
        .max()
        .unwrap_or(0)
}

/// Whether `row` has its bucket and its size, so it has a bar.
fn measured(row: &Row) -> bool {
    row.shelf() != Shelf::Measuring && row.size.is_some()
}

/// One tree line's columns.
struct Cells {
    label: Text<'static>,
    size: String,
    bar: Text<'static>,
    state: Text<'static>,
}

fn cells(app: &App, look: Look, tree: &Tree<'_>, line: Line) -> Cells {
    let indent = "  ".repeat(usize::from(line.depth));
    let fold = |node| look.fold(app.is_collapsed(node));
    match line.node {
        Node::Finding(id) => app.row(id).map_or_else(empty_cells, |row| {
            let label = finding_label(app, tree, row, line.depth);
            let mut cells = finding_cells(app, look, row, &format!("{indent}{label}"));
            if let Some(scale) = tree.scale.filter(|_| measured(row)) {
                cells.bar = Text::styled(bar(look, row.bytes(), scale), look.tint(row.shelf()));
            }
            cells
        }),
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
                bar: Text::from(""),
                state: Text::styled(format!(" {}", shelf_hint(shelf)), look.dim()),
            }
        }
        Node::Group(_, lens) => {
            heading_cells(app, line, &indent, fold(line.node), lens_title(lens))
        }
        Node::Project(_, _, id) => {
            let name = project_name(app, tree, id);
            heading_cells(app, line, &indent, fold(line.node), &name)
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
            bar: Text::from(""),
            state: Text::styled(" so this report is partial", look.dim()),
        },
        Node::Notice(index) => Cells {
            label: Text::from(vec![
                Span::raw(format!("  {indent}")),
                Span::styled("Note: ", Style::new().add_modifier(Modifier::BOLD)),
                Span::raw(
                    app.notices
                        .get(index)
                        .map_or_else(String::new, |notice| escaped(notice)),
                ),
            ]),
            size: String::new(),
            bar: Text::from(""),
            state: Text::from(""),
        },
        Node::Gap(index) => Cells {
            label: Text::raw(format!(
                "  {indent}{}",
                app.gaps
                    .get(index)
                    .map_or_else(String::new, |gap| escaped(&gap.message))
            )),
            size: String::new(),
            bar: Text::from(""),
            state: Text::from(""),
        },
    }
}

/// A group's or a project's line: its name, how many findings and their total.
fn heading_cells(app: &App, line: Line, indent: &str, fold: &str, name: &str) -> Cells {
    let (count, total) = sum(app, line.node);
    Cells {
        label: Text::from(vec![
            Span::raw(format!("  {indent}{fold}")),
            Span::raw(format!("{} ({count})", escaped(name))),
        ]),
        size: bytes(total),
        bar: Text::from(""),
        state: Text::from(""),
    }
}

/// A project's name, or its path from home when another project shown shares it.
fn project_name(app: &App, tree: &Tree<'_>, id: ProjectId) -> String {
    app.project(id).map_or_else(String::new, |project| {
        if !tree.clashing.contains(&id) {
            return project.name.clone();
        }
        let home = app.home.as_deref();
        home.and_then(|home| project.path.strip_prefix(home).ok())
            .map_or_else(
                || project.path.display().to_string(),
                |rest| format!("~/{}", slashed(rest)),
            )
    })
}

/// A finding's label: its path inside its project under the project's line, else the
/// project and that path, else its last two components.
fn finding_label(app: &App, tree: &Tree<'_>, row: &Row, depth: u8) -> String {
    let spot = &row.spot;
    match spot.project {
        Some(_) if depth > 2 => spot.inside.clone(),
        Some(id) if tree.clashing.contains(&id) => {
            format!("{}/{}", project_name(app, tree, id), spot.inside)
        }
        Some(_) | None => spot.name.clone(),
    }
}

/// A bar for `bytes` out of `scale`, in `look`'s symbols, as wide as the column.
fn bar(look: Look, bytes: u64, scale: u64) -> String {
    let cells = usize::from(BAR - 1);
    let eighths = u128::from(bytes) * (cells as u128 * 8) / u128::from(scale.max(1));
    format!(
        " {}",
        look.bar(u64::try_from(eighths).unwrap_or(u64::MAX), cells)
    )
}

fn finding_cells(app: &App, look: Look, row: &Row, label: &str) -> Cells {
    let selected = app.selected.contains(&row.finding.id);
    let mark = if selected {
        Span::styled(format!("{} ", look.mark()), look.fg(Color::Cyan))
    } else {
        Span::raw("  ")
    };
    Cells {
        label: Text::from(vec![
            mark,
            Span::raw(escaped(label)),
            Span::styled(
                format!("  {}", app.catalog.title(&row.finding.rule)),
                look.dim(),
            ),
        ]),
        size: row
            .size
            .as_ref()
            .map_or_else(String::new, |size| human::size(Some(size))),
        bar: Text::from(""),
        state: Text::styled(format!(" {}", state(row)), look.tint(row.shelf())),
    }
}

fn empty_cells() -> Cells {
    Cells {
        label: Text::from(""),
        size: String::new(),
        bar: Text::from(""),
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
/// `web/node_modules`. The label of a finding outside every project root.
pub(crate) fn tail(path: &Path) -> String {
    let parts: Vec<_> = path.components().rev().take(2).collect();
    let parts: Vec<String> = parts
        .iter()
        .rev()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

/// `path`'s components with `/` between them, as the tree writes paths on every system.
pub(crate) fn slashed(path: &Path) -> String {
    let parts: Vec<_> = path
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
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

/// The detail pane's border: on its left beside the tree, on top below it.
fn detail_block(look: Look, wide: bool) -> Block<'static> {
    Block::new()
        .borders(if wide { Borders::LEFT } else { Borders::TOP })
        .border_set(look.border())
        .border_style(look.dim())
}

/// The detail pane for `focus`, from the line `App::detail_first` says. When more
/// follows than fits, its last line says so.
fn render_detail(app: &App, look: Look, focus: Option<Node>, areas: &Areas, frame: &mut Frame<'_>) {
    let block = detail_block(look, areas.wide);
    let inner = block.inner(areas.detail);
    frame.render_widget(block, areas.detail);
    let rows = focus.map_or_else(Vec::new, |node| wrap(detail(app, look, node), inner.width));
    let height = usize::from(inner.height);
    let scrolled = focus.map_or(0, |node| app.detail_first(node));
    let first = scrolled.min(rows.len().saturating_sub(height));
    let more = rows.len() > first + height;
    let take = if more {
        height.saturating_sub(1)
    } else {
        height
    };
    let mut shown: Vec<Text<'static>> = rows.into_iter().skip(first).take(take).collect();
    if more {
        shown.push(Text::styled(
            format!("{} J/K scroll", look.ellipsis("more")),
            look.dim(),
        ));
    }
    frame.render_widget(Paragraph::new(shown), inner);
}

/// The last line the detail pane for `node` can scroll to on `app`'s screen: none
/// when it all fits.
pub(crate) fn detail_end(app: &App, node: Node) -> usize {
    let areas = areas(Rect::new(0, 0, app.width, app.height));
    // Both looks draw symbols of the same width, so either wraps the same.
    let look = Look {
        color: false,
        ascii: true,
    };
    let inner = detail_block(look, areas.wide).inner(areas.detail);
    let rows = wrap(detail(app, look, node), inner.width).len();
    rows.saturating_sub(usize::from(inner.height))
}

/// `lines` broken into rows of at most `width` columns: between words where it can, a
/// word longer than a row wherever it must. A break swallows the space it falls on.
fn wrap(lines: Vec<Text<'static>>, width: u16) -> Vec<Text<'static>> {
    let width = usize::from(width);
    if width == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for line in lines {
        let mut filling = Filling::new(line.style, width);
        for (word, space) in words(&line) {
            filling.word(&word);
            filling.space(&space);
        }
        rows.extend(filling.finish());
    }
    rows
}

/// A styled character.
type Cell = (char, Style);

/// `line`'s characters with their styles, as words each followed by the spaces after
/// it. Leading spaces come as a first, empty word's.
fn words(line: &Text<'_>) -> Vec<(Vec<Cell>, Vec<Cell>)> {
    let mut words: Vec<(Vec<Cell>, Vec<Cell>)> = vec![(Vec::new(), Vec::new())];
    for span in &line.spans {
        for c in span.content.chars() {
            let style = span.style;
            let Some(last) = words.last_mut() else {
                continue;
            };
            if c == ' ' {
                last.1.push((c, style));
            } else if last.1.is_empty() {
                last.0.push((c, style));
            } else {
                words.push((vec![(c, style)], Vec::new()));
            }
        }
    }
    words
}

fn char_cols(c: char) -> usize {
    cols(c.encode_utf8(&mut [0; 4]))
}

/// One line of the detail pane being broken into rows.
struct Filling {
    width: usize,
    style: Style,
    rows: Vec<Text<'static>>,
    spans: Vec<Span<'static>>,
    cols: usize,
}

impl Filling {
    const fn new(style: Style, width: usize) -> Self {
        Self {
            width,
            style,
            rows: Vec::new(),
            spans: Vec::new(),
            cols: 0,
        }
    }

    /// Adds `word` on this row, or on the next when it fits there whole.
    fn word(&mut self, word: &[Cell]) {
        let wide: usize = word.iter().map(|&(c, _)| char_cols(c)).sum();
        if self.cols + wide > self.width && wide <= self.width {
            self.next_row();
        }
        for &(c, style) in word {
            if self.cols + char_cols(c) > self.width {
                self.next_row();
            }
            self.push(c, style);
        }
    }

    /// Adds `space` up to the end of the row, where it breaks and the rest is dropped.
    fn space(&mut self, space: &[Cell]) {
        for &(c, style) in space {
            if self.cols + char_cols(c) > self.width {
                self.next_row();
                return;
            }
            self.push(c, style);
        }
    }

    fn push(&mut self, c: char, style: Style) {
        self.cols += char_cols(c);
        match self.spans.last_mut() {
            Some(span) if span.style == style => span.content.to_mut().push(c),
            _ => self.spans.push(Span::styled(c.to_string(), style)),
        }
    }

    /// Ends the row so far, unless nothing is on it yet.
    fn next_row(&mut self) {
        if self.cols == 0 {
            return;
        }
        self.cols = 0;
        let spans = std::mem::take(&mut self.spans);
        self.rows.push(Text::from(spans).style(self.style));
    }

    /// The rows, at least one: an empty line stays a row, and a break on the last
    /// space doesn't add one.
    fn finish(mut self) -> Vec<Text<'static>> {
        self.next_row();
        if self.rows.is_empty() {
            self.rows.push(Text::default().style(self.style));
        }
        self.rows
    }
}

fn detail(app: &App, look: Look, node: Node) -> Vec<Text<'static>> {
    match node {
        Node::Finding(id) => app
            .row(id)
            .map_or_else(Vec::new, |row| finding_detail(app, look, row)),
        Node::Project(_, _, id) => project_detail(app, look, id),
        Node::Shelf(shelf) | Node::Group(shelf, _) => shelf_detail(app, look, node, shelf),
        Node::Gaps => vec![
            heading("Not covered"),
            Text::from(
                "The scan couldn't read everything, so this report is partial. An empty bucket doesn't mean there is nothing to clean.",
            ),
        ],
        Node::Gap(index) => app.gaps.get(index).map_or_else(Vec::new, |gap| {
            vec![
                heading("Not covered"),
                labelled("Integration", gap.integration.clone()),
                labelled("What happened", escaped(&gap.message)),
            ]
        }),
        Node::Notice(index) => app.notices.get(index).map_or_else(Vec::new, |notice| {
            vec![
                heading("Note"),
                Text::from(escaped(notice)),
                Text::styled(
                    "It doesn't make the scan partial: it says how far the check for running programs went.",
                    look.dim(),
                ),
            ]
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
                .flat_map(|s| human::snippet_lines(s))
                .map(|line| Text::raw(format!("  {line}"))),
        );
    }
    lines
}

/// A project's line: its path, then its findings in each bucket with their totals,
/// as the filter and lens show them.
fn project_detail(app: &App, look: Look, id: ProjectId) -> Vec<Text<'static>> {
    let Some(project) = app.project(id) else {
        return Vec::new();
    };
    let mut lines = vec![
        heading(&format!("Project {}", escaped(&project.name))),
        Text::raw(escaped(&project.path.display().to_string())),
    ];
    for shelf in [
        Shelf::Safe,
        Shelf::Review,
        Shelf::Protected,
        Shelf::Measuring,
    ] {
        let mut rows: Vec<&Row> = [Lens::Agents, Lens::Dev, Lens::System]
            .into_iter()
            .flat_map(|lens| app.under(Node::Project(shelf, lens, id)))
            .collect();
        if rows.is_empty() {
            continue;
        }
        rows.sort_by(|a, b| a.spot.inside.cmp(&b.spot.inside));
        let total: u64 = rows.iter().map(|row| row.bytes()).sum();
        lines.push(Text::from(""));
        lines.push(Text::styled(
            format!(
                "{}: {}, {}",
                shelf_title(shelf),
                human::findings(rows.len()),
                bytes(total)
            ),
            look.tint(shelf).add_modifier(Modifier::BOLD),
        ));
        lines.extend(rows.iter().map(|row| {
            Text::raw(format!(
                "  {}  {}",
                escaped(&row.spot.inside),
                human::size(row.size.as_ref())
            ))
        }));
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
            "passes every check at {}: recognised, comes back, untracked, no running program shows it in use, not recent",
            title_case(level.name())
        ),
        Some(verdict) => human::reasons_in_full(verdict),
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
    for (index, suggestion) in app.search.suggestions().iter().enumerate() {
        let label = if index == 0 { "Suggested roots" } else { "" };
        let line = vec![Span::raw(settings::describe(suggestion, look))];
        panel.push(Some(&Item::Suggested(index)), label, line);
    }
    for (index, note) in settings::search_notes(&app.search, look)
        .into_iter()
        .enumerate()
    {
        let label = if index == 0 && app.search.suggestions().is_empty() {
            "Suggested roots"
        } else {
            ""
        };
        panel.push(None, label, vec![Span::styled(note, look.dim())]);
    }
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
        ("h l  ← →", "fold and unfold a group or project"),
        ("g G", "first and last line"),
        (
            "/",
            "filter by words of the path or rule; enter keeps it, esc clears it",
        ),
        ("tab", "show one lens: Agents, Dev, System, then all"),
        ("s", "select or unselect the finding"),
        ("a", "add the group's or project's Safe and Review findings"),
        ("c", "select the whole Safe bucket"),
        ("p", "keep the finding for good, with an optional note"),
        ("P", "keep every finding of its rule: turn the rule off"),
        ("o", "sort by size, last change or name"),
        ("J K", "scroll the detail pane"),
        ("y", "send the path to the terminal clipboard"),
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
    let height = area.height.saturating_sub(2).min(22);
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
