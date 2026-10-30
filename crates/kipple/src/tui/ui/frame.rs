//! The frame round the screen and the lines inside it, and the title line in its top
//! edge: the level, the scan, the roots, the sort order, and what narrows the tree,
//! each shortened or left out in turn when they don't all fit.

use kipple_core::ScanOutcome;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::{Block, Paragraph};

use super::{Look, cols};
use crate::human::{self, escaped, homed};
use crate::theme::Role;
use crate::tui::app::{App, Mode, Progress};

/// The frame on the edge of `area` and a rule across it at each row of `rules`. The
/// title goes on last, with [`title`], so a join never lands in its text.
pub(super) fn outline(look: Look<'_>, area: Rect, rules: &[u16], frame: &mut Frame<'_>) {
    let block = Block::bordered()
        .border_set(look.glyphs.border())
        .border_style(look.style(Role::Border));
    frame.render_widget(block, area);
    let lines = look.glyphs.lines();
    let style = look.style(Role::Border);
    let buffer = frame.buffer_mut();
    for &y in rules {
        for x in area.left()..area.right() {
            let symbol = if x == area.left() {
                lines.vertical_right
            } else if x + 1 == area.right() {
                lines.vertical_left
            } else {
                lines.horizontal
            };
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_symbol(symbol).set_style(style);
            }
        }
    }
}

/// A vertical line at column `x` from row `top` to just before `bottom`, joined to the
/// lines it meets at either end.
pub(super) fn vertical(look: Look<'_>, x: u16, top: u16, bottom: u16, buffer: &mut Buffer) {
    let lines = look.glyphs.lines();
    let style = look.style(Role::Border);
    for y in top..bottom {
        if let Some(cell) = buffer.cell_mut((x, y)) {
            cell.set_symbol(lines.vertical).set_style(style);
        }
    }
    if let Some(above) = top.checked_sub(1) {
        join(look, buffer, (x, above), Vertical::StartsBelow);
    }
    join(look, buffer, (x, bottom), Vertical::EndsOn);
}

/// Where a vertical line meets a horizontal one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vertical {
    /// It starts under it.
    StartsBelow,
    /// It ends on it.
    EndsOn,
}

/// Joins a vertical line to the horizontal one at `at`, where `vertical` says it meets it.
fn join(look: Look<'_>, buffer: &mut Buffer, at: (u16, u16), vertical: Vertical) {
    let below = vertical == Vertical::StartsBelow;
    let lines = look.glyphs.lines();
    let Some(cell) = buffer.cell_mut(at) else {
        return;
    };
    let symbol = cell.symbol();
    let joined = if symbol == lines.horizontal {
        if below {
            lines.horizontal_down
        } else {
            lines.horizontal_up
        }
    } else if symbol == lines.horizontal_down && !below || symbol == lines.horizontal_up && below {
        lines.cross
    } else {
        return;
    };
    cell.set_symbol(joined);
}

/// `title` in the top edge of `area`. Its cells start from the text role, not the
/// frame's, so what it says isn't drawn as part of the border.
pub(super) fn title(look: Look<'_>, title: Text<'static>, area: Rect, frame: &mut Frame<'_>) {
    let edge = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1);
    let wide = u16::try_from(title.width())
        .unwrap_or(u16::MAX)
        .min(edge.width);
    let text = look.style(Role::Text);
    let buffer = frame.buffer_mut();
    for x in edge.x..edge.x + wide {
        if let Some(cell) = buffer.cell_mut((x, edge.y)) {
            cell.reset();
            cell.set_style(text);
        }
    }
    frame.render_widget(Paragraph::new(title), edge);
}

/// The separator between the title line's items: the frame's own line.
fn separator(look: Look<'_>) -> Span<'static> {
    let line = look.glyphs.lines().horizontal;
    Span::styled(format!(" {line} "), look.style(Role::Border))
}

/// The home screen's title line, in at most `width` columns.
pub(super) fn header(app: &App, look: Look<'_>, width: u16) -> Text<'static> {
    let sep = separator(look);
    let width = usize::from(width);
    let mut items = Items::new(app, look);
    // While the line is too wide, each step shortens or drops one item: the scan time's
    // words, the sort order's and the partial warning's first, then the scan time, the
    // sort order, the lens, the filter and last the partial warning. What changes how
    // the tree reads has its room before the roots, and the filter's text is cut to
    // what is left before the filter goes.
    for step in [
        Step::Short(Item::State),
        Step::Short(Item::Order),
        Step::Short(Item::Partial),
        Step::Drop(Item::State),
        Step::Drop(Item::Order),
        Step::Drop(Item::Lens),
        Step::Drop(Item::Filter),
        Step::Drop(Item::Partial),
    ] {
        // Words and the scan time go to keep the filter whole; a signal goes only to
        // keep the filter's first character.
        let filter = if matches!(step, Step::Short(_) | Step::Drop(Item::State)) {
            FilterText::Whole
        } else {
            FilterText::Least
        };
        if items.width(sep.width()) + items.filter_width(look, sep.width(), filter) <= width {
            break;
        }
        items.apply(step);
    }
    let room = width.saturating_sub(items.width(sep.width()));
    let filter = items.filter.as_deref().map(|filter| {
        cut(
            look,
            filter,
            room.saturating_sub(sep.width() + cols(&filter_text(""))),
        )
    });
    let room = room.saturating_sub(
        filter
            .as_deref()
            .map_or(0, |filter| sep.width() + cols(&filter_text(filter))),
    );
    let mut spans = vec![
        Span::raw(" "),
        Span::styled("kipple", look.style(Role::Title)),
        sep.clone(),
        Span::styled(items.level.clone(), look.style(Role::LevelCurrent)),
    ];
    if let Some(state) = items.state() {
        spans.push(sep.clone());
        spans.push(Span::styled(state, look.style(Role::Muted)));
    }
    if let Some(roots) = roots(app, look, room.saturating_sub(sep.width())) {
        spans.push(sep.clone());
        spans.push(Span::raw(roots));
    }
    for span in items.tail(look, filter) {
        spans.push(sep.clone());
        spans.push(span);
    }
    spans.push(Span::raw(" "));
    Text::from(spans)
}

/// A part of the title line that can be shortened or dropped to fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    State,
    Order,
    Partial,
    Lens,
    Filter,
}

/// How much of the filter's text the title line shows.
#[derive(Debug, Clone, Copy)]
enum FilterText {
    /// All of it.
    Whole,
    /// Its first character and an ellipsis.
    Least,
}

#[derive(Debug, Clone, Copy)]
enum Step {
    Short(Item),
    Drop(Item),
}

/// The title line's items besides the roots, each shown while it is `Some`.
struct Items {
    level: String,
    /// The scan state, and the scan time alone when it finished: only that can go.
    state: Option<(String, Option<String>)>,
    order: Option<&'static str>,
    partial: Option<usize>,
    lens: Option<String>,
    /// The filter's text, cut to the room left.
    filter: Option<String>,
    short: Vec<Item>,
}

impl Items {
    fn new(app: &App, look: Look<'_>) -> Self {
        let state = match app.progress {
            Progress::Scanning => (look.glyphs.ellipsis(human::SCANNING), None),
            Progress::Done { outcome, .. } if outcome != ScanOutcome::Completed => {
                (human::outcome(outcome).to_owned(), None)
            }
            Progress::Done { elapsed, .. } => {
                let time = format!("{}.{} s", elapsed.as_secs(), elapsed.subsec_millis() / 100);
                (format!("scanned in {time}"), Some(time))
            }
        };
        Self {
            level: human::level(app.policy.level).to_owned(),
            state: Some(state),
            order: app.order.title(),
            partial: (!app.gaps.is_empty()).then_some(app.gaps.len()),
            lens: app.lens.map(|lens| format!("lens: {}", human::lens(lens))),
            filter: (!app.filter.is_empty() && app.mode != Mode::Filter)
                .then(|| escaped(&app.filter)),
            short: Vec::new(),
        }
    }

    fn apply(&mut self, step: Step) {
        match step {
            Step::Short(item) => self.short.push(item),
            // A scan still running, or stopped early, always says so.
            Step::Drop(Item::State) => {
                if matches!(self.state, Some((_, Some(_)))) {
                    self.state = None;
                }
            }
            Step::Drop(Item::Order) => self.order = None,
            Step::Drop(Item::Partial) => self.partial = None,
            Step::Drop(Item::Lens) => self.lens = None,
            Step::Drop(Item::Filter) => self.filter = None,
        }
    }

    fn state(&self) -> Option<String> {
        let (full, time) = self.state.as_ref()?;
        Some(match time {
            Some(time) if self.short.contains(&Item::State) => time.clone(),
            Some(_) | None => full.clone(),
        })
    }

    fn order(&self) -> Option<&'static str> {
        let order = self.order?;
        Some(match order {
            "by last change" if self.short.contains(&Item::Order) => "by change",
            order => order,
        })
    }

    fn partial(&self) -> Option<String> {
        let gaps = self.partial?;
        Some(if self.short.contains(&Item::Partial) {
            format!("partial: {gaps}")
        } else {
            format!("partial: {gaps} not covered")
        })
    }

    /// The columns the filter takes with its separator, showing as much of its text as
    /// `text` says.
    fn filter_width(&self, look: Look<'_>, sep: usize, text: FilterText) -> usize {
        self.filter.as_deref().map_or(0, |filter| {
            let shown = match text {
                FilterText::Least => cut(look, filter, 1),
                FilterText::Whole => filter.to_owned(),
            };
            sep + cols(&filter_text(&shown)).min(cols(&filter_text(filter)))
        })
    }

    /// The columns the line takes without the roots and the filter.
    fn width(&self, sep: usize) -> usize {
        let parts = [
            Some(cols(&self.level)),
            self.state().map(|state| cols(&state)),
            self.order().map(cols),
            self.partial().map(|partial| cols(&partial)),
            self.lens.as_deref().map(cols),
        ];
        let items: usize = parts.iter().flatten().map(|part| part + sep).sum();
        // ` kipple` and the space at the end.
        1 + cols("kipple") + items + 1
    }

    /// The items after the roots, in order: the sort order, the partial warning, the
    /// lens and `filter`.
    fn tail(&self, look: Look<'_>, filter: Option<String>) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        if let Some(order) = self.order() {
            spans.push(Span::raw(order));
        }
        if let Some(partial) = self.partial() {
            spans.push(Span::styled(partial, look.style(Role::Warning)));
        }
        if let Some(lens) = &self.lens {
            spans.push(Span::raw(lens.clone()));
        }
        if let Some(filter) = filter {
            spans.push(Span::raw(filter_text(&filter)));
        }
        spans
    }
}

fn filter_text(text: &str) -> String {
    format!("filter: {text}")
}

/// `text` in at most `room` columns: whole, or as much of its start as fits before an
/// ellipsis, and never less than its first character.
fn cut(look: Look<'_>, text: &str, room: usize) -> String {
    if cols(text) <= room {
        return text.to_owned();
    }
    let ellipsis = look.glyphs.ellipsis("");
    let mut kept = String::new();
    let mut used = cols(&ellipsis);
    for c in text.chars() {
        let wide = cols(c.encode_utf8(&mut [0; 4]));
        if used + wide > room && !kept.is_empty() {
            break;
        }
        used += wide;
        kept.push(c);
    }
    format!("{kept}{ellipsis}")
}

/// A title line of `parts` joined by the frame's line, for the screens beside the home:
/// the first in the title role, the last muted.
pub(super) fn plain(look: Look<'_>, parts: &[String]) -> Text<'static> {
    let sep = separator(look);
    let mut spans = vec![Span::raw(" ")];
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            spans.push(sep.clone());
        }
        let role = match index {
            0 => Role::Title,
            _ if index + 1 == parts.len() => Role::Muted,
            _ => Role::Text,
        };
        spans.push(Span::styled(part.clone(), look.style(role)));
    }
    spans.push(Span::raw(" "));
    Text::from(spans)
}

/// The project roots in at most `room` columns: the first one and how many more,
/// the first one shortened from the left, or only how many. `None` when not even that
/// fits.
fn roots(app: &App, look: Look<'_>, room: usize) -> Option<String> {
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
    let ellipsis = look.glyphs.ellipsis("");
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
