//! The theme's roles in the CLI's human output: SGR escapes around a role's text when
//! colour is on, nothing at all when it is off.

use std::fmt::Write as _;

use ratatui::style::{Color, Modifier, Style};

use super::{Role, Theme};

/// How the human output paints a role: with a theme drawn for the terminal, or plain.
#[derive(Debug, Clone, Default)]
pub struct Ink {
    theme: Option<Theme>,
}

impl Ink {
    /// No escapes at all: piped output, `--color never`, `NO_COLOR`.
    #[must_use]
    pub const fn plain() -> Self {
        Self { theme: None }
    }

    /// `theme`, already drawn for the terminal's colours.
    #[must_use]
    pub const fn new(theme: Theme) -> Self {
        Self { theme: Some(theme) }
    }

    /// `text` in the style of `role`: unchanged when plain or when the role has no style.
    /// Pad `text` before painting it, so the escapes don't count as columns.
    #[must_use]
    pub fn paint(&self, role: Role, text: &str) -> String {
        let codes = self
            .theme
            .as_ref()
            .map(|theme| sgr(theme.style(role)))
            .unwrap_or_default();
        if codes.is_empty() || text.is_empty() {
            text.to_owned()
        } else {
            format!("\x1b[{codes}m{text}\x1b[0m")
        }
    }
}

/// The SGR parameters for `style`, `;`-separated; empty when it changes nothing.
fn sgr(style: Style) -> String {
    const MODIFIERS: [(Modifier, &str); 5] = [
        (Modifier::BOLD, "1"),
        (Modifier::DIM, "2"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::REVERSED, "7"),
    ];
    let mut codes: Vec<String> = MODIFIERS
        .iter()
        .filter(|(modifier, _)| style.add_modifier.contains(*modifier))
        .map(|(_, code)| (*code).to_owned())
        .collect();
    codes.extend(style.fg.map(|colour| colour_code(colour, 30)));
    codes.extend(style.bg.map(|colour| colour_code(colour, 40)));
    codes.join(";")
}

/// The SGR parameters of `colour` as a foreground (`base` 30) or background (40).
fn colour_code(colour: Color, base: u8) -> String {
    let named = |offset: u8| (base + offset).to_string();
    let bright = |offset: u8| (base + 60 + offset).to_string();
    match colour {
        Color::Reset => named(9),
        Color::Black => named(0),
        Color::Red => named(1),
        Color::Green => named(2),
        Color::Yellow => named(3),
        Color::Blue => named(4),
        Color::Magenta => named(5),
        Color::Cyan => named(6),
        Color::Gray => named(7),
        Color::DarkGray => bright(0),
        Color::LightRed => bright(1),
        Color::LightGreen => bright(2),
        Color::LightYellow => bright(3),
        Color::LightBlue => bright(4),
        Color::LightMagenta => bright(5),
        Color::LightCyan => bright(6),
        Color::White => bright(7),
        Color::Indexed(index) => format!("{}8;5;{index}", base / 10),
        Color::Rgb(r, g, b) => {
            let mut code = format!("{}8;2", base / 10);
            for channel in [r, g, b] {
                write!(code, ";{channel}").ok();
            }
            code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Catches a wrong SGR code: a bright colour as its dark twin, a background written
    // as a foreground, or a 256-colour or 24-bit colour in the wrong form.
    #[test]
    fn roles_paint_as_their_sgr_codes() {
        let text = "version = 1\n[roles]\n\
            safe = { fg = \"green\", bold = true }\n\
            review = { fg = \"bright-black\", bg = 208 }\n\
            protected = { fg = \"#F2A23A\", underline = true }\n";
        let theme = super::super::parse::parse(text)
            .unwrap()
            .over(Theme::default());
        let ink = Ink::new(theme);
        assert_eq!(ink.paint(Role::Safe, "safe"), "\x1b[1;32msafe\x1b[0m");
        assert_eq!(
            ink.paint(Role::Review, "review"),
            "\x1b[90;48;5;208mreview\x1b[0m"
        );
        assert_eq!(
            ink.paint(Role::Protected, "kept"),
            "\x1b[4;38;2;242;162;58mkept\x1b[0m"
        );
        // A role without a style, and empty text, stay as they are.
        assert_eq!(ink.paint(Role::Text, "text"), "text");
        assert_eq!(ink.paint(Role::Safe, ""), "");
    }

    // Catches an escape in plain output, which would corrupt piped text.
    #[test]
    fn plain_ink_writes_no_escape() {
        for role in Role::ALL {
            assert_eq!(Ink::plain().paint(role, "word"), "word");
        }
    }
}
