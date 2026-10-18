//! Where each part of the home screen goes, from the terminal's size.

use ratatui::layout::{Constraint, Layout as Split, Rect};

/// A terminal at least this wide shows the detail pane beside the tree; a narrower one
/// shows it below.
pub(super) const WIDE: u16 = 100;

/// Where each part of the screen goes.
pub(super) struct Areas {
    pub(super) header: Rect,
    /// The tree and the detail pane together, where the settings panel goes.
    pub(super) body: Rect,
    pub(super) list: Rect,
    pub(super) detail: Rect,
    pub(super) footer: Rect,
    pub(super) wide: bool,
}

pub(super) fn layout(area: Rect) -> Areas {
    let [header, body, footer] = Split::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(area);
    let wide = area.width >= WIDE;
    let [list, detail] = if wide {
        Split::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(body)
    } else {
        Split::vertical([Constraint::Fill(1), Constraint::Fill(1)]).areas(body)
    };
    Areas {
        header,
        body,
        list,
        detail,
        footer,
        wide,
    }
}

/// How many tree lines fit on a `width` by `height` screen.
pub(crate) fn list_rows(width: u16, height: u16) -> usize {
    usize::from(layout(Rect::new(0, 0, width, height)).list.height)
}
