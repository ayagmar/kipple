//! Where each part of the screen goes, from the terminal's size and `[ui]` (D-057): one
//! outer frame with the title line in its top edge, the summary band, the tree and the
//! detail pane, and the key bar, with a rule between each.

use ratatui::layout::{Margin, Rect};

use crate::config::{DetailPlace, UiConfig};

/// From this many columns, findings have size bars and the band shows one card per
/// bucket; below it, the band is one line and the totals take a line of the key bar, as
/// they do whenever there is no band.
pub(crate) const WIDE: u16 = 100;
/// From this many columns, the detail pane goes right of the tree unless `[ui]` says
/// where.
const RIGHT: u16 = 120;
/// From this many rows, the band is shown.
const BAND_ROWS: u16 = 24;
/// The share of the body's width the detail pane takes beside the tree, in percent.
const DETAIL_SHARE: u16 = 45;
/// The columns the tree keeps beside the detail pane, for its labels besides the size,
/// bar and state columns, while the pane can still have [`DETAIL_MIN`].
const TREE_MIN: u16 = 72;
const DETAIL_MIN: u16 = 44;

/// What the body holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Screen {
    /// The tree, with the band and the detail pane as `[ui]` says.
    Home,
    /// A panel in place of the band, the tree and the detail pane: settings.
    Panel,
}

/// Where the found and eligible totals are shown, so they are always somewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Totals {
    /// In the band's last card.
    Band,
    /// On the key bar's first line, with the selection: whenever the band doesn't have
    /// a card for them.
    KeyBar,
}

/// Where the band's text goes, and whether it shows one card per bucket.
#[derive(Debug, Clone, Copy)]
pub(super) struct Band {
    pub(super) area: Rect,
    pub(super) cards: bool,
}

/// Where each part of the screen goes. Every area but `outer` is inside the frame,
/// and the text areas keep a column clear on each side.
#[derive(Debug, Clone)]
pub(super) struct Areas {
    /// The whole screen: the frame is drawn on its edge.
    pub(super) outer: Rect,
    /// The band's text.
    pub(super) band: Option<Band>,
    /// Between the band's rule and the key bar's: where a panel goes.
    pub(super) body: Rect,
    pub(super) list: Rect,
    pub(super) detail: Option<Rect>,
    /// The column of the line between the tree and the detail pane right of it.
    pub(super) divider: Option<u16>,
    pub(super) keybar: Rect,
    /// The rows of the rules that cross the whole frame.
    pub(super) rules: Vec<u16>,
    pub(super) bars: bool,
    pub(super) totals: Totals,
}

/// The parts of `screen` on `area`, as `ui` sets them out.
pub(super) fn layout(area: Rect, ui: &UiConfig, screen: Screen) -> Areas {
    let wide = area.width >= WIDE;
    let banded = screen == Screen::Home && ui.summary && area.height >= BAND_ROWS;
    let totals = if banded && wide {
        Totals::Band
    } else {
        Totals::KeyBar
    };
    let mut rows = rows(area, banded, totals);
    let beside = match (screen, ui.detail) {
        (Screen::Panel, _) | (Screen::Home, DetailPlace::Off) => None,
        (Screen::Home, DetailPlace::Right) => Some(true),
        (Screen::Home, DetailPlace::Bottom) => Some(false),
        (Screen::Home, DetailPlace::Auto) => Some(area.width >= RIGHT),
    };
    let body = rows.body;
    let (list, detail, divider) = match beside {
        None => (body, None, None),
        Some(true) => {
            let (list, detail, x) = side_by_side(body);
            (list, Some(detail), Some(x))
        }
        Some(false) => {
            let (list, detail, y) = stacked(body);
            rows.rules.push(y);
            (list, Some(detail), None)
        }
    };
    let keybar = rows.keybar;
    Areas {
        outer: area,
        band: rows.band.map(|band| Band {
            area: padded(band),
            cards: wide,
        }),
        body,
        list: padded(list),
        detail: detail.map(padded),
        divider,
        // Narrow, the totals line needs every column: only the left one stays clear.
        keybar: if wide {
            padded(keybar)
        } else {
            Rect {
                x: keybar.x + 1,
                width: keybar.width.saturating_sub(1),
                ..keybar
            }
        },
        rules: rows.rules,
        bars: ui.bars && wide,
        totals,
    }
}

/// The frame's rows, top to bottom: the band when `banded`, the body, the key bar (two
/// lines when it shows the `totals`), and the rules between them.
struct Rows {
    band: Option<Rect>,
    body: Rect,
    keybar: Rect,
    rules: Vec<u16>,
}

fn rows(area: Rect, banded: bool, totals: Totals) -> Rows {
    let wide = area.width >= WIDE;
    let inner = area.inner(Margin::new(1, 1));
    let mut rules = Vec::new();
    let keybar_rows = match totals {
        Totals::Band => 1,
        Totals::KeyBar => 2,
    };
    let mut bottom = inner.bottom().saturating_sub(keybar_rows).max(inner.y);
    let keybar = Rect::new(inner.x, bottom, inner.width, keybar_rows.min(inner.height));
    let mut top = inner.y;
    let band = banded.then(|| {
        let height = if wide { 2 } else { 1 };
        let band = Rect::new(inner.x, top, inner.width, height);
        rules.push(top + height);
        top += height + 1;
        band
    });
    if bottom > top {
        bottom -= 1;
        rules.push(bottom);
    }
    let body = Rect::new(inner.x, top, inner.width, bottom.saturating_sub(top));
    Rows {
        band,
        body,
        keybar,
        rules,
    }
}

/// `body` as the tree, the detail pane right of it, and the column of the line between.
fn side_by_side(body: Rect) -> (Rect, Rect, u16) {
    let room = body.width.saturating_sub(1);
    let mut right = room * DETAIL_SHARE / 100;
    if room >= TREE_MIN + DETAIL_MIN {
        right = right.min(room - TREE_MIN);
    }
    let left = room - right;
    let x = body.x + left;
    let list = Rect {
        width: left,
        ..body
    };
    let detail = Rect::new(
        x + 1,
        body.y,
        body.right().saturating_sub(x + 1),
        body.height,
    );
    (list, detail, x)
}

/// `body` as the tree, the detail pane below it, and the row of the rule between.
const fn stacked(body: Rect) -> (Rect, Rect, u16) {
    let above = body.height.saturating_sub(1).div_ceil(2);
    let y = body.y + above;
    let list = Rect {
        height: above,
        ..body
    };
    let detail = Rect::new(
        body.x,
        y + 1,
        body.width,
        body.bottom().saturating_sub(y + 1),
    );
    (list, detail, y)
}

/// `area` less a column on each side.
const fn padded(area: Rect) -> Rect {
    area.inner(Margin::new(1, 0))
}

/// How many tree lines fit on a `width` by `height` screen set out by `ui`.
pub(crate) fn list_rows(width: u16, height: u16, ui: &UiConfig) -> usize {
    usize::from(
        layout(Rect::new(0, 0, width, height), ui, Screen::Home)
            .list
            .height,
    )
}
