//! A theme written back as a theme file, every role spelled out, so `kipple theme show`
//! gives a user a complete file to start their own from.

use std::fmt::Write as _;

use ratatui::style::{Color, Style};

use super::parse::{ANSI, MODIFIERS};
use super::{Role, Theme};

impl Theme {
    /// This theme as a theme file, with every role, headed by `comment`.
    #[must_use]
    pub fn to_toml(&self, comment: &str) -> String {
        let mut text = String::new();
        for line in comment.lines() {
            writeln!(text, "# {line}").ok();
        }
        text.push_str("version = 1\n\n[roles]\n");
        for role in Role::ALL {
            let name = role.name();
            let key = if name.contains('.') {
                format!("\"{name}\"")
            } else {
                name.to_owned()
            };
            writeln!(text, "{key} = {}", style(self.style(role))).ok();
        }
        text
    }
}

/// `style` as an inline table.
fn style(style: Style) -> String {
    let mut parts = Vec::new();
    if let Some(fg) = style.fg {
        parts.push(format!("fg = {}", colour(fg)));
    }
    if let Some(bg) = style.bg {
        parts.push(format!("bg = {}", colour(bg)));
    }
    for (name, modifier) in MODIFIERS {
        if style.add_modifier.contains(modifier) {
            parts.push(format!("{name} = true"));
        }
    }
    if parts.is_empty() {
        "{}".to_owned()
    } else {
        format!("{{ {} }}", parts.join(", "))
    }
}

/// `colour` as a theme file writes it.
fn colour(colour: Color) -> String {
    match colour {
        Color::Reset => "\"reset\"".to_owned(),
        Color::Indexed(index) => index.to_string(),
        Color::Rgb(r, g, b) => format!("\"#{r:02X}{g:02X}{b:02X}\""),
        Color::Black
        | Color::Red
        | Color::Green
        | Color::Yellow
        | Color::Blue
        | Color::Magenta
        | Color::Cyan
        | Color::Gray
        | Color::DarkGray
        | Color::LightRed
        | Color::LightGreen
        | Color::LightYellow
        | Color::LightBlue
        | Color::LightMagenta
        | Color::LightCyan
        | Color::White => ANSI
            .iter()
            .find(|(_, ansi)| *ansi == colour)
            .map_or_else(|| "\"reset\"".to_owned(), |(name, _)| format!("\"{name}\"")),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{BUILTIN, parse};
    use super::*;

    // Catches `theme show` printing a file that doesn't read back as the same theme: a
    // colour, modifier or quoted role name lost on the way out.
    #[test]
    fn every_built_in_theme_reads_back_as_itself() {
        for (name, _) in BUILTIN {
            let theme = Theme::builtin(name).unwrap();
            let text = theme.to_toml("a copy\nof it");
            assert!(
                text.starts_with("# a copy\n# of it\nversion = 1\n"),
                "{text}"
            );
            let back = parse::parse(&text).unwrap().over(Theme::default());
            assert_eq!(back, theme, "{name}:\n{text}");
        }
        let probe = Theme::probe();
        let back = parse::parse(&probe.to_toml(""))
            .unwrap()
            .over(Theme::default());
        assert_eq!(back, probe);
    }
}
