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
use crate::theme::{Glyphs, Look, Role, Theme};

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

/// What a clean's screen shows: the head that stays above its list, and the list from
/// `scroll` on.
struct Page {
    head: Vec<Text<'static>>,
    lines: Vec<Text<'static>>,
    scroll: usize,
}

/// The page of `cleaning`.
fn page(app: &App, cleaning: &Cleaning, look: Look<'_>) -> Page {
    let titled = |title: String| vec![Text::styled(title, look.style(Role::Title))];
    match cleaning {
        Cleaning::Planning(_) => Page {
            head: titled(crate::human::review::PLANNING.to_owned()),
            lines: Vec::new(),
            scroll: 0,
        },
        Cleaning::Reviewing(shown) => Page {
            head: review::head(review::title(shown), shown, look),
            lines: review::lines(app, shown, look),
            scroll: shown.scroll,
        },
        // The question the typed size answers, above the plan it runs.
        Cleaning::Typing(shown, _) => {
            let items = shown.prepared.plan().entries().len();
            let question = receipt::typed_question(items, &shown.total);
            Page {
                head: review::head(question.trim_end().to_owned(), shown, look),
                lines: review::lines(app, shown, look),
                scroll: shown.scroll,
            }
        }
        Cleaning::Applying(applying) => Page {
            head: titled(progress::title(applying)),
            lines: progress::lines(applying, app.home.as_deref(), look),
            scroll: 0,
        },
        Cleaning::Ended(ended) => Page {
            head: titled(result::title(ended)),
            lines: result::lines(ended, app.home.as_deref(), look),
            scroll: match ended {
                Ended::Ran { scroll, .. } => *scroll,
                Ended::Failed { .. } => 0,
            },
        },
    }
}

/// A page's rows laid out in a clean's `area`: the head's, a blank row, then the list's
/// in the rest. Each line too wide for the terminal breaks into rows that start where it
/// does.
struct Laid {
    head: Vec<Text<'static>>,
    head_area: Rect,
    rows: Vec<Text<'static>>,
    list: Rect,
}

fn lay(page: Page, area: Rect) -> Laid {
    let area = area.inner(Margin::new(1, 0));
    let head = wrap(hung(page.head), area.width);
    let height = u16::try_from(head.len() + 1).unwrap_or(u16::MAX);
    let [head_area, list] =
        Layout::vertical([Constraint::Length(height), Constraint::Fill(1)]).areas(area);
    Laid {
        head,
        head_area,
        rows: wrap(hung(page.lines), list.width),
        list,
    }
}

/// `lines`, each hung where its text starts.
fn hung(lines: Vec<Text<'static>>) -> Vec<Hung> {
    lines
        .into_iter()
        .map(|line| {
            let hang = line.spans.first().map_or(0, |span| {
                span.content.len() - span.content.trim_start().len()
            });
            Hung { line, hang }
        })
        .collect()
}

/// The furthest a clean's list scrolls, at the app's size: its rows past the first page.
pub(crate) fn clean_end(app: &App) -> usize {
    let Some(cleaning) = &app.cleaning else {
        return 0;
    };
    let areas = layout::layout(
        Rect::new(0, 0, app.width, app.height),
        &app.config.ui,
        Screen::Panel,
    );
    // Styles don't change where a line wraps, and both glyph sets draw symbols of the
    // same width, so any look wraps the same.
    let look = Look {
        theme: &Theme::default(),
        glyphs: Glyphs::Ascii,
    };
    let laid = lay(page(app, cleaning, look), areas.body);
    laid.rows
        .len()
        .saturating_sub(usize::from(laid.list.height))
}

/// A clean's screen in place of the tree and the detail pane: its head, then its list
/// from where it was scrolled.
fn clean(app: &App, cleaning: &Cleaning, look: Look<'_>, area: Rect, frame: &mut Frame<'_>) {
    let page = page(app, cleaning, look);
    let scroll = page.scroll;
    let laid = lay(page, area);
    frame.render_widget(Paragraph::new(laid.head), laid.head_area);
    let rows = usize::from(laid.list.height);
    // Running, the newest rows show; otherwise from where it was scrolled, never past
    // the last page.
    let bottom = laid.rows.len().saturating_sub(rows);
    let first = match cleaning {
        Cleaning::Applying(_) => bottom,
        Cleaning::Planning(_)
        | Cleaning::Reviewing(_)
        | Cleaning::Typing(..)
        | Cleaning::Ended(_) => scroll.min(bottom),
    };
    let shown: Vec<Text<'static>> = laid.rows.into_iter().skip(first).collect();
    frame.render_widget(Paragraph::new(shown), laid.list);
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
