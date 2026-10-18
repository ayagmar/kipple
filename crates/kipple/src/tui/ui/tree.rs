//! The tree: buckets, lenses, projects and findings, one line each, with their sizes,
//! bars and states.

use std::collections::BTreeSet;
use std::path::Path;

use kipple_core::Verdict;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::Paragraph;

use super::{Look, lens_title, shelf_title};
use crate::human::{self, bytes, escaped};
use crate::tui::app::{App, Line, Node, ProjectId, Row, Shelf};

/// The size column, wide enough for `>= 1023.9 GiB`.
const SIZE: u16 = 13;
/// The size bar column from [`WIDE`] columns: a space, then the bar.
const BAR: u16 = 7;
/// The state column, wide enough for its longest text: the gaps hint
/// ` so this report is partial`. The longest row state is ` unknown: what is lost +3`.
const STATE: u16 = 26;

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

/// The visible tree and the cursor's line in it.
pub(super) struct Tree<'a> {
    lines: &'a [Line],
    at: usize,
    /// The projects whose lines show their path from home.
    clashing: BTreeSet<ProjectId>,
    /// What a full size bar stands for, when there are bars.
    scale: Option<u64>,
}

impl<'a> Tree<'a> {
    /// `lines` of `app`, with bars when the screen is `wide`.
    pub(super) fn new(app: &App, lines: &'a [Line], wide: bool) -> Self {
        Self {
            at: app.cursor_index(lines),
            lines,
            clashing: app.clashing(),
            scale: wide.then(|| scale(app, lines)),
        }
    }

    /// The node of the cursor's line.
    pub(super) fn focus(&self) -> Option<Node> {
        self.lines.get(self.at).map(|line| line.node)
    }
}

pub(super) fn render(app: &App, look: Look, tree: &Tree<'_>, area: Rect, frame: &mut Frame<'_>) {
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
pub(super) fn sum(app: &App, node: Node) -> (usize, u64) {
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

const fn shelf_hint(shelf: Shelf) -> &'static str {
    match shelf {
        Shelf::Safe => "passes every check",
        Shelf::Review => "loses more, or recent",
        Shelf::Protected => "not eligible",
        Shelf::Measuring => "not sorted yet",
    }
}
