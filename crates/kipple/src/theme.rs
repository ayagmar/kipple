//! How the screen looks (D-058): a [`Theme`] and [`Glyphs`].
//!
//! A theme gives each semantic [`Role`] a style, and the glyphs say which symbols to draw
//! with. The screen asks for a role and never names a colour, and every state is also a
//! word, so a theme without colour loses nothing.
//!
//! The built-in themes are TOML files in `themes/`, compiled in, so each is also an
//! example of the format.

mod glyphs;
mod parse;
mod role;

use std::fmt;

use ratatui::style::Style;

pub use self::glyphs::Glyphs;
pub use self::role::Role;

/// The built-in themes, by name.
const BUILTIN: [(&str, &str); 2] = [
    ("kipple", include_str!("../../../themes/kipple.toml")),
    ("mono", include_str!("../../../themes/mono.toml")),
];

/// A style for every role. A role the theme leaves out is drawn in the terminal's own
/// style, which is all the default theme has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Theme {
    styles: [Style; Role::ALL.len()],
}

impl Theme {
    /// The built-in theme `name`: `kipple`, the default, or `mono`, without colour.
    ///
    /// # Errors
    /// When there is no such built-in theme, or its file doesn't read.
    pub fn builtin(name: &str) -> Result<Self, ThemeError> {
        let (_, text) = BUILTIN
            .into_iter()
            .find(|&(builtin, _)| builtin == name)
            .ok_or_else(|| ThemeError {
                position: None,
                message: format!("there is no built-in theme {name}"),
            })?;
        parse::parse(text)
    }

    /// How `role` is drawn.
    #[must_use]
    pub fn style(&self, role: Role) -> Style {
        self.styles.get(role as usize).copied().unwrap_or_default()
    }

    fn set(&mut self, role: Role, style: Style) {
        if let Some(slot) = self.styles.get_mut(role as usize) {
            *slot = style;
        }
    }

    /// A theme for tests that tells every role apart: role `n` of [`Role::ALL`] is
    /// drawn in `Indexed(n)`, the cursor as a background so the roles under it show.
    #[cfg(test)]
    pub(crate) fn probe() -> Self {
        let mut theme = Self::default();
        for (index, role) in (0..).zip(Role::ALL) {
            let colour = ratatui::style::Color::Indexed(index);
            let style = if role == Role::Cursor {
                Style::new().bg(colour)
            } else {
                Style::new().fg(colour)
            };
            theme.set(role, style);
        }
        theme
    }
}

/// How the screen is drawn: the theme gives each role its style, the glyphs its
/// symbols.
#[derive(Debug, Clone, Copy)]
pub struct Look<'a> {
    /// The styles.
    pub theme: &'a Theme,
    /// The symbols.
    pub glyphs: Glyphs,
}

impl Look<'_> {
    /// How `role` is drawn.
    #[must_use]
    pub fn style(self, role: Role) -> Style {
        self.theme.style(role)
    }
}

/// Why a theme file can't be used, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeError {
    /// The line and column, from 1.
    pub position: Option<(usize, usize)>,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some((line, column)) = self.position {
            write!(f, "line {line}, column {column}: ")?;
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for ThemeError {}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::*;

    // Catches a built-in theme that no longer reads, which would stop the home screen
    // from opening.
    #[test]
    fn every_built_in_theme_reads() {
        for (name, _) in BUILTIN {
            assert!(Theme::builtin(name).is_ok(), "{name}");
        }
        assert!(Theme::builtin("dusk").is_err());
    }

    // Catches mono drawing a colour, which NO_COLOR forbids, or dropping the bold and
    // reverse that mark headings and the cursor without colour.
    #[test]
    fn mono_has_no_colour_but_keeps_bold_and_reverse() {
        let mono = Theme::builtin("mono").unwrap();
        for role in Role::ALL {
            let style = mono.style(role);
            assert_eq!((style.fg, style.bg), (None, None), "{role:?}");
        }
        assert!(
            mono.style(Role::Cursor)
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            mono.style(Role::Title)
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    // Catches a theme file whose mistakes are ignored instead of reported where they
    // are: a typo would silently draw the terminal's default.
    #[test]
    fn a_mistake_in_a_theme_file_is_an_error_at_its_line_and_column() {
        let cases = [
            ("[roles]\nsafe = {}", None, "version: expected 1"),
            ("version = 2", Some((1, 11)), "version: expected 1"),
            (
                "version = 1\nname = \"x\"",
                Some((2, 1)),
                "unknown key name",
            ),
            (
                "version = 1\n[roles]\nsafer = {}",
                Some((3, 1)),
                "unknown role safer",
            ),
            (
                "version = 1\n[roles]\nsafe = { fg = \"grean\" }",
                Some((3, 15)),
                "safe.fg: expected",
            ),
            (
                "version = 1\n[roles]\nsafe = { fg = 256 }",
                Some((3, 15)),
                "safe.fg: expected",
            ),
            (
                "version = 1\n[roles]\nsafe = { blink = true }",
                Some((3, 10)),
                "unknown key safe.blink",
            ),
            (
                "version = 1\n[roles]\nsafe = { bold = 1 }",
                Some((3, 17)),
                "safe.bold: expected true or false",
            ),
        ];
        for (text, position, message) in cases {
            let error = parse::parse(text).unwrap_err();
            assert_eq!(error.position, position, "{text}");
            assert!(error.message.starts_with(message), "{text}: {error}");
        }
    }

    // Catches a colour read as another one: a bright name as its dark twin, a hex
    // colour with its channels swapped, or an index off by one.
    #[test]
    fn colours_read_as_ansi_names_hex_indexes_and_reset() {
        let text = "version = 1\n[roles]\n\
            safe = { fg = \"bright-black\", bg = \"white\" }\n\
            review = { fg = \"#F2A23A\" }\n\
            protected = { fg = 208, bg = \"reset\", bold = true, dim = true, italic = true, underline = true, reversed = true }";
        let theme = parse::parse(text).unwrap();

        let safe = theme.style(Role::Safe);
        assert_eq!(
            (safe.fg, safe.bg),
            (Some(Color::DarkGray), Some(Color::Gray))
        );
        assert_eq!(
            theme.style(Role::Review).fg,
            Some(Color::Rgb(0xF2, 0xA2, 0x3A))
        );
        let protected = theme.style(Role::Protected);
        assert_eq!(
            (protected.fg, protected.bg),
            (Some(Color::Indexed(208)), Some(Color::Reset))
        );
        assert_eq!(
            protected.add_modifier,
            Modifier::BOLD
                | Modifier::DIM
                | Modifier::ITALIC
                | Modifier::UNDERLINED
                | Modifier::REVERSED
        );
        assert_eq!(theme.style(Role::Text), Style::new());
    }
}
