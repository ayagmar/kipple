//! The key bar at the foot of the frame: the keys, or what is being typed, or the
//! answer to the last key, with the selection at the right. Narrow, or with no band, the
//! totals take a line of their own above it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::Paragraph;

use super::layout::Totals;
use super::tree::tail;
use super::{Look, cols};
use crate::human::{bytes, escaped, review};
use crate::theme::Role;
use crate::tui::app::{App, Cleaning, Mode};

/// What the hints that show however narrow the bar is say: how to get help and how to
/// leave. Their keys can be rebound, so they are known by their words.
const ALWAYS: [&str; 4] = [" help", " quit", " back", " home"];

/// Draws the key bar in `area`: one line, or two with the found and eligible totals
/// first when `totals` puts them on the key bar.
pub(super) fn render(app: &App, look: Look<'_>, area: Rect, totals: Totals, frame: &mut Frame<'_>) {
    let width = usize::from(area.width);
    let left = left(app, look);
    if totals == Totals::KeyBar {
        let left = match left {
            Left::Hints(hints) => hints_line(look, &hints, width),
            Left::Text(text) => text,
        };
        let sums = selection(app, look, Totals::KeyBar, width);
        frame.render_widget(Paragraph::new(vec![sums, left]), area);
        return;
    }
    let left = match left {
        Left::Hints(hints) => {
            // The hints that always show have their room first; the selection shortens
            // to fit beside them, and the other hints get what is left.
            let beside = width.saturating_sub(always_width(&hints) + 2);
            let selection = selection(app, look, Totals::Band, beside);
            let room = width.saturating_sub(selection.width() + 2);
            frame.render_widget(Paragraph::new(hints_line(look, &hints, room)), area);
            frame.render_widget(Paragraph::new(selection).right_aligned(), area);
            return;
        }
        Left::Text(text) => text,
    };
    frame.render_widget(Paragraph::new(left.clone()), area);
    // The selection shows when it fits beside what is typed or answered.
    let room = width.saturating_sub(left.width() + 2);
    let selection = selection(app, look, Totals::Band, room);
    if selection.width() <= room {
        frame.render_widget(Paragraph::new(selection).right_aligned(), area);
    }
}

/// What the selection is and what it frees, after the found and eligible totals when
/// `totals` puts them on the key bar, in at most `room` columns if it can be: the
/// selection's size goes first, so what it frees always shows.
fn selection(app: &App, look: Look<'_>, totals: Totals, room: usize) -> Text<'static> {
    let full = selection_with(app, look, totals, SelectedSize::Shown);
    if full.width() <= room {
        return full;
    }
    selection_with(app, look, totals, SelectedSize::Left)
}

/// Whether the selection's size shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectedSize {
    Shown,
    Left,
}

fn selection_with(app: &App, look: Look<'_>, totals: Totals, size: SelectedSize) -> Text<'static> {
    let sums = app.totals();
    let mut spans = Vec::new();
    if totals == Totals::KeyBar {
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
    spans.push(Span::raw(format!("selected {}", sums.selected_count)));
    if size == SelectedSize::Shown {
        spans.push(Span::raw(look.glyphs.dot()));
        spans.push(Span::raw(bytes(sums.selected)));
    }
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
    if let Some(cleaning) = &app.cleaning {
        return clean_left(app, cleaning, look, typing);
    }
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

/// The left of the bar while a clean is under way: the size being typed, the answer to
/// the last key, or the keys of the review or the result.
fn clean_left(
    app: &App,
    cleaning: &Cleaning,
    look: Look<'_>,
    typing: impl Fn(String, &str, &'static str) -> Left,
) -> Left {
    // The question is the screen's title, so what is typed always fits beside it.
    if let Cleaning::Typing(_, typed) = cleaning {
        return typing(
            "Size: ".to_owned(),
            typed,
            "  enter confirms, esc goes back",
        );
    }
    if let Some(notice) = &app.notice {
        return Left::Text(Text::styled(notice.clone(), look.style(Role::Accent)));
    }
    let muted = |text: &str| Left::Text(Text::styled(text.to_owned(), look.style(Role::Muted)));
    match cleaning {
        Cleaning::Planning(_) => muted("esc stops planning"),
        Cleaning::Reviewing(_) | Cleaning::Typing(..) => {
            Left::Hints(look.glyphs.keys(&app.keymap().review_bar()))
        }
        Cleaning::Applying(_) => muted(review::STOP_KEY),
        Cleaning::Ended(_) => Left::Hints(look.glyphs.keys(&app.keymap().result_bar())),
    }
}

/// The hints of `hints` that fit in `room` columns, in order, keeping the ones in
/// [`ALWAYS`] when others have to go.
fn hints_line(look: Look<'_>, hints: &str, room: usize) -> Text<'static> {
    let all: Vec<&str> = hints.split("  ").collect();
    let kept = always_width(hints) + 2;
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
    // A hint is its keys, joined by `/`, then what they do.
    let shown: Vec<Hint> = shown
        .iter()
        .map(|hint| {
            let (keys, what) = hint.split_once(' ').unwrap_or((hint, ""));
            Hint::new(keys.to_owned(), what.to_owned())
        })
        .collect();
    joined(look, &shown, "  ")
}

/// How many columns the hints in [`ALWAYS`] take, with the gaps between them.
fn always_width(hints: &str) -> usize {
    let widths: Vec<usize> = hints
        .split("  ")
        .filter(|hint| always(hint))
        .map(cols)
        .collect();
    widths.iter().sum::<usize>() + 2 * widths.len().saturating_sub(1)
}

fn always(hint: &str) -> bool {
    ALWAYS.iter().any(|what| hint.ends_with(what))
}

/// A key hint: the keys, then what they do.
pub(super) struct Hint {
    keys: String,
    what: String,
}

impl Hint {
    pub(super) const fn new(keys: String, what: String) -> Self {
        Self { keys, what }
    }
}

/// `hints` joined by `between`, each its keys in the key role and what they do in the
/// key label role.
pub(super) fn joined(look: Look<'_>, hints: &[Hint], between: &str) -> Text<'static> {
    let label = look.style(Role::KeyLabel);
    let mut spans = Vec::new();
    for (index, Hint { keys, what }) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(between.to_owned(), label));
        }
        spans.push(Span::styled(keys.clone(), look.style(Role::Key)));
        spans.push(Span::styled(format!(" {what}"), label));
    }
    Text::from(spans)
}
