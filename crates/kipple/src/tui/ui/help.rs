//! The keys, over the home screen, as the keymap has them.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use super::Look;
use super::layout::WIDE;
use crate::theme::Role;
use crate::tui::app::App;
use crate::tui::keymap::{Action, HelpLine, Mode};

pub(super) fn render(app: &App, look: Look<'_>, frame: &mut Frame<'_>) {
    let mut lines: Vec<Text<'static>> = app
        .keymap()
        .help()
        .into_iter()
        .map(|HelpLine { keys, what }| {
            let keys = look.glyphs.keys(&keys);
            Text::from(vec![
                Span::styled(format!("{keys:<10} "), look.style(Role::Key)),
                Span::raw(what),
            ])
        })
        .collect();
    lines.push(Text::from(""));
    let confirm = app
        .keymap()
        .first(Mode::Review, Action::Confirm)
        .unwrap_or_default();
    lines.push(Text::styled(
        format!("Selecting removes nothing: only {confirm} on the review cleans."),
        look.style(Role::Muted),
    ));
    // Narrow, it takes the whole screen, frame and all, so nothing shows beside it.
    let area = frame.area();
    let (width, height) = if area.width < WIDE {
        (area.width, area.height)
    } else {
        (72, area.height.saturating_sub(2).min(22))
    };
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    // Clear wipes the cells' styles too, so the popup paints its own text role.
    let block = Block::bordered()
        .border_set(look.glyphs.border())
        .border_style(look.style(Role::Border))
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
