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
mod tree;
mod wrap;

use kipple_core::{Lens, Level};
use ratatui::Frame;
use ratatui::text::{Line as Text, Span};

pub(crate) use self::detail::detail_end;
pub(crate) use self::layout::list_rows;
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
        Shelf::Safe => Role::Safe,
        Shelf::Review => Role::Review,
        Shelf::Protected => Role::Protected,
        Shelf::Measuring => Role::Muted,
    }
}

/// The role of a finding's size bar in `shelf`.
const fn bar_tint(shelf: Shelf) -> Role {
    match shelf {
        Shelf::Safe => Role::BarSafe,
        Shelf::Review => Role::BarReview,
        Shelf::Protected => Role::BarProtected,
        Shelf::Measuring => Role::BarTrack,
    }
}

/// Draws `app` into `frame`.
pub(crate) fn render(app: &App, look: Look<'_>, frame: &mut Frame<'_>) {
    look.fill(frame);
    let areas = layout::layout(frame.area());
    let lines = app.lines();
    let tree = tree::Tree::new(app, &lines, areas.wide);
    frame.render_widget(frame::header(app, look, areas.header.width), areas.header);
    frame.render_widget(keybar::footer(app, look), areas.footer);
    if app.mode == Mode::Settings {
        settings::render(app, look, areas.body, frame);
        return;
    }
    tree::render(app, look, &tree, areas.list, frame);
    let focus = tree.focus();
    detail::render(app, look, focus, &areas, frame);
    if app.mode == Mode::Help {
        help::render(look, frame);
    }
}

/// How many columns `text` takes.
fn cols(text: &str) -> usize {
    Span::raw(text).width()
}

pub(crate) const fn shelf_title(shelf: Shelf) -> &'static str {
    match shelf {
        Shelf::Safe => "Safe to clean",
        Shelf::Review => "Review",
        Shelf::Protected => "Protected",
        Shelf::Measuring => "Measuring",
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

/// What `level` puts in Safe (`docs/04-safety-model.md` §2a).
pub(crate) const fn level_about(level: Level) -> &'static str {
    match level {
        Level::Careful => "Safe: build output and regenerated caches; recent: 14 days",
        Level::Balanced => "Safe: rebuilt, reinstalled, re-downloaded; recent: 7 days",
        Level::Thorough => "Safe: Balanced plus what costs a rollback; recent: 2 days",
    }
}
