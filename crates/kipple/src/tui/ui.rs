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
mod progress;
mod result;
mod review;
pub(crate) mod settings;
mod summary;
mod tree;
mod wrap;

use std::path::{Path, PathBuf};

use kipple_core::Bucket;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::Paragraph;

pub(crate) use self::detail::detail_end;
use self::layout::Screen;
pub(crate) use self::layout::list_rows;
pub(crate) use self::tree::{offset, slashed, tail};
use self::wrap::{Hung, wrap};
use super::app::{App, Cleaning, Ended, Mode, Shelf};
use crate::human::receipt;
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
    let screen = if app.cleaning.is_some() {
        Screen::Panel
    } else {
        screen
    };
    let areas = layout::layout(frame.area(), &app.config.ui, screen);
    frame::outline(look, areas.outer, &areas.rules, frame);
    keybar::render(app, look, areas.keybar, areas.totals, frame);
    let width = areas.outer.width.saturating_sub(4);
    let title = frame::header(app, look, width);
    if let Some(cleaning) = &app.cleaning {
        clean(app, cleaning, look, areas.body, frame);
        frame::title(look, title, areas.outer, frame);
        return;
    }
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

/// A clean's screen in place of the tree and the detail pane: its title, then its
/// lines from `scroll` on.
fn clean(app: &App, cleaning: &Cleaning, look: Look<'_>, area: Rect, frame: &mut Frame<'_>) {
    let (title, lines, scroll) = match cleaning {
        Cleaning::Planning(_) => (crate::human::review::PLANNING.to_owned(), Vec::new(), 0),
        Cleaning::Reviewing(shown) => (
            review::title(shown),
            review::lines(app, shown, look),
            shown.scroll,
        ),
        // The question the typed size answers, above the plan it runs.
        Cleaning::Typing(shown, _) => {
            let items = shown.prepared.plan().entries().len();
            let question = receipt::typed_question(items, &shown.total);
            (
                question.trim_end().to_owned(),
                review::lines(app, shown, look),
                shown.scroll,
            )
        }
        Cleaning::Applying(applying) => (
            progress::title(applying),
            progress::lines(applying, app.home.as_deref(), look),
            0,
        ),
        Cleaning::Ended(ended) => {
            let scroll = match ended {
                Ended::Ran { scroll, .. } => *scroll,
                Ended::Failed { .. } => 0,
            };
            let lines = result::lines(ended, app.home.as_deref(), look);
            (result::title(ended), lines, scroll)
        }
    };
    let area = area.inner(Margin::new(1, 0));
    let [head, list] = Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(Text::styled(title, look.style(Role::Title))),
        head,
    );
    // Each line too wide for the terminal breaks into rows that start where it does.
    let lines = wrap(
        lines
            .into_iter()
            .map(|line| {
                let hang = line.spans.first().map_or(0, |span| {
                    span.content.len() - span.content.trim_start().len()
                });
                Hung { line, hang }
            })
            .collect(),
        list.width,
    );
    let rows = usize::from(list.height);
    // Running, the newest rows show; otherwise from where it was scrolled, never past
    // the last page.
    let bottom = lines.len().saturating_sub(rows);
    let first = match cleaning {
        Cleaning::Applying(_) => bottom,
        Cleaning::Planning(_)
        | Cleaning::Reviewing(_)
        | Cleaning::Typing(..)
        | Cleaning::Ended(_) => scroll.min(bottom),
    };
    let shown: Vec<Text<'static>> = lines.into_iter().skip(first).collect();
    frame.render_widget(Paragraph::new(shown), list);
}

/// `path` as the home screen shows it in a clean's lines: below the home directory,
/// from `~` (`docs/03-functional-spec.md` §5 Paths).
fn homed(path: &Path, home: Option<&Path>) -> PathBuf {
    PathBuf::from(crate::human::homed(path, home))
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
