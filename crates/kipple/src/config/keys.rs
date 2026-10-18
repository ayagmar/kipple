//! The `[keys]` table: which key does what on the home screen (D-056). Each entry names
//! an action and the keys that replace its defaults, `select = ["s", "space"]`.

use std::io::{self, Write};
use std::ops::Range;

use toml_edit::{Item, TableLike};

use super::parse::{ParseError, Reader, entries};
use crate::human::escaped;
use crate::tui::Key;
use crate::tui::keymap::{Action, Keymap, key_name, key_named};

/// What `[keys]` says, as the keymap it gives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeysConfig {
    keymap: Keymap,
    /// The entries, each an action and the keys that replace its defaults.
    user: Vec<(Action, Vec<Key>)>,
}

impl KeysConfig {
    /// The keymap: the defaults with the config's bindings over them.
    pub(crate) const fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// Each entry whose keys aren't its action's defaults, in one fixed order.
    fn changed(&self) -> impl Iterator<Item = (Action, &[Key])> {
        Action::ALL.into_iter().filter_map(|action| {
            let (_, keys) = self.user.iter().find(|(a, _)| *a == action)?;
            (keys.as_slice() != action.defaults()).then_some((action, keys.as_slice()))
        })
    }

    /// Its part of `kipple config show`: each entry that changes a default.
    pub(super) fn write_show(&self, out: &mut dyn Write) -> io::Result<()> {
        for (action, keys) in self.changed() {
            let keys: Vec<String> = keys.iter().map(|&key| key_name(key)).collect();
            let key = format!("keys.{}", action.name());
            writeln!(out, "{key:<22}{}", escaped(&keys.join(", ")))?;
        }
        Ok(())
    }
}

const WANTED: &str = "one character, space, enter, tab, backspace, up, down, left, right, home, end, pageup or pagedown";

/// Where a `[keys]` entry is in the file, then where each of its keys is.
type Spans = (Option<Range<usize>>, Vec<Option<Range<usize>>>);

/// A key from the file, with where it is.
type KeyAt = (Key, Option<Range<usize>>);

impl Reader<'_> {
    pub(super) fn keys(&self, table: &dyn TableLike) -> Result<KeysConfig, ParseError> {
        let mut user = Vec::new();
        let mut spans: Vec<Spans> = Vec::new();
        for (name, key, item) in entries(table) {
            let Some(action) = Action::from_name(name) else {
                return Err(self.error_at(key.span(), format!("unknown key keys.{name}")));
            };
            let (keys, at) = self.key_list(name, item)?.into_iter().unzip();
            user.push((action, keys));
            spans.push((key.span(), at));
        }
        Keymap::check(&user).map_err(|conflict| {
            let entry = spans.get(conflict.entry);
            let span = conflict.key.map_or_else(
                || entry.and_then(|(span, _)| span.clone()),
                |index| entry.and_then(|(_, keys)| keys.get(index).cloned().flatten()),
            );
            let name = user
                .get(conflict.entry)
                .map_or("", |(action, _)| action.name());
            self.error_at(span, format!("keys.{name}: {}", conflict.message))
        })?;
        Ok(KeysConfig {
            keymap: Keymap::build(&user),
            user,
        })
    }

    /// The keys `keys.<name>` lists, and where each is.
    fn key_list(&self, name: &str, item: &Item) -> Result<Vec<KeyAt>, ParseError> {
        let array = item.as_array().ok_or_else(|| {
            let message = format!("keys.{name}: expected a list of keys");
            self.error_at(item.span(), message)
        })?;
        let mut keys = Vec::new();
        for value in array {
            let Some(key) = value.as_str().and_then(key_named) else {
                let message = format!("keys.{name}: expected {WANTED}");
                return Err(self.error_at(value.span(), message));
            };
            keys.push((key, value.span()));
        }
        Ok(keys)
    }
}
