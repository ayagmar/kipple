//! The home screen's drawing: pure functions of the [`App`] state and the [`Look`] into a
//! ratatui frame (`docs/03-functional-spec.md` §5), one module per part of the screen.
//! A finding's state is always a word; colour only repeats it.

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
use ratatui::style::{Color, Style};
use ratatui::symbols::border;
use ratatui::text::Span;

pub(crate) use self::detail::detail_end;
pub(crate) use self::layout::list_rows;
pub(crate) use self::tree::{offset, slashed, tail};
use super::app::{App, Mode, Shelf};

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

/// Draws `app` into `frame`.
pub(crate) fn render(app: &App, look: Look, frame: &mut Frame<'_>) {
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
