//! The settings in agents' own config files that move a store kipple must keep closed:
//! Codex's `sqlite_home` and Pi's `sessionDir` (`docs/05-architecture.md` §1). Those files
//! can hold secrets, so nothing else in them is read, kept or shown.
//!
//! Each file is parsed, because these values decide what stays closed: a value decoded wrongly
//! would name the wrong directory and leave the real one open. Every place a document sets
//! the key counts, at any depth, and a document that doesn't parse or a value that isn't a
//! string is an error, never an unset key. A parser's error can quote the document, so it is
//! dropped here and the message says only that the file can't be parsed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{io, io::ErrorKind};

use kipple_core::{SettingProblem, UnresolvedRoot};
use serde_json::Value as Json;
use toml::de::{DeTable, DeValue};

use super::tool_path::Tool;
use crate::config_file::read_config;

/// One setting in one config file.
#[derive(Debug, Clone, Copy)]
pub(super) struct ConfigKey {
    /// The file's name, for people.
    pub(super) file: &'static str,
    /// The key.
    pub(super) key: &'static str,
    /// Whose file it is, which says how it is written and what its paths mean.
    pub(super) tool: Tool,
}

impl ConfigKey {
    /// Every value `path` gives this key, none when the file isn't there.
    ///
    /// # Errors
    /// When the file can't be read or parsed, or a value isn't a string.
    pub(super) fn values(self, path: &Path) -> Result<Vec<PathBuf>, UnresolvedRoot> {
        let unusable = |source: io::Error| UnresolvedRoot::Setting {
            file: self.file,
            problem: SettingProblem::Unreadable(Arc::new(source)),
        };
        let invalid = |message: String| unusable(io::Error::new(ErrorKind::InvalidData, message));
        let Some(text) = read_config(path).map_err(unusable)? else {
            return Ok(Vec::new());
        };
        let found = match self.tool {
            Tool::Codex => toml_values(&text, self.key),
            Tool::Pi => json_values(&text, self.key),
        };
        found
            .ok_or_else(|| invalid("it can't be parsed".to_owned()))?
            .into_iter()
            .map(|value| {
                let message = || format!("`{}` isn't a string", self.key);
                value.map(PathBuf::from).ok_or_else(|| invalid(message()))
            })
            .collect()
    }
}

/// Every place the TOML `text` sets `key`, `None` for a value that isn't a string. `None`
/// when `text` doesn't parse.
fn toml_values(text: &str, key: &str) -> Option<Vec<Option<String>>> {
    let document = DeTable::parse(text).ok()?;
    let mut found = Vec::new();
    in_toml_table(document.get_ref(), key, &mut found);
    Some(found)
}

/// The parser bounds how deep a document nests, so this recursion is bounded too.
fn in_toml_table(table: &DeTable<'_>, key: &str, found: &mut Vec<Option<String>>) {
    for (name, value) in table {
        if name.get_ref() == key {
            found.push(value.get_ref().as_str().map(str::to_owned));
        }
        in_toml_value(value.get_ref(), key, found);
    }
}

fn in_toml_value(value: &DeValue<'_>, key: &str, found: &mut Vec<Option<String>>) {
    match value {
        DeValue::Table(table) => in_toml_table(table, key, found),
        DeValue::Array(items) => {
            for item in items {
                in_toml_value(item.get_ref(), key, found);
            }
        }
        DeValue::String(_)
        | DeValue::Integer(_)
        | DeValue::Float(_)
        | DeValue::Boolean(_)
        | DeValue::Datetime(_) => {}
    }
}

/// Every place the JSON `text` sets `key`, `None` for a value that isn't a string. `None`
/// when `text` doesn't parse. A leading byte order mark is skipped, as Pi skips it. Of a key
/// repeated in one object only the last counts, which is also the one Pi's `JSON.parse` keeps.
fn json_values(text: &str, key: &str) -> Option<Vec<Option<String>>> {
    let document: Json =
        serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(text)).ok()?;
    let mut found = Vec::new();
    in_json(&document, key, &mut found);
    Some(found)
}

/// The parser bounds how deep a document nests, so this recursion is bounded too.
fn in_json(value: &Json, key: &str, found: &mut Vec<Option<String>>) {
    match value {
        Json::Object(object) => {
            for (name, item) in object {
                if name == key {
                    found.push(item.as_str().map(str::to_owned));
                }
                in_json(item, key, found);
            }
        }
        Json::Array(items) => {
            for item in items {
                in_json(item, key, found);
            }
        }
        Json::Null | Json::Bool(_) | Json::Number(_) | Json::String(_) => {}
    }
}
