//! The tree: buckets, lenses, projects and findings, one line each, with their sizes,
//! bars and states.

use std::collections::BTreeSet;
use std::path::Path;

use kipple_core::{Hold, Verdict};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::Paragraph;

use super::layout::Bars;
use super::{Look, drawn_bar, tint};
use crate::human::{self, bytes, bytes_short, escaped};
use crate::json::SizeOut;
use crate::theme::{Fold, Role};
use crate::tui::app::{App, Line, Node, ProjectId, Row, Shelf};

/// The size column's least width, enough for `1023.9 GiB`: a lower bound (`>= `)
/// widens it.
const SIZE: usize = 10;
/// The size column's least width when sizes are written short: `1023G`.
const SHORT: usize = 5;
/// The size bar column, when there are bars: a space, then the bar.
const BAR: usize = 7;

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
pub(super) struct Tree<'lines> {
    lines: &'lines [Line],
    at: usize,
    /// The projects whose lines show their path from home.
    clashing: BTreeSet<ProjectId>,
    /// What a full size bar stands for, when there are bars.
    scale: Option<u64>,
}

impl<'a> Tree<'a> {
    /// `lines` of `app`, with size bars as `bars` says.
    pub(super) fn new(app: &App, lines: &'a [Line], bars: Bars) -> Self {
        Self {
            at: app.cursor_index(lines),
            lines,
            clashing: app.clashing(),
            scale: (bars == Bars::Shown).then(|| scale(app, lines)),
        }
    }

    /// The node of the cursor's line.
    pub(super) fn focus(&self) -> Option<Node> {
        self.lines.get(self.at).map(|line| line.node)
    }
}

pub(super) fn render(
    app: &App,
    look: Look<'_>,
    tree: &Tree<'_>,
    area: Rect,
    frame: &mut Frame<'_>,
) {
    let (lines, at) = (tree.lines, tree.at);
    let rows = usize::from(area.height);
    let first = offset(app.scroll, at, rows, lines.len());
    let shown: Vec<(usize, Cells)> = lines
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(index, line)| (index, cells(app, look, tree, *line)))
        .collect();
    let sized: Vec<&Cells> = shown
        .iter()
        .map(|(_, cells)| cells)
        .filter(|cells| !cells.sentence())
        .collect();
    let columns = Columns::new(look, &sized, area.width, tree.scale);
    for (y, (index, cells)) in (area.y..).zip(shown) {
        let row = Rect::new(area.x, y, area.width, 1);
        let [label, size, bar, state] = if cells.sentence() {
            [row, Rect::default(), Rect::default(), Rect::default()]
        } else {
            columns.split(row, &cells)
        };
        let focused = index == at;
        // The cursor's style fills the whole row and wins over each span's own.
        let base = if focused {
            look.style(Role::Cursor)
        } else {
            Style::new()
        };
        let paint = |text: Text<'static>| if focused { look.cursor(text) } else { text };
        let text = fitted(look, &cells, usize::from(label.width));
        frame.render_widget(Paragraph::new(paint(text)).style(base), label);
        let shown_size = cells.size.text(columns.form);
        frame.render_widget(Paragraph::new(shown_size).style(base).right_aligned(), size);
        if bar.width > 0 {
            let drawn = Text::from(cells.bar);
            frame.render_widget(Paragraph::new(paint(drawn)).style(base), bar);
        }
        // A heading's hint shows whole or not at all; a finding's state always shows.
        let state_text = if cells.finding || cells.state.width() <= usize::from(state.width) {
            cells.state
        } else {
            Text::from("")
        };
        frame.render_widget(Paragraph::new(paint(state_text)).style(base), state);
    }
}

/// How wide each column of the lines shown is; the state column takes the rest. The
/// label has its room first: the state column is only as wide as the states shown
/// need, and a heading's hint, then the bars, then the sizes' long form give way
/// before a path is cut, which is never cut below its last component.
struct Columns {
    label: u16,
    size: u16,
    bar: u16,
    /// How sizes are written.
    form: Form,
}

impl Columns {
    /// The columns for `shown` in `width`, with room for a bar when there is a `scale`.
    fn new(look: Look<'_>, shown: &[&Cells], width: u16, scale: Option<u64>) -> Self {
        let width = usize::from(width);
        let widest = |of: &dyn Fn(&Cells) -> Option<usize>| {
            shown
                .iter()
                .filter_map(|&cells| of(cells))
                .max()
                .unwrap_or(0)
        };
        // A space before the size, so it never touches the label.
        let sizes = |form: Form| {
            let least = match form {
                Form::Short => SHORT,
                Form::Full => SIZE,
            };
            widest(&|cells| Some(super::cols(&cells.size.text(form)))).max(least) + 1
        };
        let mut size = sizes(Form::Full);
        let states = widest(&|cells| cells.finding.then(|| cells.state.width()));
        let hints = widest(&|cells| (!cells.finding).then(|| cells.state.width()));
        let need = widest(&|cells| Some(lead_width(cells) + super::cols(&cells.name.content)));
        let floor = widest(&|cells| Some(lead_width(cells) + least(look, cells)));
        let mut bar = if scale.is_some() { BAR } else { 0 };
        let mut state = states.max(hints.saturating_sub(bar));
        let over = |columns: [usize; 3]| need + columns.iter().sum::<usize>() > width;
        if over([size, bar, state]) {
            state = states;
        }
        if over([size, bar, state]) {
            bar = 0;
        }
        let form = if over([size, bar, state]) {
            Form::Short
        } else {
            Form::Full
        };
        if form == Form::Short {
            size = sizes(Form::Short).min(size);
        }
        // A finding's state is its hold's only label (never colour alone), so it keeps
        // its room and the path gives way below its floor first.
        let room = width.saturating_sub(states);
        let label = width
            .saturating_sub(size + bar + state)
            .max(floor.min(room));
        let size = size.min(room.saturating_sub(label));
        let bar = bar.min(room.saturating_sub(label + size));
        let narrow = |value: usize| u16::try_from(value).unwrap_or(u16::MAX);
        Self {
            label: narrow(label),
            size: narrow(size),
            bar: narrow(bar),
            form,
        }
    }

    /// `row` as its label, size, bar and state; a heading has no bar, so its state
    /// starts where the bar would.
    fn split(&self, row: Rect, cells: &Cells) -> [Rect; 4] {
        let at = |x: u16, width: u16| Rect::new(row.x + x, row.y, width, 1);
        let bar = if cells.finding { self.bar } else { 0 };
        let state = self.label + self.size + bar;
        [
            at(0, self.label),
            at(self.label, self.size),
            at(self.label + self.size, bar),
            at(state, row.width.saturating_sub(state)),
        ]
    }
}

/// How the tree writes sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Full,
    /// Short (`512M`), when the columns don't fit in full.
    Short,
}

/// A line's size, as the tree writes it: in full, or short (`512M`) when the columns
/// don't fit.
enum Size {
    None,
    /// A heading's total.
    Total(u64),
    /// A finding's size, which may be a lower bound.
    Finding(SizeOut),
}

impl Size {
    fn text(&self, form: Form) -> String {
        match (self, form) {
            (Self::None, _) => String::new(),
            (Self::Total(total), Form::Full) => bytes(*total),
            (Self::Total(total), Form::Short) => bytes_short(*total),
            (Self::Finding(size), Form::Full) => human::size(Some(size)),
            (Self::Finding(size), Form::Short) => human::size_short(Some(size)),
        }
    }
}

fn lead_width(cells: &Cells) -> usize {
    cells.lead.iter().map(Span::width).sum()
}

/// The least of a line's name that is shown: a path's last component after an
/// ellipsis, or an ellipsis alone.
fn least(look: Look<'_>, cells: &Cells) -> usize {
    let name = &cells.name.content;
    let ellipsis = look.glyphs.ellipsis("");
    let least = if cells.path {
        name.rsplit_once('/').map_or_else(
            || name.to_string(),
            |(_, last)| format!("{ellipsis}/{last}"),
        )
    } else {
        ellipsis
    };
    super::cols(&least).min(super::cols(name))
}

/// A line's label in `width` columns: its lead, its name, cut when it must be (a path
/// from its start, keeping its last component; other names at a word's end), and a
/// finding's rule title in what is left, cut where a word ends, or left out.
fn fitted(look: Look<'_>, cells: &Cells, width: usize) -> Text<'static> {
    let room = width.saturating_sub(lead_width(cells));
    let name = &cells.name;
    let shown = if cells.path {
        path_in(look, &name.content, room)
    } else {
        words_in(look, &name.content, room).unwrap_or_else(|| look.glyphs.ellipsis(""))
    };
    let left = room.saturating_sub(super::cols(&shown));
    let mut spans = cells.lead.clone();
    spans.push(Span::styled(shown, name.style));
    // A title shows from its first word on, or not at all.
    let first = |title: &str| {
        let word = title.split(' ').next().unwrap_or_default();
        2 + super::cols(word) + super::cols(&look.glyphs.ellipsis(""))
    };
    if let Some(title) = cells
        .title
        .as_ref()
        .filter(|title| left >= first(&title.content))
    {
        let text = format!("  {}", title.content);
        if let Some(text) = words_in(look, &text, left) {
            spans.push(Span::styled(text, title.style));
        }
    }
    Text::from(spans)
}

/// `path` in `room` columns: whole, or as much of its end as fits after an ellipsis,
/// starting at a component unless most of one fits, and never less than its last
/// component.
fn path_in(look: Look<'_>, path: &str, room: usize) -> String {
    let Some((_, last)) = path.rsplit_once('/') else {
        return path.to_owned();
    };
    if super::cols(path) <= room {
        return path.to_owned();
    }
    let ellipsis = look.glyphs.ellipsis("");
    let keep = room.saturating_sub(super::cols(&ellipsis));
    let mut start = path.len();
    let mut used = 0;
    for (index, c) in path.char_indices().rev() {
        used += super::cols(c.encode_utf8(&mut [0; 4]));
        if used > keep {
            break;
        }
        start = index;
    }
    let mut end = path.get(start..).unwrap_or_default();
    // A few letters of a component say nothing: it goes whole.
    if let Some(at) = end
        .find('/')
        .filter(|&at| end.get(..at).is_some_and(|part| part.chars().count() < 4))
    {
        end = end.get(at..).unwrap_or(end);
    }
    if end.len() <= last.len() {
        format!("{ellipsis}/{last}")
    } else {
        format!("{ellipsis}{end}")
    }
}

/// `text` in `room` columns: whole, or cut where a word ends (else where it must) and
/// ended with an ellipsis, so a cut label never reads as whole. `None` when not even
/// an ellipsis fits.
fn words_in(look: Look<'_>, text: &str, room: usize) -> Option<String> {
    if super::cols(text) <= room {
        return Some(text.to_owned());
    }
    let ellipsis = look.glyphs.ellipsis("");
    let room = room.checked_sub(super::cols(&ellipsis))?;
    let mut kept = String::new();
    let mut used = 0;
    let mut cut_inside = false;
    for c in text.chars() {
        let wide = super::cols(c.encode_utf8(&mut [0; 4]));
        if used + wide > room {
            cut_inside = c != ' ';
            break;
        }
        used += wide;
        kept.push(c);
    }
    // Words are cut where one ends, unless that leaves nothing of the text.
    if cut_inside
        && let Some(space) = kept.trim_end().rfind(' ')
        && kept
            .get(..space)
            .is_some_and(|before| !before.trim().is_empty())
    {
        kept.truncate(space);
    }
    Some(format!("{}{ellipsis}", kept.trim_end()))
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
    /// The selection mark, the indent and the fold: never cut.
    lead: Vec<Span<'static>>,
    name: Span<'static>,
    /// Whether `name` is a path, which is cut from its start.
    path: bool,
    /// A finding's rule title, the first to go when there isn't room.
    title: Option<Span<'static>>,
    size: Size,
    bar: Vec<Span<'static>>,
    state: Text<'static>,
    /// Whether it is a finding's line: a heading has no bar, so its state has that room
    /// too.
    finding: bool,
}

impl Cells {
    /// Whether it is a line of words alone, with no size or state: a note or what the
    /// scan couldn't read, which has the whole row.
    fn sentence(&self) -> bool {
        !self.finding && matches!(self.size, Size::None) && self.state.width() == 0
    }

    /// A heading's line: `lead`, then `name`.
    fn heading(lead: String, name: Span<'static>) -> Self {
        Self {
            lead: vec![Span::raw(lead)],
            name,
            path: false,
            title: None,
            size: Size::None,
            bar: Vec::new(),
            state: Text::from(""),
            finding: false,
        }
    }
}

fn cells(app: &App, look: Look<'_>, tree: &Tree<'_>, line: Line) -> Cells {
    let indent = "  ".repeat(usize::from(line.depth));
    let fold = |node| {
        let fold = if app.is_collapsed(node) {
            Fold::Collapsed
        } else {
            Fold::Open
        };
        look.glyphs.fold(fold)
    };
    let lead = |node| format!("  {indent}{}", fold(node));
    match line.node {
        Node::Finding(id) => app.row(id).map_or_else(
            || Cells::heading(String::new(), Span::raw("")),
            |row| {
                let label = finding_label(app, tree, row, line.depth);
                let mut cells = finding_cells(app, look, row, &indent, &label);
                if let Some(scale) = tree.scale.filter(|_| measured(row)) {
                    cells.bar = bar(look, row, scale);
                }
                cells
            },
        ),
        Node::Shelf(shelf) => {
            let Sum {
                count,
                bytes: total,
            } = sum(app, line.node);
            let standing = human::standing(shelf.bucket());
            let name = Span::styled(
                format!("{} ({count})", standing.title),
                look.style(tint(shelf)).add_modifier(Modifier::BOLD),
            );
            Cells {
                size: Size::Total(total),
                state: Text::styled(format!(" {}", standing.hint), look.style(Role::Muted)),
                ..Cells::heading(lead(line.node), name)
            }
        }
        Node::Group(_, lens) => heading_cells(app, line, lead(line.node), human::lens(lens)),
        Node::Project(_, _, id) => {
            let name = project_name(app, tree, id);
            heading_cells(app, line, lead(line.node), &name)
        }
        Node::Gaps => {
            let name = Span::styled(
                format!("{} ({})", human::NOT_COVERED, app.gaps.len()),
                look.style(Role::Warning).add_modifier(Modifier::BOLD),
            );
            Cells {
                state: Text::styled(format!(" {}", human::PARTIAL), look.style(Role::Muted)),
                ..Cells::heading(lead(line.node), name)
            }
        }
        Node::Notice(index) => {
            let notice = app
                .notices
                .get(index)
                .map_or_else(String::new, |notice| escaped(notice));
            let mut cells = Cells::heading(format!("  {indent}"), Span::raw(notice));
            cells
                .lead
                .push(Span::styled("Note: ", look.style(Role::Label)));
            cells
        }
        Node::Gap(index) => {
            let gap = app
                .gaps
                .get(index)
                .map_or_else(String::new, |gap| escaped(&gap.message));
            Cells::heading(format!("  {indent}"), Span::raw(gap))
        }
    }
}

/// A group's or a project's line: its name, how many findings and their total.
fn heading_cells(app: &App, line: Line, lead: String, name: &str) -> Cells {
    let Sum {
        count,
        bytes: total,
    } = sum(app, line.node);
    Cells {
        size: Size::Total(total),
        ..Cells::heading(lead, Span::raw(format!("{} ({count})", escaped(name))))
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

/// A bar for `row` out of `scale`, as wide as the column.
fn bar(look: Look<'_>, row: &Row, scale: u64) -> Vec<Span<'static>> {
    let cells = BAR - 1;
    let eighths = u128::from(row.bytes()) * (cells as u128 * 8) / u128::from(scale.max(1));
    let mut spans = vec![Span::raw(" ")];
    spans.extend(drawn_bar(look, row.shelf(), eighths, cells));
    spans
}

fn finding_cells(app: &App, look: Look<'_>, row: &Row, indent: &str, label: &str) -> Cells {
    let selected = app.selected.contains(&row.finding.id);
    let mark = if selected {
        Span::styled(
            format!("{} ", look.glyphs.mark()),
            look.style(Role::Selected),
        )
    } else {
        Span::raw("  ")
    };
    // What the user chose to keep reads as theirs, whatever else holds it.
    let kept =
        matches!(&row.verdict, Some(Verdict::Protected(holds)) if holds.contains(&Hold::Kept));
    let role = if kept { Role::Kept } else { tint(row.shelf()) };
    Cells {
        lead: vec![mark, Span::raw(indent.to_owned())],
        name: Span::raw(escaped(label)),
        path: true,
        title: Some(Span::styled(
            app.catalog.title(&row.finding.rule).to_owned(),
            look.style(Role::Muted),
        )),
        size: row.size.clone().map_or(Size::None, Size::Finding),
        bar: Vec::new(),
        state: Text::styled(format!(" {}", state(row)), look.style(role)),
        finding: true,
    }
}

/// A row's state word: the first reason it isn't Safe, and how many more there are.
fn state(row: &Row) -> String {
    let Some(verdict) = &row.verdict else {
        return human::MEASURING.to_owned();
    };
    match human::reason_labels(verdict).as_slice() {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first, rest @ ..] => format!("{first} +{}", rest.len()),
    }
}

/// How many findings are under a node, and their size in bytes.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Sum {
    pub(super) count: usize,
    pub(super) bytes: u64,
}

/// How many findings are under `node`, and their size.
pub(super) fn sum(app: &App, node: Node) -> Sum {
    app.under(node).fold(Sum::default(), |sum, row| Sum {
        count: sum.count + 1,
        bytes: sum.bytes + row.bytes(),
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
