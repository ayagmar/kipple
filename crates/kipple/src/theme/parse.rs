//! Reading a theme file strictly, like the config: every key known, every value
//! checked, every error placed at a line and column.
//!
//! ```toml
//! version = 1
//! extends = "kipple"
//! [roles]
//! safe = { fg = "green", bold = true }
//! "bar.safe" = { fg = "#7FD47F" }
//! ```

use std::ops::Range;

use ratatui::style::{Color, Modifier, Style};
use toml_edit::{Document, Item, TableLike, Value};

use super::{Role, Theme, ThemeError, valid_name};
use crate::config::position;

/// The ANSI colour names a theme may use, in the terminal's palette order.
pub(super) const ANSI: [(&str, Color); 16] = [
    ("black", Color::Black),
    ("red", Color::Red),
    ("green", Color::Green),
    ("yellow", Color::Yellow),
    ("blue", Color::Blue),
    ("magenta", Color::Magenta),
    ("cyan", Color::Cyan),
    ("white", Color::Gray),
    ("bright-black", Color::DarkGray),
    ("bright-red", Color::LightRed),
    ("bright-green", Color::LightGreen),
    ("bright-yellow", Color::LightYellow),
    ("bright-blue", Color::LightBlue),
    ("bright-magenta", Color::LightMagenta),
    ("bright-cyan", Color::LightCyan),
    ("bright-white", Color::White),
];

/// The modifiers a role may switch on, by their key.
pub(super) const MODIFIERS: [(&str, Modifier); 5] = [
    ("bold", Modifier::BOLD),
    ("dim", Modifier::DIM),
    ("italic", Modifier::ITALIC),
    ("underline", Modifier::UNDERLINED),
    ("reversed", Modifier::REVERSED),
];

const COLOUR: &str = "an ANSI colour name, #rrggbb, an index from 0 to 255, or reset";

/// What a theme file says: the theme it extends, if any, and its own roles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ThemeFile {
    /// The theme it starts from, and where its name is in the file.
    pub(super) extends: Option<(String, (usize, usize))>,
    /// The roles it sets; each replaces the style the base theme gives it.
    pub(super) roles: Vec<(Role, Style)>,
}

impl ThemeFile {
    /// `base` with this file's roles set over it.
    pub(super) fn over(&self, mut base: Theme) -> Theme {
        for &(role, style) in &self.roles {
            base.set(role, style);
        }
        base
    }
}

/// The theme file `text`.
pub(super) fn parse(text: &str) -> Result<ThemeFile, ThemeError> {
    let document = Document::parse(text).map_err(|e| ThemeError {
        file: None,
        position: e.span().map(|span| position(text, span.start)),
        message: e.message().trim_end().to_owned(),
    })?;
    let place = |span: Option<Range<usize>>| span.map(|span| position(text, span.start));
    let at = |span: Option<Range<usize>>, message: String| ThemeError {
        file: None,
        position: place(span),
        message,
    };
    Reader {
        at: &at,
        place: &place,
    }
    .theme(document.as_table())
}

struct Reader<'doc> {
    at: &'doc dyn Fn(Option<Range<usize>>, String) -> ThemeError,
    place: &'doc dyn Fn(Option<Range<usize>>) -> Option<(usize, usize)>,
}

impl Reader<'_> {
    fn theme(&self, root: &dyn TableLike) -> Result<ThemeFile, ThemeError> {
        let version = root.get("version");
        if version.and_then(Item::as_integer) != Some(1) {
            return Err((self.at)(
                version.and_then(Item::span),
                "version: expected 1".to_owned(),
            ));
        }
        let mut theme = ThemeFile::default();
        for (name, item) in root.iter() {
            let span = root.key(name).and_then(toml_edit::Key::span);
            match name {
                "version" => {}
                "extends" => theme.extends = Some(self.extends(item)?),
                "roles" => self.roles(item, &mut theme)?,
                _ => return Err((self.at)(span, format!("unknown key {name}"))),
            }
        }
        Ok(theme)
    }

    /// The name of the theme `extends` names, and where it is.
    fn extends(&self, item: &Item) -> Result<(String, (usize, usize)), ThemeError> {
        let name = item
            .as_str()
            .filter(|name| valid_name(name))
            .ok_or_else(|| {
                let wanted = "extends: expected a theme name: lowercase letters, digits and -";
                (self.at)(item.span(), wanted.to_owned())
            })?;
        let place = (self.place)(item.span()).unwrap_or((1, 1));
        Ok((name.to_owned(), place))
    }

    fn roles(&self, roles: &Item, theme: &mut ThemeFile) -> Result<(), ThemeError> {
        let table = roles
            .as_table_like()
            .ok_or_else(|| (self.at)(roles.span(), "roles: expected a table".to_owned()))?;
        for (name, value) in table.iter() {
            let span = table.key(name).and_then(toml_edit::Key::span);
            let role = Role::from_name(name)
                .ok_or_else(|| (self.at)(span, format!("unknown role {name}")))?;
            theme.roles.push((role, self.style(name, value)?));
        }
        Ok(())
    }

    fn style(&self, role: &str, style: &Item) -> Result<Style, ThemeError> {
        let table = style
            .as_table_like()
            .ok_or_else(|| (self.at)(style.span(), format!("{role}: expected a table")))?;
        let mut read = Style::new();
        for (name, value) in table.iter() {
            let span = table.key(name).and_then(toml_edit::Key::span);
            match name {
                "fg" => read.fg = Some(self.colour(role, name, value)?),
                "bg" => read.bg = Some(self.colour(role, name, value)?),
                _ => read = read.add_modifier(self.modifier(role, name, span, value)?),
            }
        }
        Ok(read)
    }

    /// The modifier `key` switches on, or none when it is `false`.
    fn modifier(
        &self,
        role: &str,
        key: &str,
        span: Option<Range<usize>>,
        value: &Item,
    ) -> Result<Modifier, ThemeError> {
        let &(_, modifier) = MODIFIERS
            .iter()
            .find(|(name, _)| *name == key)
            .ok_or_else(|| (self.at)(span, format!("unknown key {role}.{key}")))?;
        let on = value.as_bool().ok_or_else(|| {
            (self.at)(
                value.span(),
                format!("{role}.{key}: expected true or false"),
            )
        })?;
        Ok(if on { modifier } else { Modifier::empty() })
    }

    fn colour(&self, role: &str, key: &str, item: &Item) -> Result<Color, ThemeError> {
        let colour = match item.as_value() {
            Some(Value::String(name)) => colour(name.value()),
            Some(Value::Integer(index)) => u8::try_from(*index.value()).ok().map(Color::Indexed),
            _ => None,
        };
        colour.ok_or_else(|| (self.at)(item.span(), format!("{role}.{key}: expected {COLOUR}")))
    }
}

/// The colour `name` stands for.
pub(super) fn colour(name: &str) -> Option<Color> {
    if name == "reset" {
        return Some(Color::Reset);
    }
    if let Some(&(_, colour)) = ANSI.iter().find(|(ansi, _)| *ansi == name) {
        return Some(colour);
    }
    let hex = name
        .strip_prefix('#')
        .filter(|hex| hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))?;
    let channel = |at: usize| {
        hex.get(at..at + 2)
            .and_then(|c| u8::from_str_radix(c, 16).ok())
    };
    Some(Color::Rgb(channel(0)?, channel(2)?, channel(4)?))
}
