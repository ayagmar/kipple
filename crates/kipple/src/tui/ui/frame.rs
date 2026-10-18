//! The frame round the screen and the lines inside it, and the title line in its top
//! edge: the level, the scan, the roots, the sort order, and what narrows the tree.

use kipple_core::ScanOutcome;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::{Block, Paragraph};

use super::{Look, cols, lens_title, title_case};
use crate::human::{bytes, escaped, homed};
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
        join(look, buffer, (x, above), true);
    }
    join(look, buffer, (x, bottom), false);
}

/// Joins a vertical line to the horizontal one at `at`: `below` when the vertical line
/// starts under it, else when it ends on it.
fn join(look: Look<'_>, buffer: &mut Buffer, at: (u16, u16), below: bool) {
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

/// The home screen's title line, in at most `width` columns, with the found and
/// eligible totals when `totals` says nothing else shows them.
pub(super) fn header(app: &App, look: Look<'_>, width: u16, totals: bool) -> Text<'static> {
    let sep = separator(look);
    let state = match app.progress {
        Progress::Scanning => look.glyphs.ellipsis("scanning"),
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
    let level = title_case(app.policy.level.name());
    let head = vec![
        Span::raw(" "),
        Span::styled("kipple", look.style(Role::Title)),
        sep.clone(),
        Span::styled(level, look.style(Role::LevelCurrent)),
        sep.clone(),
        Span::styled(state, look.style(Role::Muted)),
    ];
    // What changes how the tree reads comes after the roots, but has its room first:
    // the roots, fixed config also listed in settings, get what is left.
    let mut tail = Vec::new();
    if totals {
        let sums = app.totals();
        tail.push(Span::raw(format!(
            "found {}{}eligible {}",
            bytes(sums.discovered),
            look.glyphs.dot(),
            bytes(sums.eligible)
        )));
    }
    if let Some(order) = app.order.title() {
        tail.push(Span::raw(order));
    }
    if !app.gaps.is_empty() {
        tail.push(Span::styled(
            format!("partial: {} not covered", app.gaps.len()),
            look.style(Role::Warning),
        ));
    }
    if let Some(lens) = app.lens {
        tail.push(Span::raw(format!("lens: {}", lens_title(lens))));
    }
    if !app.filter.is_empty() && app.mode != Mode::Filter {
        tail.push(Span::raw(format!("filter: {}", escaped(&app.filter))));
    }
    let used: usize = head.iter().map(Span::width).sum::<usize>()
        + tail
            .iter()
            .map(|span| span.width() + sep.width())
            .sum::<usize>()
        + 1;
    let room = usize::from(width).saturating_sub(used + sep.width());
    let mut spans = head;
    if let Some(roots) = roots(app, look, room) {
        spans.push(sep.clone());
        spans.push(Span::raw(roots));
    }
    for span in tail {
        spans.push(sep.clone());
        spans.push(span);
    }
    spans.push(Span::raw(" "));
    Text::from(spans)
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
