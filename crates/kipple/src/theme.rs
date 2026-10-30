//! How the screen looks (D-055, D-058): a [`Theme`] and [`Glyphs`].
//!
//! A theme gives each semantic [`Role`] a style, and the glyphs say which symbols to draw
//! with. The screen asks for a role and never names a colour, and every state is also a
//! word, so a theme without colour loses nothing.
//!
//! The built-in themes are TOML files in `themes/`, compiled in, so each is also an
//! example of the format. A user's own themes are files in the `themes` directory beside
//! the default config file ([`Themes`]), and may start from another theme with
//! `extends`. Colour is used or not, and in how many colours, by [`Colours`].

mod colour;
mod glyphs;
mod ink;
mod parse;
mod role;
mod schema;
mod show;
mod source;

use std::fmt;
use std::path::PathBuf;

use ratatui::style::Style;

use crate::config::Position;

pub use self::colour::{ColorChoice, Colours, Depth, Stdout};
pub use self::glyphs::{Fold, Glyphs};
pub use self::ink::Ink;
pub use self::role::Role;
pub(crate) use self::schema::schema;
pub(crate) use self::source::write_list;
pub use self::source::{Catalog, Entry, Origin, Themes};

/// The theme drawn when neither `--theme` nor the config names one.
pub const DEFAULT: &str = "kipple";

/// The built-in themes, by name.
const BUILTIN: [(&str, &str); 5] = [
    ("kipple", include_str!("../../../themes/kipple.toml")),
    (
        "kipple-dark",
        include_str!("../../../themes/kipple-dark.toml"),
    ),
    (
        "kipple-light",
        include_str!("../../../themes/kipple-light.toml"),
    ),
    (
        "high-contrast",
        include_str!("../../../themes/high-contrast.toml"),
    ),
    ("mono", include_str!("../../../themes/mono.toml")),
];

/// Whether `name` can name a theme: lowercase ASCII letters, digits and `-`, as a file
/// name on every system.
#[must_use]
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A style for every role. A role the theme leaves out is drawn in the terminal's own
/// style.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Theme {
    styles: [Style; Role::ALL.len()],
}

impl Theme {
    /// No style for any role: everything in the terminal's own style.
    pub(crate) const EMPTY: Self = Self {
        styles: [Style::new(); Role::ALL.len()],
    };

    /// The built-in theme `name`.
    ///
    /// # Errors
    /// When there is no such built-in theme, or its file doesn't read.
    pub fn builtin(name: &str) -> Result<Self, ThemeError> {
        let text = builtin_text(name).ok_or_else(|| ThemeError {
            file: None,
            position: None,
            message: format!("there is no built-in theme {name}"),
        })?;
        Ok(parse::parse(text)?.over(Self::default()))
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

    /// This theme as `colours` let it be drawn: `mono` without colour, else each colour
    /// brought down to the nearest the terminal has.
    #[must_use]
    pub fn drawn(&self, colours: Colours) -> Self {
        let Colours::On(depth) = colours else {
            // `mono` is compiled in, and a test reads every built-in theme.
            return Self::builtin("mono").unwrap_or_default();
        };
        let mut drawn = self.clone();
        for style in &mut drawn.styles {
            style.fg = style.fg.map(|c| depth.fit(c));
            style.bg = style.bg.map(|c| depth.fit(c));
        }
        drawn
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

/// The text of the built-in theme `name`.
fn builtin_text(name: &str) -> Option<&'static str> {
    BUILTIN
        .into_iter()
        .find(|&(builtin, _)| builtin == name)
        .map(|(_, text)| text)
}

/// How the screen is drawn: the theme gives each role its style, the glyphs its
/// symbols.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Look<'theme> {
    /// The styles.
    pub(crate) theme: &'theme Theme,
    /// The symbols.
    pub(crate) glyphs: Glyphs,
}

impl Look<'_> {
    /// How `role` is drawn.
    #[must_use]
    pub(crate) fn style(self, role: Role) -> Style {
        self.theme.style(role)
    }
}

/// Why a theme can't be used, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeError {
    /// The theme file, unless the theme is built in or wasn't found.
    pub file: Option<PathBuf>,
    /// Where in the file.
    pub position: Option<Position>,
    /// What is wrong.
    pub message: String,
}

impl ThemeError {
    /// The error, as found in `file`.
    fn in_file(self, file: Option<&PathBuf>) -> Self {
        Self {
            file: file.cloned(),
            ..self
        }
    }
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(file) = &self.file {
            write!(f, "{}", file.display())?;
            match self.position {
                Some(position) => write!(f, ", {position}: ")?,
                None => f.write_str(": ")?,
            }
        } else if let Some(position) = self.position {
            write!(f, "{position}: ")?;
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for ThemeError {}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::*;
    use crate::config::Position;

    // Catches a built-in theme that no longer reads, which would stop the home screen
    // from opening, or one that leans on `extends`, which built-ins don't resolve.
    #[test]
    fn every_built_in_theme_reads_and_stands_alone() {
        for (name, text) in BUILTIN {
            assert!(Theme::builtin(name).is_ok(), "{name}");
            assert_eq!(parse::parse(text).unwrap().extends, None, "{name}");
            assert!(valid_name(name), "{name}");
        }
        assert!(Theme::builtin("dusk").is_err());
    }

    // Catches a built-in theme that leaves a role to the terminal by mistake: every
    // built-in names every role, so a new role can't be forgotten in one of them.
    #[test]
    fn every_built_in_theme_names_every_role() {
        for (name, text) in BUILTIN {
            let file = parse::parse(text).unwrap();
            for role in Role::ALL {
                assert!(
                    file.roles.iter().any(|set| set.role == role),
                    "{name} leaves out {}",
                    role.name()
                );
            }
        }
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

    // Catches colour leaking through when it is off: every built-in theme, drawn
    // without colour, is mono.
    #[test]
    fn without_colour_every_theme_draws_as_mono() {
        let mono = Theme::builtin("mono").unwrap();
        for (name, _) in BUILTIN {
            assert_eq!(
                Theme::builtin(name).unwrap().drawn(Colours::Off),
                mono,
                "{name}"
            );
        }
    }

    // Catches the brand amber spreading: BRAND.md keeps it for what the user keeps.
    #[test]
    fn brand_amber_is_only_for_what_is_kept() {
        let amber = Color::Rgb(0xF2, 0xA2, 0x3A);
        for (name, _) in BUILTIN {
            let theme = Theme::builtin(name).unwrap();
            for role in Role::ALL.into_iter().filter(|&role| role != Role::Kept) {
                let style = theme.style(role);
                assert_ne!(style.fg, Some(amber), "{name} {}", role.name());
                assert_ne!(style.bg, Some(amber), "{name} {}", role.name());
            }
        }
    }

    // Catches a theme file whose mistakes are ignored instead of reported where they
    // are: a typo would silently draw the terminal's default.
    #[test]
    fn a_mistake_in_a_theme_file_is_an_error_at_its_line_and_column() {
        let cases = [
            ("[roles]\nsafe = {}", None, "version: expected 1"),
            (
                "version = 2",
                Some(Position {
                    line: 1,
                    column: 11,
                }),
                "version: expected 1",
            ),
            (
                "version = 1\nname = \"x\"",
                Some(Position { line: 2, column: 1 }),
                "unknown key name",
            ),
            (
                "version = 1\nextends = \"Dusk\"",
                Some(Position {
                    line: 2,
                    column: 11,
                }),
                "extends: expected a theme name",
            ),
            (
                "version = 1\n[roles]\nsafer = {}",
                Some(Position { line: 3, column: 1 }),
                "unknown role safer",
            ),
            (
                "version = 1\n[roles]\nsafe = { fg = \"grean\" }",
                Some(Position {
                    line: 3,
                    column: 15,
                }),
                "safe.fg: expected",
            ),
            (
                "version = 1\n[roles]\nsafe = { fg = 256 }",
                Some(Position {
                    line: 3,
                    column: 15,
                }),
                "safe.fg: expected",
            ),
            (
                "version = 1\n[roles]\nsafe = { blink = true }",
                Some(Position {
                    line: 3,
                    column: 10,
                }),
                "unknown key safe.blink",
            ),
            (
                "version = 1\n[roles]\nsafe = { bold = 1 }",
                Some(Position {
                    line: 3,
                    column: 17,
                }),
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
        let theme = parse::parse(text).unwrap().over(Theme::default());

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
