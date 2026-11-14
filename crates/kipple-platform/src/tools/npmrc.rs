//! Where npm's cache is, as its configuration says (S5 npm §2), read the way npm 11.20.0's
//! `@npmcli/config` reads it (`workspaces/config/lib/index.js`, `parse-field.js`,
//! `env-replace.js` and the `ini` package): its `npm_config_*` variables in any case, then
//! the user's npmrc, the global npmrc and npm's own. A project's `.npmrc` depends on the
//! directory npm runs in, so it isn't read; `explain` states that limit.
//!
//! Only the keys that place the cache are read: `cache`, and `userconfig`, `globalconfig`
//! and `prefix`, which decide which user and global file can set it. An npmrc holds
//! tokens, so no line, key or value of one is kept, logged or put in an error. Anything
//! kipple can't read exactly as npm does leaves the cache unknown, never the default.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, ErrorKind, Read as _};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use kipple_core::{RootSource, SettingProblem, SymbolicRoot, UnresolvedRoot};

use crate::environment::{Environment, Var};
use crate::os;

/// The largest npmrc read. A larger one is unknown, never read in part.
const LIMIT: u64 = 256 * 1024;

/// What the errors and sources call each place a setting can come from.
const ENVIRONMENT: &str = "the environment";
const USER: &str = "the user npmrc";
const GLOBAL: &str = "the global npmrc";
const BUILTIN: &str = "npm's own npmrc";

/// The keys kipple reads, and no other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Cache,
    UserConfig,
    GlobalConfig,
    Prefix,
}

const KEYS: [Key; 4] = [Key::Cache, Key::UserConfig, Key::GlobalConfig, Key::Prefix];

impl Key {
    const fn name(self) -> &'static str {
        match self {
            Self::Cache => "cache",
            Self::UserConfig => "userconfig",
            Self::GlobalConfig => "globalconfig",
            Self::Prefix => "prefix",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Cache => 0,
            Self::UserConfig => 1,
            Self::GlobalConfig => 2,
            Self::Prefix => 3,
        }
    }

    /// Why its value can't be placed, said without the value.
    const fn unplaced(self) -> &'static str {
        match self {
            Self::Cache => "sets `cache` to something that isn't an absolute path as npm reads it",
            Self::UserConfig => {
                "sets `userconfig` to something that isn't an absolute path as npm reads it"
            }
            Self::GlobalConfig => {
                "sets `globalconfig` to something that isn't an absolute path as npm reads it"
            }
            Self::Prefix => {
                "sets `prefix` to something that isn't an absolute path as npm reads it"
            }
        }
    }

    fn named(name: &str) -> Option<Self> {
        KEYS.into_iter().find(|key| key.name() == name)
    }
}

/// A value as npm decodes it, before `${VAR}` and `~/` are expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Raw {
    /// Text.
    Text(String),
    /// A flag, `true`, `false`, `null`, a list, or a quoting kipple doesn't decode: never a
    /// path kipple can place.
    Other,
}

/// What one source sets of the keys kipple reads.
#[derive(Debug, Default)]
struct Settings([Option<Raw>; 4]);

impl Settings {
    fn get(&self, key: Key) -> Option<&Raw> {
        self.0.get(key.index()).and_then(Option::as_ref)
    }

    fn set(&mut self, key: Key, raw: Raw) {
        if let Some(slot) = self.0.get_mut(key.index()) {
            *slot = Some(raw);
        }
    }
}

/// One source of settings: what it sets and what it is called.
#[derive(Debug)]
struct Layer {
    settings: Settings,
    label: &'static str,
}

/// Where npm found by `PATH` lives, as far as its configuration depends on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Install {
    /// Its package directory, `…/node_modules/npm`, which holds its own `npmrc`.
    pub(crate) package: PathBuf,
    /// The node that runs it, whose prefix places the global npmrc by default.
    pub(crate) node: PathBuf,
}

/// What resolving npm's cache reads besides the files.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Reading<'context> {
    /// kipple's environment, which is the user's.
    pub(crate) env: &'context Environment,
    /// The home directory, which `~/` names, if it is known.
    pub(crate) home: Option<&'context Path>,
    /// The npm whose own and global npmrc count, if one was found.
    pub(crate) install: Option<&'context Install>,
}

/// Where npm's cache is, and what said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placed {
    /// The cache root, absolute and folded as npm folds it.
    pub(crate) path: PathBuf,
    /// What set it.
    pub(crate) source: RootSource,
}

/// Where npm's cache is, read as npm reads it: a variable, then the user npmrc, the
/// global one, npm's own and the default.
///
/// # Errors
/// Whenever any of that can't be read exactly as npm does: kipple never guesses a cache.
pub(crate) fn cache(reading: &Reading<'_>) -> Result<Placed, UnresolvedRoot> {
    let env = from_environment(reading.env)?;
    if let Some(raw) = env.settings.get(Key::Cache) {
        let source = RootSource::Env("npm_config_cache");
        return placed(raw, Key::Cache, &env, reading, source);
    }
    let install = reading.install.ok_or_else(|| {
        unusable(
            "npm",
            "wasn't found on PATH, so its own npmrc, which may move its cache, can't be read",
        )
    })?;
    let builtin = from_file(&install.package.join("npmrc"), BUILTIN, reading.env)?;
    let user_file = user_file(&env, &builtin, reading)?;
    let user = from_file(&user_file, USER, reading.env)?;
    if let Some(raw) = user.settings.get(Key::Cache) {
        return placed(raw, Key::Cache, &user, reading, setting(USER));
    }
    let global_file = global_file([&env, &user, &builtin], reading, install)?;
    let global = from_file(&global_file, GLOBAL, reading.env)?;
    for layer in [&global, &builtin] {
        if let Some(raw) = layer.settings.get(Key::Cache) {
            return placed(raw, Key::Cache, layer, reading, setting(layer.label));
        }
    }
    default_cache(reading)
}

const fn setting(file: &'static str) -> RootSource {
    RootSource::Setting { file, key: "cache" }
}

const fn unusable(file: &'static str, problem: &'static str) -> UnresolvedRoot {
    UnresolvedRoot::Setting {
        file,
        problem: SettingProblem::Unusable(problem),
    }
}

fn placed(
    raw: &Raw,
    key: Key,
    layer: &Layer,
    reading: &Reading<'_>,
    source: RootSource,
) -> Result<Placed, UnresolvedRoot> {
    let path = path_of(raw, key, layer.label, reading)?;
    Ok(Placed { path, source })
}

/// The user npmrc: `userconfig` from a variable or npm's own npmrc, else `~/.npmrc`.
fn user_file(
    env: &Layer,
    builtin: &Layer,
    reading: &Reading<'_>,
) -> Result<PathBuf, UnresolvedRoot> {
    for layer in [env, builtin] {
        if let Some(raw) = layer.settings.get(Key::UserConfig) {
            return path_of(raw, Key::UserConfig, layer.label, reading);
        }
    }
    let home = reading.home.ok_or(UnresolvedRoot::BaseUnresolved {
        base: SymbolicRoot::Home,
    })?;
    Ok(home.join(".npmrc"))
}

/// The global npmrc: `globalconfig` where one sets it, else `etc/npmrc` below `prefix`
/// where one sets that, else below the prefix of the node that runs npm.
fn global_file(
    layers: [&Layer; 3],
    reading: &Reading<'_>,
    install: &Install,
) -> Result<PathBuf, UnresolvedRoot> {
    for key in [Key::GlobalConfig, Key::Prefix] {
        for layer in layers {
            if let Some(raw) = layer.settings.get(key) {
                let path = path_of(raw, key, layer.label, reading)?;
                return Ok(match key {
                    Key::Prefix => path.join("etc").join("npmrc"),
                    Key::GlobalConfig | Key::Cache | Key::UserConfig => path,
                });
            }
        }
    }
    Ok(default_prefix(reading.env, install)?
        .join("etc")
        .join("npmrc"))
}

/// npm's own prefix: two levels above its node, or its node's directory on Windows.
/// `PREFIX` and `DESTDIR` move it in ways kipple doesn't follow, so either one set is
/// unknown.
fn default_prefix(env: &Environment, install: &Install) -> Result<PathBuf, UnresolvedRoot> {
    if env.get("PREFIX").is_some() || (!cfg!(windows) && env.get("DESTDIR").is_some()) {
        return Err(unusable(
            ENVIRONMENT,
            "sets PREFIX or DESTDIR, which move npm's global npmrc in ways kipple doesn't follow",
        ));
    }
    let levels = if cfg!(windows) { 1 } else { 2 };
    let prefix = install.node.ancestors().nth(levels);
    prefix.map(Path::to_path_buf).ok_or_else(|| {
        unusable(
            "npm",
            "runs from a node with no prefix above it, so its global npmrc can't be placed",
        )
    })
}

/// The cache npm uses when nothing sets one: `~/.npm`, or on Windows `npm-cache` below
/// `%LOCALAPPDATA%`, else below the home.
fn default_cache(reading: &Reading<'_>) -> Result<Placed, UnresolvedRoot> {
    if cfg!(windows)
        && let Some(local) = reading.env.get("LOCALAPPDATA")
    {
        let local = PathBuf::from(local);
        if !local.is_absolute() {
            return Err(unusable(
                ENVIRONMENT,
                "sets LOCALAPPDATA to a relative path",
            ));
        }
        let path = fold(&local.join("npm-cache"));
        return Ok(Placed {
            path,
            source: RootSource::Env("LOCALAPPDATA"),
        });
    }
    let home = reading.home.ok_or(UnresolvedRoot::BaseUnresolved {
        base: SymbolicRoot::Home,
    })?;
    let relative = if cfg!(windows) { "npm-cache" } else { ".npm" };
    Ok(Placed {
        path: home.join(relative),
        source: RootSource::Default {
            base: SymbolicRoot::Home,
            relative,
        },
    })
}

/// What npm's variables set: `npm_config_<key>` in any case, an empty one ignored. Two
/// spellings with different values are unknown, since which one npm takes depends on the
/// order the system lists them in.
fn from_environment(env: &Environment) -> Result<Layer, UnresolvedRoot> {
    let mut settings = Settings::default();
    for Var { name, value } in env.vars() {
        let Some(key) = env_key(name) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        let raw = value
            .to_str()
            .map_or(Raw::Other, |text| Raw::Text(text.to_owned()));
        match settings.get(key) {
            Some(earlier) if *earlier != raw => {
                return Err(unusable(
                    ENVIRONMENT,
                    "sets one of npm's settings that place its cache in two spellings, with \
                     different values",
                ));
            }
            Some(_) | None => settings.set(key, raw),
        }
    }
    Ok(Layer {
        settings,
        label: ENVIRONMENT,
    })
}

/// The key a variable named `name` sets, if it is one kipple reads: `npm_config_` in any
/// case, then the key with `_` for `-`, in any case.
fn env_key(name: &OsStr) -> Option<Key> {
    let name = name.to_str()?;
    let prefix = name.get(.."npm_config_".len())?;
    if !prefix.eq_ignore_ascii_case("npm_config_") {
        return None;
    }
    let rest = name.get("npm_config_".len()..)?;
    Key::named(&rest.to_ascii_lowercase())
}

/// What the npmrc at `path` sets, nothing when there is none.
fn from_file(path: &Path, label: &'static str, env: &Environment) -> Result<Layer, UnresolvedRoot> {
    let settings = match read(path) {
        Ok(Some(text)) => parse(&text, env).map_err(|problem| unusable(label, problem))?,
        Ok(None) => Settings::default(),
        Err(error) => {
            return Err(UnresolvedRoot::Setting {
                file: label,
                problem: SettingProblem::Unreadable(Arc::new(error)),
            });
        }
    };
    Ok(Layer { settings, label })
}

/// The whole of the file at `path`, followed as npm follows it, `None` when there is no
/// such file. Larger than [`LIMIT`] or not UTF-8 is an error.
fn read(path: &Path) -> io::Result<Option<String>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if file.metadata()?.is_dir() {
        return Err(ErrorKind::IsADirectory.into());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(ErrorKind::FileTooLarge.into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| ErrorKind::InvalidData.into())
}

/// What an npmrc sets of the keys kipple reads, as the `ini` package decodes its top
/// level: lines split on CR and LF, comments and blank lines skipped, the first section
/// header ending the top level, a line `key = value` or a bare `key`, and the last of a
/// key winning. A line JavaScript would split elsewhere, or a key this can't decode the
/// way npm does, is an error, never a guess.
fn parse(text: &str, env: &Environment) -> Result<Settings, &'static str> {
    const UNPARSABLE: &str = "can't be read the way npm reads it";
    let mut settings = Settings::default();
    for line in text.split(['\r', '\n']) {
        if line.contains(['\u{2028}', '\u{2029}', '\u{85}']) {
            return Err(UNPARSABLE);
        }
        let start = js_trim(line);
        if start.is_empty() || start.starts_with([';', '#']) || line.starts_with('=') {
            continue;
        }
        if is_section(line) {
            break;
        }
        let (key, value) = line
            .split_once('=')
            .map_or((line, None), |(k, v)| (k, Some(v)));
        let Decoded::Text(decoded) = decode(key) else {
            continue;
        };
        let written = if decoded.contains("${") {
            expand(&decoded, env, Unset::Keep).ok_or(UNPARSABLE)?
        } else {
            decoded
        };
        let (name, list) = written
            .strip_suffix("[]")
            .filter(|_| written.len() > 2)
            .map_or((written.as_str(), false), |name| (name, true));
        let Some(known) = Key::named(name) else {
            continue;
        };
        let raw = match (value.map(decode), list) {
            (Some(Decoded::Text(set)), false)
                if !matches!(set.as_str(), "true" | "false" | "null") =>
            {
                Raw::Text(set)
            }
            _ => Raw::Other,
        };
        settings.set(known, raw);
    }
    Ok(settings)
}

/// Whether `line` is a section header, `[name]` from its first character with only white
/// space after.
fn is_section(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('[') else {
        return false;
    };
    rest.split_once(']')
        .is_some_and(|(_, after)| js_trim(after).is_empty())
}

/// A key or value as the `ini` package decodes it.
#[derive(Debug, PartialEq, Eq)]
enum Decoded {
    Text(String),
    /// A quoting whose JSON decoding kipple doesn't model.
    Uncertain,
}

/// Decodes `raw` as `ini`'s `unsafe` does: trimmed, a quoted one unquoted, an unquoted one
/// cut at the first `;` or `#` not escaped by `\`. Quoted text holding a quote or a
/// backslash, or single-quoted text that reads as JSON other than a string, is uncertain.
fn decode(raw: &str) -> Decoded {
    let val = js_trim(raw);
    let quote = val
        .chars()
        .next()
        .filter(|first| ['"', '\''].contains(first));
    if let Some(quote) = quote
        && val.ends_with(quote)
    {
        let Some(body) = val.get(1..val.len() - 1).filter(|_| val.len() >= 2) else {
            return Decoded::Uncertain;
        };
        let plain = !body.contains(['"', '\'', '\\']) && !body.chars().any(char::is_control);
        let json = quote == '\'' && reads_as_json(body);
        return if plain && !json {
            Decoded::Text(body.to_owned())
        } else {
            Decoded::Uncertain
        };
    }
    Decoded::Text(js_trim(&unescaped(val)).to_owned())
}

/// `text` up to its first `;` or `#` not escaped, with `\;`, `\#` and `\\` unescaped and
/// any other `\` kept, as `ini` does.
fn unescaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut escaped = false;
    for c in text.chars() {
        if escaped {
            if !matches!(c, '\\' | ';' | '#') {
                out.push('\\');
            }
            out.push(c);
            escaped = false;
        } else if matches!(c, ';' | '#') {
            break;
        } else if c == '\\' {
            escaped = true;
        } else {
            out.push(c);
        }
    }
    if escaped {
        out.push('\\');
    }
    out
}

/// Whether `JSON.parse` would turn `text` into something, which for single-quoted text
/// replaces it.
fn reads_as_json(text: &str) -> bool {
    let text = js_trim(text);
    matches!(text, "true" | "false" | "null")
        || text.starts_with(['[', '{', '-'])
        || text.starts_with(|c: char| c.is_ascii_digit())
}

/// `text` without the white space JavaScript's `trim` removes, a byte order mark included.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// What a reference to an unset variable without `?` becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unset {
    /// It stays as written, as npm leaves it in a key.
    Keep,
    /// It makes the whole value unknown, since npm would read the reference as a path.
    Refuse,
}

/// `text` with each `${NAME}` replaced by the variable's value and each `${NAME?}` by the
/// value or nothing, as `env-replace.js` does. `None` when a `\` comes before a reference,
/// a value isn't text, or a reference to an unset variable is to be refused.
fn expand(text: &str, env: &Environment, unset: Unset) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        let (before, from) = rest.split_at(start);
        if before.ends_with('\\') {
            return None;
        }
        out.push_str(before);
        let body = from.get(2..)?;
        let Some((name, optional, after)) = reference(body) else {
            out.push_str("${");
            rest = body;
            continue;
        };
        match (exact_var(env, name), optional, unset) {
            (Some(value), _, _) => out.push_str(value.to_str()?),
            (None, true, _) => {}
            (None, false, Unset::Keep) => out.push_str(from.get(..from.len() - after.len())?),
            (None, false, Unset::Refuse) => return None,
        }
        rest = after;
    }
    out.push_str(rest);
    Some(out)
}

/// The name of a reference at the start of `body` (after `${`), whether it ends in `?`,
/// and what follows its `}`.
fn reference(body: &str) -> Option<(&str, bool, &str)> {
    let end = body.find(['$', '{', '}', '?'])?;
    let name = body.get(..end).filter(|name| !name.is_empty())?;
    let after = body.get(end..)?;
    if let Some(after) = after.strip_prefix("?}") {
        return Some((name, true, after));
    }
    after.strip_prefix('}').map(|after| (name, false, after))
}

/// The variable named exactly `name`, empty or not, as Node's `process.env` gives it.
fn exact_var<'env>(env: &'env Environment, name: &str) -> Option<&'env OsStr> {
    env.vars()
        .find(|var| os::env_name_matches(var.name, name))
        .map(|var| var.value)
}

/// `raw` as the absolute path npm reads for `key`: trimmed, references expanded, `~/`
/// below the home, then folded. Anything else is an error naming `label` and `key` only.
fn path_of(
    raw: &Raw,
    key: Key,
    label: &'static str,
    reading: &Reading<'_>,
) -> Result<PathBuf, UnresolvedRoot> {
    let unplaced = || unusable(label, key.unplaced());
    let Raw::Text(text) = raw else {
        return Err(unplaced());
    };
    let text = expand(js_trim(text), reading.env, Unset::Refuse).ok_or_else(unplaced)?;
    let text = js_trim(&text);
    if !cfg!(windows) && text.contains('\\') {
        return Err(unplaced());
    }
    let separators: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
    let below_home = text
        .strip_prefix('~')
        .and_then(|rest| rest.strip_prefix(separators));
    let path = match below_home {
        Some(rest) => reading
            .home
            .ok_or(UnresolvedRoot::BaseUnresolved {
                base: SymbolicRoot::Home,
            })?
            .join(rest),
        None => PathBuf::from(text),
    };
    if !path.is_absolute() {
        return Err(unplaced());
    }
    Ok(fold(&path))
}

/// `path` with `.` and `..` folded by text, as Node's `path.resolve` folds them.
fn fold(path: &Path) -> PathBuf {
    let mut folded = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                folded.push(component);
            }
        }
    }
    folded
}
