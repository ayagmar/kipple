//! The home screen's drawing: pure functions of the [`App`] state and the [`Look`] (a
//! [`Theme`] and [`Glyphs`]) into a ratatui frame (`docs/03-functional-spec.md` §5), one
//! module per part of the screen. Each asks the theme for a [`Role`] and never names a
//! colour; a finding's state is always a word, and colour only repeats it.

mod detail;
mod frame;
mod help;
mod keybar;
mod layout;
pub(crate) mod onboarding;
pub(crate) mod settings;
mod summary;
mod tree;
mod wrap;

use kipple_core::Bucket;
use ratatui::Frame;
use ratatui::text::{Line as Text, Span};

pub(crate) use self::detail::detail_end;
pub(crate) use self::layout::list_rows;
use self::layout::{Screen, Totals};
pub(crate) use self::tree::{offset, slashed, tail};
use super::app::{App, Mode, Shelf};
use crate::theme::{Look, Role};

impl Look<'_> {
    /// Paints the whole screen in the text role, which every other role is patched
    /// over.
    fn fill(self, frame: &mut Frame<'_>) {
        let area = frame.area();
        frame.buffer_mut().set_style(area, self.style(Role::Text));
    }

    /// `line` as the cursor draws it: the cursor's style patched over each span's own.
    fn cursor(self, line: Text<'static>) -> Text<'static> {
        let cursor = self.style(Role::Cursor);
        let style = line.style.patch(cursor);
        let spans: Vec<Span<'static>> = line
            .spans
            .into_iter()
            .map(|span| {
                let patched = span.style.patch(cursor);
                span.style(patched)
            })
            .collect();
        Text::from(spans).style(style)
    }
}

/// The role of a bucket's name and state.
const fn tint(shelf: Shelf) -> Role {
    match shelf {
        Shelf::Bucket(bucket) => Role::bucket(bucket),
        Shelf::Measuring => Role::Muted,
    }
}

/// The role of a finding's size bar in `shelf`.
const fn bar_tint(shelf: Shelf) -> Role {
    match shelf {
        Shelf::Bucket(Bucket::Safe) => Role::BarSafe,
        Shelf::Bucket(Bucket::Review) => Role::BarReview,
        Shelf::Bucket(Bucket::Protected) => Role::BarProtected,
        Shelf::Measuring => Role::BarTrack,
    }
}

/// A bar `eighths` eighths of a cell long in `cells` cells, for a finding or a bucket
/// in `shelf`: what is filled in the bar's role and the rest in the track's, whose
/// background also shows behind a part-filled cell.
fn drawn_bar(look: Look<'_>, shelf: Shelf, eighths: u128, cells: usize) -> Vec<Span<'static>> {
    let drawn = look
        .glyphs
        .bar(u64::try_from(eighths).unwrap_or(u64::MAX), cells);
    let filled = drawn.trim_end();
    let track = look.style(Role::BarTrack);
    let mut style = look.style(bar_tint(shelf));
    style.bg = style.bg.or(track.bg);
    vec![
        Span::styled(filled.to_owned(), style),
        Span::styled(" ".repeat(cells.saturating_sub(cols(filled))), track),
    ]
}

/// Draws `app` into `frame`.
pub(crate) fn render(app: &App, look: Look<'_>, frame: &mut Frame<'_>) {
    look.fill(frame);
    let screen = if app.mode == Mode::Settings {
        Screen::Panel
    } else {
        Screen::Home
    };
    let areas = layout::layout(frame.area(), &app.config.ui, screen);
    frame::outline(look, areas.outer, &areas.rules, frame);
    keybar::render(
        app,
        look,
        areas.keybar,
        areas.totals == Totals::KeyBar,
        frame,
    );
    let width = areas.outer.width.saturating_sub(4);
    let title = frame::header(app, look, width);
    if screen == Screen::Panel {
        settings::render(app, look, areas.body, frame);
        frame::title(look, title, areas.outer, frame);
        return;
    }
    summary::render(app, look, &areas, frame);
    let lines = app.lines();
    let tree = tree::Tree::new(app, &lines, areas.bars);
    tree::render(app, look, &tree, areas.list, frame);
    if let Some(detail) = areas.detail {
        if let Some(x) = areas.divider {
            frame::vertical(
                look,
                x,
                areas.body.y,
                areas.body.bottom(),
                frame.buffer_mut(),
            );
        }
        detail::render(app, look, tree.focus(), detail, frame);
    }
    frame::title(look, title, areas.outer, frame);
    if app.mode == Mode::Help {
        help::render(app, look, frame);
    }
}

/// How many columns `text` takes.
fn cols(text: &str) -> usize {
    Span::raw(text).width()
}

pub(crate) fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}
