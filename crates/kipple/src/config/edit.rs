//! Changing the config file: a batch of key edits applied in one atomic write to what is
//! on disk at that moment, keeping comments and every other key.

use std::path::{MAIN_SEPARATOR, Path, PathBuf};

use kipple_core::{Level, Method, Roots, RuleId};
use kipple_platform::{WriteError, write_config};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value, value};

use super::parse::{self, ParseError};
use super::{Catalog, Config, ConfigError, Location, expand, home, same_path};

/// One change to the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// Sets `care.level`.
    Level(Level),
    /// Sets `clean.method`.
    Method(Method),
    /// Sets `scan.project_roots`.
    ProjectRoots(Vec<PathBuf>),
    /// Adds a path to `keep.paths`, with an optional note.
    Keep {
        /// The path kept.
        path: PathBuf,
        /// Why.
        note: Option<String>,
    },
    /// Removes every `keep.paths` entry for this path.
    Unkeep(PathBuf),
    /// Turns a rule off (`rules.<id>.enabled = false`), or on again, which also takes it
    /// out of `exclude.rules`.
    RuleEnabled {
        /// The rule.
        rule: RuleId,
        /// Whether it runs.
        enabled: bool,
    },
}

/// Applies `edits` to the config file at `location` in one atomic write.
///
/// It returns what the file says now, so the caller rebuilds its policy from it, an
/// editor's changes included. Paths inside the home directory of `roots` are written with
/// `~`.
///
/// # Errors
/// When the config directory is unknown, a path isn't valid UTF-8, the file on disk no
/// longer reads as a valid config (it may be mid-edit), or it can't be written. Nothing
/// is changed then.
pub fn write(
    location: &Location,
    edits: &[Edit],
    roots: &Roots,
    catalog: &Catalog,
) -> Result<Config, ConfigError> {
    let error = |position, message: String| ConfigError {
        location: location.clone(),
        position,
        message,
    };
    let Some(path) = &location.path else {
        let message = "kipple's config directory is unknown, so nothing was written";
        return Err(error(None, message.to_owned()));
    };
    let home = home(roots);
    let written = write_config(path, |current| {
        let text = current.unwrap_or_default();
        parse::parse(text, catalog).map_err(|e| ParseError {
            message: format!(
                "the file no longer reads as a valid config, so nothing was written: {}",
                e.message
            ),
            ..e
        })?;
        let mut document: DocumentMut = text.parse().map_err(|_| ParseError {
            position: None,
            message: "the file no longer parses, so nothing was written".to_owned(),
        })?;
        for edit in edits {
            apply(&mut document, edit, home)?;
        }
        let edited = document.to_string();
        parse::parse(&edited, catalog)?;
        Ok(edited)
    });
    let text = written.map_err(|e: WriteError<ParseError>| match e {
        WriteError::Edit(e) => error(e.position, e.message),
        WriteError::Io { path: file, source } => error(
            None,
            format!("{} can't be written: {source}", file.display()),
        ),
    })?;
    parse::parse(&text, catalog).map_err(|e| error(e.position, e.message))
}

fn apply(document: &mut DocumentMut, edit: &Edit, home: Option<&Path>) -> Result<(), ParseError> {
    let root = document.as_table_mut();
    match edit {
        Edit::Level(level) => {
            table(root, "care")?.insert("level", value(level.name()));
        }
        Edit::Method(method) => {
            table(root, "clean")?.insert("method", value(method.name()));
        }
        Edit::ProjectRoots(paths) => {
            let written = paths
                .iter()
                .map(|path| written(path, home))
                .collect::<Result<Array, _>>()?;
            table(root, "scan")?.insert("project_roots", value(written));
        }
        Edit::Keep { path, note } => {
            let mut entry = InlineTable::new();
            entry.insert("path", written(path, home)?);
            if let Some(note) = note {
                entry.insert("note", note.as_str().into());
            }
            array(table(root, "keep")?, "paths")?.push(entry);
        }
        Edit::Unkeep(path) => {
            if let Some(kept) = root
                .get_mut("keep")
                .and_then(Item::as_table_like_mut)
                .and_then(|keep| keep.get_mut("paths"))
                .and_then(Item::as_array_mut)
            {
                kept.retain(|entry| !names(entry, path, home));
            }
        }
        Edit::RuleEnabled { rule, enabled } => set_enabled(root, rule, *enabled)?,
    }
    Ok(())
}

fn set_enabled(root: &mut Table, rule: &RuleId, enabled: bool) -> Result<(), ParseError> {
    if !enabled {
        let rules = table(root, "rules")?;
        table(rules, &rule.0)?.insert("enabled", value(false));
        return Ok(());
    }
    if let Some(rules) = root.get_mut("rules").and_then(Item::as_table_like_mut) {
        if let Some(own) = rules.get_mut(&rule.0).and_then(Item::as_table_like_mut) {
            own.remove("enabled");
            if own.is_empty() {
                rules.remove(&rule.0);
            }
        }
        if rules.is_empty() {
            root.remove("rules");
        }
    }
    if let Some(excluded) = root
        .get_mut("exclude")
        .and_then(Item::as_table_like_mut)
        .and_then(|exclude| exclude.get_mut("rules"))
        .and_then(Item::as_array_mut)
    {
        excluded.retain(|id| id.as_str() != Some(rule.0.as_str()));
    }
    Ok(())
}

/// The table `name` of `parent`, created when missing. A table that only holds others
/// gets no header of its own.
fn table<'a>(
    parent: &'a mut dyn TableLike,
    name: &str,
) -> Result<&'a mut dyn TableLike, ParseError> {
    parent
        .entry(name)
        .or_insert_with(|| {
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_like_mut()
        .ok_or_else(|| not_a(name, "table"))
}

/// The array `name` of `table`, created when missing.
fn array<'a>(table: &'a mut dyn TableLike, name: &str) -> Result<&'a mut Array, ParseError> {
    table
        .entry(name)
        .or_insert_with(|| value(Array::new()))
        .as_array_mut()
        .ok_or_else(|| not_a(name, "list"))
}

/// What the validated file can't hold, should it happen.
fn not_a(name: &str, kind: &str) -> ParseError {
    ParseError {
        position: None,
        message: format!("{name} is not a {kind}, so nothing was written"),
    }
}

/// `path` as config holds it: with `~` for the home directory.
fn written(path: &Path, home: Option<&Path>) -> Result<Value, ParseError> {
    let Some(text) = path.to_str() else {
        let message = format!(
            "{} isn't valid UTF-8, so it can't be written to the config file",
            path.display()
        );
        return Err(ParseError {
            position: None,
            message,
        });
    };
    let inside = home.and_then(|home| path.strip_prefix(home).ok());
    let shown = match inside.and_then(Path::to_str) {
        Some("") => "~".to_owned(),
        Some(rest) => format!("~{MAIN_SEPARATOR}{rest}"),
        None => text.to_owned(),
    };
    Ok(shown.into())
}

/// Whether a `keep.paths` entry names `path`.
fn names(entry: &Value, path: &Path, home: Option<&Path>) -> bool {
    let written = match entry {
        Value::String(text) => Some(text.value().as_str()),
        Value::InlineTable(table) => table.get("path").and_then(Value::as_str),
        Value::Integer(_)
        | Value::Float(_)
        | Value::Boolean(_)
        | Value::Datetime(_)
        | Value::Array(_) => None,
    };
    written
        .and_then(|written| expand(written, home))
        .is_some_and(|expanded| same_path(&expanded, path))
}
