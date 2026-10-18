//! The two lines at the foot: the totals, then the keys or what is being typed.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::Paragraph;

use super::Look;
use super::tree::tail;
use crate::human::{bytes, escaped};
use crate::theme::Role;
use crate::tui::app::{App, Mode};

pub(super) fn footer(app: &App, look: Look<'_>) -> Paragraph<'static> {
    let totals = app.totals();
    let first = vec![
        Span::raw(format!("found {}", bytes(totals.discovered))),
        Span::raw(look.glyphs.dot()),
        Span::raw(format!("eligible {}", bytes(totals.eligible))),
        Span::raw(look.glyphs.dot()),
        Span::raw(format!(
            "selected {} ({})",
            totals.selected_count,
            bytes(totals.selected)
        )),
        Span::raw(look.glyphs.dot()),
        Span::raw(format!("frees {}", bytes(totals.reclaim))),
    ];
    let typing = |prompt: String, text: &str, hint: &'static str| {
        Text::from(vec![
            Span::raw(format!("{prompt}{}", escaped(text))),
            Span::styled("_", Style::new().add_modifier(Modifier::SLOW_BLINK)),
            Span::styled(hint, look.style(Role::Muted)),
        ])
    };
    let second = match (app.mode, &app.notice, &app.panel.typing) {
        (Mode::Filter, _, _) => typing(
            "/".to_owned(),
            &app.filter,
            "  enter keeps it, esc clears it",
        ),
        (Mode::Note(id), _, _) => {
            let what = app
                .row(id)
                .map_or_else(String::new, |row| escaped(&tail(&row.finding.path)));
            typing(
                format!("Keep {what}. Note (optional): "),
                &app.note,
                "  enter keeps, esc cancels",
            )
        }
        (Mode::Settings, _, Some(typed)) => typing(
            "Project root to add (absolute or ~/...): ".to_owned(),
            typed,
            "  enter adds, esc cancels",
        ),
        (_, Some(notice), _) => Text::styled(notice.clone(), look.style(Role::Accent)),
        (Mode::Settings, None, None) => Text::styled(
            "j/k move  h/l change  enter change or add  d remove  esc back",
            look.style(Role::KeyLabel),
        ),
        (_, None, _) => Text::styled(
            "j/k move  / filter  s select  c safe  p keep  , settings  ? help  q quit",
            look.style(Role::KeyLabel),
        ),
    };
    Paragraph::new(vec![Text::from(first), second])
}
