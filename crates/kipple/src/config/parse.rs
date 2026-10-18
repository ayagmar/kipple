//! Reading the config file's TOML strictly: every key known, every value checked, and
//! every error placed at a line and column.

use std::ops::Range;

use kipple_core::{Level, Method, RuleId, RuleSettings};
use toml_edit::{Document, Item, Key, TableLike, Value};

use super::{Catalog, Config, is_config_path};

/// Keys `docs/03-functional-spec.md` §6 lists that kipple doesn't read yet.
const LATER: &[&str] = &["review", "retention"];

/// What is wrong with the file, and where.
#[derive(Debug)]
pub(super) struct ParseError {
    pub(super) position: Option<(usize, usize)>,
    pub(super) message: String,
}

/// Validates `text` against the keys of `docs/03-functional-spec.md` §6 that exist.
pub(super) fn parse(text: &str, catalog: &Catalog) -> Result<Config, ParseError> {
    let document = Document::parse(text).map_err(|e| ParseError {
        position: e.span().map(|span| position(text, span.start)),
        message: e.message().trim_end().to_owned(),
    })?;
    let at = |span: Option<Range<usize>>, message: String| ParseError {
        position: span.map(|span| position(text, span.start)),
        message,
    };
    Reader { catalog, at: &at }.config(document.as_table())
}

/// The line and column, from 1, of byte `offset` in `text`.
fn position(text: &str, offset: usize) -> (usize, usize) {
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |newline| newline + 1);
    let column = before.get(start..).map_or(0, |s| s.chars().count()) + 1;
    (line, column)
}

type At<'a> = &'a dyn Fn(Option<Range<usize>>, String) -> ParseError;

/// A string from the file, with where it is.
type Spanned = (String, Option<Range<usize>>);

pub(super) struct Reader<'a> {
    catalog: &'a Catalog,
    at: At<'a>,
}

impl Reader<'_> {
    fn config(&self, root: &dyn TableLike) -> Result<Config, ParseError> {
        let mut config = Config::default();
        let known = [
            "care", "clean", "scan", "exclude", "keep", "rules", "ui", "keys",
        ];
        for (name, key, item) in entries(root) {
            if !known.contains(&name) && !LATER.contains(&name) {
                return Err(self.unknown(name, key));
            }
            let table = self.table(name, key, item)?;
            match name {
                "care" => self.care(table, &mut config)?,
                "clean" => self.clean(table, &mut config)?,
                "scan" => self.scan(table, &mut config)?,
                "exclude" => self.exclude(table, &mut config)?,
                "keep" => self.keep(table, &mut config)?,
                "rules" => self.rules(table, &mut config)?,
                "ui" => config.ui = self.ui(table)?,
                "keys" => config.keys = self.keys(table)?,
                _ => return Err(self.unknown(name, key)),
            }
        }
        Ok(config)
    }

    fn care(&self, care: &dyn TableLike, config: &mut Config) -> Result<(), ParseError> {
        self.only("care", care, &["level"])?;
        config.level = self.named(care, "level", "care.level", Level::from_name)?;
        Ok(())
    }

    fn clean(&self, clean: &dyn TableLike, config: &mut Config) -> Result<(), ParseError> {
        self.only("clean", clean, &["method"])?;
        config.method = self.named(clean, "method", "clean.method", Method::from_name)?;
        Ok(())
    }

    fn scan(&self, scan: &dyn TableLike, config: &mut Config) -> Result<(), ParseError> {
        self.only("scan", scan, &["project_roots"])?;
        if let Some((_, roots)) = scan.get_key_value("project_roots") {
            config.project_roots = Some(self.paths("scan.project_roots", roots)?);
        }
        Ok(())
    }

    fn keep(&self, keep: &dyn TableLike, config: &mut Config) -> Result<(), ParseError> {
        self.only("keep", keep, &["paths"])?;
        if let Some((_, paths)) = keep.get_key_value("paths") {
            config.keep = self.kept(paths)?;
        }
        Ok(())
    }

    fn rules(&self, rules: &dyn TableLike, config: &mut Config) -> Result<(), ParseError> {
        for (id, key, item) in entries(rules) {
            let rule = self.rule_id(id, key.span())?;
            let settings = self.rule(id, self.table(id, key, item)?)?;
            config.rules.push((rule, settings));
        }
        Ok(())
    }

    fn exclude(&self, exclude: &dyn TableLike, config: &mut Config) -> Result<(), ParseError> {
        self.only("exclude", exclude, &["paths", "rules"])?;
        if let Some((_, paths)) = exclude.get_key_value("paths") {
            config.exclude_paths = self.paths("exclude.paths", paths)?;
        }
        if let Some((_, rules)) = exclude.get_key_value("rules") {
            for (id, span) in self.strings("exclude.rules", rules)? {
                config.exclude_rules.push(self.rule_id(&id, span)?);
            }
        }
        Ok(())
    }

    fn rule(&self, id: &str, table: &dyn TableLike) -> Result<RuleSettings, ParseError> {
        let mut settings = RuleSettings::default();
        for (name, key, item) in entries(table) {
            let value = item.as_value();
            match name {
                "enabled" => {
                    let enabled = value.and_then(Value::as_bool);
                    let enabled = enabled
                        .ok_or_else(|| self.bad(item, "rules.<id>.enabled", "true or false"))?;
                    settings.disabled = !enabled;
                }
                "level" => {
                    let level = value.and_then(Value::as_str).and_then(Level::from_name);
                    let wanted = "careful, balanced or thorough";
                    settings.level =
                        Some(level.ok_or_else(|| self.bad(item, "rules.<id>.level", wanted))?);
                }
                "recent_days" => {
                    let days = value
                        .and_then(Value::as_integer)
                        .filter(|days| (1..=365).contains(days))
                        .and_then(|days| u16::try_from(days).ok());
                    let wanted = "a whole number of days from 1 to 365";
                    settings.recent_days =
                        Some(days.ok_or_else(|| self.bad(item, "rules.<id>.recent_days", wanted))?);
                }
                "method" => {
                    let message = format!("rules.\"{id}\".method is not supported yet");
                    return Err((self.at)(key.span(), message));
                }
                _ => return Err(self.unknown(&format!("rules.\"{id}\".{name}"), key)),
            }
        }
        Ok(settings)
    }

    fn rule_id(&self, id: &str, span: Option<Range<usize>>) -> Result<RuleId, ParseError> {
        let rule = RuleId(id.to_owned());
        if self.catalog.contains(&rule) {
            Ok(rule)
        } else {
            let message = format!("there is no built-in rule {id}; `kipple rules` lists them");
            Err((self.at)(span, message))
        }
    }

    /// `keep.paths`: each a path, or a table with a path and an optional note.
    fn kept(&self, item: &Item) -> Result<Vec<(String, Option<String>)>, ParseError> {
        let wanted = "a list of paths or { path = \"...\", note = \"...\" } tables";
        let array = item
            .as_array()
            .ok_or_else(|| self.bad(item, "keep.paths", wanted))?;
        let mut kept = Vec::new();
        for value in array {
            let entry = match value {
                Value::String(path) => (self.path("keep.paths", path.value(), value.span())?, None),
                Value::InlineTable(table) => self.kept_table(table, value)?,
                Value::Integer(_)
                | Value::Float(_)
                | Value::Boolean(_)
                | Value::Datetime(_)
                | Value::Array(_) => {
                    return Err((self.at)(
                        value.span(),
                        format!("keep.paths: expected {wanted}"),
                    ));
                }
            };
            kept.push(entry);
        }
        Ok(kept)
    }

    fn kept_table(
        &self,
        table: &toml_edit::InlineTable,
        value: &Value,
    ) -> Result<(String, Option<String>), ParseError> {
        self.only("keep.paths", table, &["path", "note"])?;
        let text = |name| table.get(name).and_then(Value::as_str).map(str::to_owned);
        let Some(path) = text("path") else {
            let message = "keep.paths: each table needs a path = \"...\"".to_owned();
            return Err((self.at)(value.span(), message));
        };
        let span = table.get("path").and_then(Value::span);
        if table.get("note").is_some_and(|note| !note.is_str()) {
            let note = table.get("note").and_then(Value::span);
            return Err((self.at)(note, "keep.paths: a note is a string".to_owned()));
        }
        Ok((self.path("keep.paths", &path, span)?, text("note")))
    }

    fn paths(&self, key: &str, item: &Item) -> Result<Vec<String>, ParseError> {
        self.strings(key, item)?
            .into_iter()
            .map(|(path, span)| self.path(key, &path, span))
            .collect()
    }

    fn path(
        &self,
        key: &str,
        path: &str,
        span: Option<Range<usize>>,
    ) -> Result<String, ParseError> {
        if is_config_path(path) {
            Ok(path.to_owned())
        } else {
            let message = format!("{key}: {path:?} is neither absolute nor starts with ~");
            Err((self.at)(span, message))
        }
    }

    fn strings(&self, key: &str, item: &Item) -> Result<Vec<Spanned>, ParseError> {
        let array = item
            .as_array()
            .ok_or_else(|| self.bad(item, key, "a list of strings"))?;
        array
            .iter()
            .map(|value| {
                let text = value.as_str().map(str::to_owned);
                let expected = || (self.at)(value.span(), format!("{key}: expected a string"));
                text.map(|text| (text, value.span())).ok_or_else(expected)
            })
            .collect()
    }

    fn named<T>(
        &self,
        table: &dyn TableLike,
        name: &str,
        key: &str,
        from: impl Fn(&str) -> Option<T>,
    ) -> Result<Option<T>, ParseError> {
        let Some((_, item)) = table.get_key_value(name) else {
            return Ok(None);
        };
        let value = item.as_str().and_then(from);
        let wanted = match key {
            "care.level" => "careful, balanced or thorough",
            _ => "delete or trash",
        };
        value.map(Some).ok_or_else(|| self.bad(item, key, wanted))
    }

    fn table<'t>(
        &self,
        name: &str,
        key: &Key,
        item: &'t Item,
    ) -> Result<&'t dyn TableLike, ParseError> {
        if LATER.contains(&name) {
            return Err((self.at)(
                key.span(),
                format!("{name} is not supported yet"),
            ));
        }
        item.as_table_like()
            .ok_or_else(|| (self.at)(item.span(), format!("{name}: expected a table")))
    }

    pub(super) fn only(
        &self,
        table: &str,
        entries_of: &dyn TableLike,
        known: &[&str],
    ) -> Result<(), ParseError> {
        match entries(entries_of).find(|(name, _, _)| !known.contains(name)) {
            Some((name, key, _)) => Err(self.unknown(&format!("{table}.{name}"), key)),
            None => Ok(()),
        }
    }

    fn unknown(&self, name: &str, key: &Key) -> ParseError {
        (self.at)(key.span(), format!("unknown key {name}"))
    }

    fn bad(&self, item: &Item, key: &str, wanted: &str) -> ParseError {
        (self.at)(item.span(), format!("{key}: expected {wanted}"))
    }
}

/// Each entry of `table` with its key, in file order.
fn entries(table: &dyn TableLike) -> impl Iterator<Item = (&str, &Key, &Item)> {
    table.iter().filter_map(move |(name, _)| {
        table
            .get_key_value(name)
            .map(|(key, item)| (name, key, item))
    })
}
