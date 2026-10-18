//! The keys, over the home screen, as the keymap has them.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use super::Look;
use super::layout::WIDE;
use crate::theme::Role;
use crate::tui::app::App;

pub(super) fn render(app: &App, look: Look<'_>, frame: &mut Frame<'_>) {
    let mut lines: Vec<Text<'static>> = app
        .keymap()
        .help()
        .into_iter()
        .map(|(keys, what)| {
            let keys = look.glyphs.keys(&keys);
            Text::from(vec![
                Span::styled(format!("{keys:<10} "), look.style(Role::Key)),
                Span::raw(what),
            ])
        })
        .collect();
    lines.push(Text::from(""));
    lines.push(Text::styled(
        "This version only finds and explains: selecting removes nothing.",
        look.style(Role::Muted),
    ));
    let area = frame.area();
    // Full width when narrow, so nothing shows beside it.
    let width = if area.width < WIDE { area.width } else { 72 };
    let height = area.height.saturating_sub(2).min(22);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    // Clear wipes the cells' styles too, so the popup paints its own text role.
    let block = Block::bordered()
        .border_set(look.glyphs.border())
        .style(look.style(Role::Text))
        .title(" Keys ");
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}
