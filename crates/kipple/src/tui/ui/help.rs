//! The keys, over the home screen.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use super::Look;
use super::layout::WIDE;

pub(super) fn render(look: Look, frame: &mut Frame<'_>) {
    let keys = [
        ("j k  ↓ ↑", "move"),
        ("h l  ← →", "fold and unfold a group or project"),
        ("g G", "first and last line"),
        (
            "/",
            "filter by words of the path or rule; enter keeps it, esc clears it",
        ),
        ("tab", "show one lens: Agents, Dev, System, then all"),
        ("s", "select or unselect the finding"),
        ("a", "add the group's or project's Safe and Review findings"),
        ("c", "select the whole Safe bucket"),
        ("p", "keep the finding for good, with an optional note"),
        ("P", "keep every finding of its rule: turn the rule off"),
        ("o", "sort by size, last change or name"),
        ("J K", "scroll the detail pane"),
        ("y", "send the path to the terminal clipboard"),
        (",", "settings: level, project roots, method, what is kept"),
        ("enter", "show what is selected"),
        ("?", "this help"),
        ("q", "quit"),
    ];
    let mut lines: Vec<Text<'static>> = keys
        .iter()
        .map(|(key, what)| {
            let key = if look.ascii {
                key.replace('↓', "dn")
                    .replace('↑', "up")
                    .replace('←', "lt")
                    .replace('→', "rt")
            } else {
                (*key).to_owned()
            };
            Text::from(vec![
                Span::styled(
                    format!("{key:<10} "),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw(*what),
            ])
        })
        .collect();
    lines.push(Text::from(""));
    lines.push(Text::styled(
        "This version only finds and explains: selecting removes nothing.",
        look.dim(),
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
    let block = Block::bordered().border_set(look.border()).title(" Keys ");
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}
