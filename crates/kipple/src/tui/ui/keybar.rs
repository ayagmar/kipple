//! The key bar at the foot of the frame: the keys, or what is being typed, or the
//! answer to the last key, with the selection at the right. Narrow, the totals take a
//! line of their own above it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::Paragraph;

use super::tree::tail;
use super::{Look, cols};
use crate::human::{bytes, escaped};
use crate::theme::Role;
use crate::tui::app::{App, Mode};

/// What the hints that show however narrow the bar is say: how to get help and how to
/// leave. Their keys can be rebound, so they are known by their words.
const ALWAYS: [&str; 3] = [" help", " quit", " back"];

/// Draws the key bar in `area`: one line, or two with the found and eligible totals
/// first when `totals`.
pub(super) fn render(app: &App, look: Look<'_>, area: Rect, totals: bool, frame: &mut Frame<'_>) {
    let selection = selection(app, look, totals);
    let width = usize::from(area.width);
    let left = match left(app, look) {
        Left::Hints(hints) if totals => hints_line(look, &hints, width),
        Left::Hints(hints) => {
            let room = width.saturating_sub(selection.width() + 2);
            hints_line(look, &hints, room)
        }
        Left::Text(text) => text,
    };
    if totals {
        frame.render_widget(Paragraph::new(vec![selection, left]), area);
        return;
    }
    frame.render_widget(Paragraph::new(left.clone()), area);
    // The selection shows when it fits beside what is typed or answered.
    if left.width() + 2 + selection.width() <= width {
        frame.render_widget(Paragraph::new(selection).right_aligned(), area);
    }
}

/// What the selection is and what it frees, after the found and eligible totals when
/// `totals`.
fn selection(app: &App, look: Look<'_>, totals: bool) -> Text<'static> {
    let sums = app.totals();
    let mut spans = Vec::new();
    if totals {
        spans.push(Span::raw(format!("found {}", bytes(sums.discovered))));
        spans.push(Span::raw(look.glyphs.dot()));
        spans.push(Span::raw(format!("eligible {}", bytes(sums.eligible))));
        spans.push(Span::raw(look.glyphs.dot()));
    } else if sums.selected_count > 0 {
        spans.push(Span::styled(
            format!("{} ", look.glyphs.mark()),
            look.style(Role::Selected),
        ));
    }
    spans.push(Span::raw(format!(
        "selected {}{}{}",
        sums.selected_count,
        look.glyphs.dot(),
        bytes(sums.selected)
    )));
    spans.push(Span::raw(look.glyphs.dot()));
    spans.push(Span::raw(format!("frees {}", bytes(sums.reclaim))));
    Text::from(spans)
}

/// The left of the bar: the key hints, or a line of its own.
enum Left {
    Hints(String),
    Text(Text<'static>),
}

fn left(app: &App, look: Look<'_>) -> Left {
    let typing = |prompt: String, text: &str, hint: &'static str| {
        Left::Text(Text::from(vec![
            Span::raw(format!("{prompt}{}", escaped(text))),
            Span::styled("_", Style::new().add_modifier(Modifier::SLOW_BLINK)),
            Span::styled(hint, look.style(Role::Muted)),
        ]))
    };
    match (app.mode, &app.notice, &app.panel.typing) {
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
        (_, Some(notice), _) => Left::Text(Text::styled(notice.clone(), look.style(Role::Accent))),
        (Mode::Settings, None, None) => Left::Hints(look.glyphs.keys(&app.keymap().settings_bar())),
        (_, None, _) => Left::Hints(look.glyphs.keys(&app.keymap().browse_bar())),
    }
}

/// The hints of `hints` that fit in `room` columns, in order, keeping the ones in
/// [`ALWAYS`] when others have to go.
fn hints_line(look: Look<'_>, hints: &str, room: usize) -> Text<'static> {
    let all: Vec<&str> = hints.split("  ").collect();
    let kept: usize = all
        .iter()
        .filter(|hint| always(hint))
        .map(|hint| cols(hint) + 2)
        .sum();
    let mut used = 0;
    let mut full = false;
    let mut shown = Vec::new();
    for hint in &all {
        let wide = cols(hint) + 2;
        if always(hint) {
            shown.push(*hint);
        } else if !full && used + wide + kept <= room + 2 {
            used += wide;
            shown.push(*hint);
        } else {
            full = true;
        }
    }
    Text::styled(shown.join("  "), look.style(Role::KeyLabel))
}

fn always(hint: &str) -> bool {
    ALWAYS.iter().any(|what| hint.ends_with(what))
}
