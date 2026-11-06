//! The settings in agents' own config files that move a store kipple must keep closed:
//! Codex's `sqlite_home` and Pi's `sessionDir` (`docs/05-architecture.md` §1). Those files
//! can hold secrets, so nothing else in them is read, kept or shown.
//!
//! These are readers of one key, not parsers of the format. A store they name only gets
//! protected, so reading too much costs nothing: every place a file sets the key counts,
//! whatever table or object it is in, and a value that can't be read is an error, never an
//! unset key.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{io, io::ErrorKind};

use kipple_core::{SettingProblem, UnresolvedRoot};

use crate::config_file::read_config;

/// How a config file writes its keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    /// `key = "value"`, one to a line.
    Toml,
    /// `"key": "value"`.
    Json,
}

/// One setting in one config file.
#[derive(Debug, Clone, Copy)]
pub(super) struct ConfigKey {
    /// The file's name, for people.
    pub(super) file: &'static str,
    /// The key.
    pub(super) key: &'static str,
    /// How the file writes it.
    pub(super) format: Format,
}

impl ConfigKey {
    /// Every value `path` gives this key, none when the file isn't there.
    ///
    /// # Errors
    /// When the file can't be read, or a value isn't a string kipple can read.
    pub(super) fn values(self, path: &Path) -> Result<Vec<PathBuf>, UnresolvedRoot> {
        let unusable = |source: io::Error| UnresolvedRoot::Setting {
            file: self.file,
            problem: SettingProblem::Unreadable(Arc::new(source)),
        };
        let Some(text) = read_config(path).map_err(unusable)? else {
            return Ok(Vec::new());
        };
        let found = match self.format {
            Format::Toml => toml_values(&text, self.key),
            Format::Json => json_values(&text, self.key),
        };
        found
            .into_iter()
            .map(|value| {
                value.map(PathBuf::from).ok_or_else(|| {
                    let message = format!("`{}` isn't a string kipple can read", self.key);
                    unusable(io::Error::new(ErrorKind::InvalidData, message))
                })
            })
            .collect()
    }
}

/// What each line setting `key` gives it, `None` for a value that can't be read.
fn toml_values(text: &str, key: &str) -> Vec<Option<String>> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(name, _)| name.trim().trim_matches(['"', '\'']) == key)
        .map(|(_, value)| toml_string(value.trim_start()))
        .collect()
}

/// The string `value` starts with, TOML's literal form included.
fn toml_string(value: &str) -> Option<String> {
    let Some(literal) = value.strip_prefix('\'') else {
        return quoted(value);
    };
    literal.split_once('\'').map(|(text, _)| text.to_owned())
}

/// What each `"key":` in `text` gives it, `None` for a value that can't be read. The same
/// words as a value, with no colon after them, are no key.
fn json_values(text: &str, key: &str) -> Vec<Option<String>> {
    let name = format!("\"{key}\"");
    text.match_indices(&name)
        .filter_map(|(at, _)| {
            let after = text.get(at + name.len()..)?.trim_start();
            after.strip_prefix(':')
        })
        .map(|value| quoted(value.trim_start()))
        .collect()
}

/// The double-quoted string `value` starts with. Only the escapes a path needs are read:
/// any other leaves it unreadable.
fn quoted(value: &str) -> Option<String> {
    let mut chars = value.strip_prefix('"')?.chars();
    let mut text = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(text),
            '\\' => match chars.next()? {
                escaped @ ('\\' | '"' | '/') => text.push(escaped),
                _ => return None,
            },
            plain => text.push(plain),
        }
    }
}
