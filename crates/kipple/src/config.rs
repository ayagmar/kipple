//! kipple's config file (`docs/03-functional-spec.md` §6, D-051): where it is, what it
//! says, the [`Policy`] and project roots it gives, and edits to it.
//!
//! Only a config file missing at the default location means the defaults. Anything else
//! that is wrong with it is an error with its line and column, and the commands that
//! scan exit with code 2.

mod edit;
mod keys;
mod parse;
mod ui;

use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};
use std::path::{self, Component, Path, PathBuf};

use kipple_core::{
    FileIdentity, FsProbe as _, HeldPath, Level, Lineage, Method, Policy, Resolution, RootSource,
    Roots, RuleId, RuleSettings, SymbolicRoot, is_within,
};
use kipple_platform::{
    CONFIG_FILE, Environment, NoFollowFs, kipple_dirs, read_config, resolve_roots,
    resolve_roots_from,
};

pub use self::edit::{Edit, write};
pub use self::keys::KeysConfig;
pub(crate) use self::parse::position;
pub use self::ui::{DetailPlace, UiConfig};
use crate::human::escaped;
use crate::setup::{Builtin, BuiltinRule};

/// The variable naming a config file.
pub const VAR: &str = "KIPPLE_CONFIG";

/// Where the config file came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `--config FILE`.
    Flag,
    /// `KIPPLE_CONFIG`.
    Env,
    /// `config.toml` in kipple's config directory.
    Default,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            Self::Flag => "--config",
            Self::Env => VAR,
            Self::Default => "the default location",
        })
    }
}

/// The config file kipple reads, and where that choice came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// Where it came from.
    pub source: Source,
    /// The file, or `None` when it is the default one and the config directory is unknown.
    pub path: Option<PathBuf>,
}

/// A rule as config may name it: one of the whole built-in catalog, whatever the
/// platform, so a config written on another OS still loads.
pub type Catalog = BTreeSet<RuleId>;

/// What the config file says, validated. Paths are as written, `~` not yet expanded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// `care.level`.
    pub level: Option<Level>,
    /// `clean.method`: how `clean` and `plan` remove, unless `--method` says otherwise.
    pub method: Option<Method>,
    /// `scan.project_roots`.
    pub project_roots: Option<Vec<String>>,
    /// `exclude.paths`.
    pub exclude_paths: Vec<String>,
    /// `exclude.rules`.
    pub exclude_rules: Vec<RuleId>,
    /// `keep.paths`, each with its note.
    pub keep: Vec<Kept>,
    /// `rules.<id>`.
    pub rules: Vec<RuleConfig>,
    /// `review.confirm_by_typing_above`, in bytes: above it, a person types the size to
    /// confirm a removal.
    pub confirm_by_typing_above: Option<u64>,
    /// `[ui]`.
    pub ui: UiConfig,
    /// `[keys]`.
    pub keys: KeysConfig,
}

/// A path in `keep.paths`, as written, and its note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    /// The path.
    pub path: String,
    /// Why it is kept.
    pub note: Option<String>,
}

/// A `rules.<id>` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleConfig {
    /// The rule.
    pub id: RuleId,
    /// What the table sets for it.
    pub settings: RuleSettings,
}

/// A place in a TOML file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// The line, from 1.
    pub line: usize,
    /// The column in characters, from 1.
    pub column: usize,
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}", self.line, self.column)
    }
}

/// A config that loaded: where it is, whether a file was there, and what it says.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// The file.
    pub location: Location,
    /// Whether a file was there. Without one, the defaults apply.
    pub exists: bool,
    /// What it says.
    pub config: Config,
}

/// Why the config can't be used. Shown with its source; every command that needs a
/// config exits with code 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// Where the file was looked for.
    pub location: Location,
    /// Where in the file what is wrong is, when it is inside the file.
    pub position: Option<Position>,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.location.path {
            Some(path) => write!(f, "config file {}", path.display())?,
            None => write!(f, "config file")?,
        }
        write!(f, " (from {})", self.location.source)?;
        if let Some(position) = self.position {
            write!(f, ", {position}")?;
        }
        write!(f, ": {}", self.message)
    }
}

/// What a scan runs with, from the config and the command line.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The config it came from.
    pub loaded: Loaded,
    /// Every root, the project roots from `--root`, else the config, else the defaults.
    pub roots: Roots,
    /// The policy every verdict comes from.
    pub policy: Policy,
    /// A line for each kept or excluded path that doesn't exist or can't be checked.
    pub warnings: Vec<String>,
}

impl Settings {
    /// Loads the config `flag` or the environment names, or the default one, and resolves
    /// the roots with the project roots `given` on the command line, if any, else the
    /// config's.
    ///
    /// # Errors
    /// When the config can't be used ([`load`]), or names a project root under an
    /// unknown home directory.
    pub fn load(
        env: &Environment,
        flag: Option<&Path>,
        given: &[PathBuf],
        catalog: &Catalog,
    ) -> Result<Self, ConfigError> {
        let upstream = resolve_roots(env, &[]);
        let location = locate(env, flag, &upstream);
        reload(env, &location, given, catalog)
    }

    /// The settings `loaded` gives, with the project roots `given` on the command line,
    /// if any, else the config's. A caller that has just written the config rebuilds its
    /// settings from what it wrote with this.
    ///
    /// # Errors
    /// When the config names a project root under an unknown home directory.
    pub fn from_loaded(
        env: &Environment,
        loaded: Loaded,
        given: &[PathBuf],
    ) -> Result<Self, ConfigError> {
        let upstream = resolve_roots(env, &[]);
        let configured = loaded
            .config
            .project_roots(&upstream)
            .map_err(|message| ConfigError {
                location: loaded.location.clone(),
                position: None,
                message,
            })?;
        let roots = match (given, configured) {
            ([], None) => upstream,
            ([], Some(configured)) => {
                let source = RootSource::Setting {
                    file: CONFIG_FILE,
                    key: "scan.project_roots",
                };
                resolve_roots_from(env, &configured, source)
            }
            (given, _) => resolve_roots(env, given),
        };
        let ConfigPolicy { policy, warnings } = loaded.config.policy(&roots);
        Ok(Self {
            loaded,
            roots,
            policy,
            warnings,
        })
    }
}

/// Every built-in rule's ID, on every platform.
#[must_use]
pub fn catalog(builtin: &Builtin) -> Catalog {
    builtin
        .rules()
        .map(|BuiltinRule { rule, .. }| rule.id.clone())
        .collect()
}

/// The settings the config at `location` gives now.
///
/// The project roots are those `given` on the command line, if any. It is what a screen
/// open for a while confirms under: the file read again from where it was found (spec
/// M3c pinned 6).
///
/// # Errors
/// When the config no longer loads ([`load`]), or names a project root under an unknown
/// home directory.
pub fn reload(
    env: &Environment,
    location: &Location,
    given: &[PathBuf],
    catalog: &Catalog,
) -> Result<Settings, ConfigError> {
    let loaded = load(location, catalog)?;
    Settings::from_loaded(env, loaded, given)
}

/// Which config file to read: `--config`, then `KIPPLE_CONFIG`, then `config.toml` in
/// the config directory of `roots`. A relative file is taken from the current directory.
#[must_use]
pub fn locate(env: &Environment, flag: Option<&Path>, roots: &Roots) -> Location {
    let named = |source, path: &Path| Location {
        source,
        path: Some(path::absolute(path).unwrap_or_else(|_| path.to_owned())),
    };
    if let Some(path) = flag {
        return named(Source::Flag, path);
    }
    if let Some(path) = env.get(VAR) {
        return named(Source::Env, Path::new(path));
    }
    Location {
        source: Source::Default,
        path: kipple_dirs(roots)
            .config
            .path
            .ok()
            .map(|dir| dir.join(CONFIG_FILE)),
    }
}

/// Reads and validates the config at `location`. Only nothing at all at the default
/// location gives the defaults.
///
/// # Errors
/// A named file that is missing or can't be read, an unknown config directory, a file
/// that isn't a regular UTF-8 file, invalid TOML, or a key or value kipple refuses.
pub fn load(location: &Location, catalog: &Catalog) -> Result<Loaded, ConfigError> {
    let error = |position, message: String| ConfigError {
        location: location.clone(),
        position,
        message,
    };
    let Some(path) = &location.path else {
        let message = "kipple's config directory is unknown; name a file with --config or \
                       KIPPLE_CONFIG";
        return Err(error(None, message.to_owned()));
    };
    let text = match read_config(path) {
        Ok(Some(text)) => text,
        Ok(None) if location.source == Source::Default => {
            return Ok(Loaded {
                location: location.clone(),
                exists: false,
                config: Config::default(),
            });
        }
        Ok(None) => return Err(error(None, "there is no such file".to_owned())),
        Err(e) => return Err(error(None, format!("it can't be read: {e}"))),
    };
    let config = parse::parse(&text, catalog).map_err(|e| error(e.position, e.message))?;
    Ok(Loaded {
        location: location.clone(),
        exists: true,
        config,
    })
}

impl Config {
    /// The project roots it names, `~` expanded from `roots`' home. `None` when it has no
    /// `scan.project_roots`, so the defaults apply; an empty list means none.
    ///
    /// # Errors
    /// When a root starts with `~` and the home directory is unknown.
    pub fn project_roots(&self, roots: &Roots) -> Result<Option<Vec<PathBuf>>, String> {
        let Some(written) = &self.project_roots else {
            return Ok(None);
        };
        let home = home(roots);
        written
            .iter()
            .map(|path| {
                expand(path, home).ok_or_else(|| {
                    format!(
                        "scan.project_roots: {path} starts with ~, and the home directory is \
                         unknown"
                    )
                })
            })
            .collect::<Result<_, _>>()
            .map(Some)
    }

    /// The policy it gives over `roots`, and a warning for each kept or excluded path that
    /// doesn't exist or can't be checked.
    #[must_use]
    pub fn policy(&self, roots: &Roots) -> ConfigPolicy {
        let mut warnings = Vec::new();
        let mut held = |key: &str, path: &str, note: Option<&String>| {
            let entry = held_path(path, roots, note.cloned());
            if let Some(problem) = check(&entry) {
                warnings.push(format!("{key}: {path} {problem}"));
            }
            entry
        };
        let keep = self
            .keep
            .iter()
            .map(|kept| held("keep.paths", &kept.path, kept.note.as_ref()))
            .collect();
        let exclude = self
            .exclude_paths
            .iter()
            .map(|path| held("exclude.paths", path, None))
            .collect();
        let mut policy = Policy {
            level: self.level.unwrap_or(Level::Balanced),
            keep,
            exclude,
            ..Policy::default()
        };
        for rule in &self.rules {
            policy.rules.insert(rule.id.clone(), rule.settings);
        }
        for rule in &self.exclude_rules {
            policy.rules.entry(rule.clone()).or_default().disabled = true;
        }
        ConfigPolicy { policy, warnings }
    }
}

/// The policy a config gives, and a warning for each kept or excluded path that doesn't
/// exist or can't be checked.
#[derive(Debug, Clone)]
pub struct ConfigPolicy {
    /// The policy.
    pub policy: Policy,
    /// The warnings.
    pub warnings: Vec<String>,
}

/// The resolved home directory, when it is known.
pub(crate) fn home(roots: &Roots) -> Option<&Path> {
    match roots.get(SymbolicRoot::Home) {
        Some(Ok(resolved)) => Some(&resolved.path),
        Some(Err(_)) | None => None,
    }
}

/// `path` with a leading `~` replaced by `home`. `None` when it needs a home that is
/// unknown.
fn expand(path: &str, home: Option<&Path>) -> Option<PathBuf> {
    let Some(rest) = path.strip_prefix('~') else {
        return Some(PathBuf::from(path));
    };
    let rest = rest.trim_start_matches(path::is_separator);
    home.map(|home| home.join(rest))
}

/// A kept or excluded path in every form a finding may take: as written, and below the
/// directory a linked root leads to, with the identities of what is there and above it.
fn held_path(path: &str, roots: &Roots, note: Option<String>) -> HeldPath {
    let Some(expanded) = expand(path, home(roots)) else {
        return HeldPath {
            note,
            ..HeldPath::default()
        };
    };
    HeldPath {
        note,
        ..held_at(&expanded, roots)
    }
}

/// `path` held in every form a finding may take: as given, and below the directory a
/// linked root leads to, with the identities of what is there and above it.
pub(crate) fn held_at(path: &Path, roots: &Roots) -> HeldPath {
    let expanded = path.to_owned();
    let mut forms = vec![expanded.clone()];
    for Resolution { resolved, .. } in roots.iter() {
        let Ok(resolved) = resolved else { continue };
        let Some(link) = &resolved.link else { continue };
        if let Ok(rest) = expanded.strip_prefix(&link.configured) {
            forms.push(resolved.path.join(rest));
        }
    }
    let lineage = Lineage(expanded.ancestors().map(identity).collect());
    HeldPath {
        forms,
        lineage,
        note: None,
    }
}

fn identity(path: &Path) -> Option<FileIdentity> {
    NoFollowFs
        .metadata(path)
        .ok()
        .and_then(|meta| meta.identity)
}

/// What is wrong with a kept or excluded path: nothing there, or it can't be checked.
fn check(entry: &HeldPath) -> Option<String> {
    let Some(path) = entry.forms.first() else {
        return Some("starts with ~, and the home directory is unknown".to_owned());
    };
    match NoFollowFs.metadata(path) {
        Ok(_) => None,
        Err(e) if e.io_kind() == Some(io::ErrorKind::NotFound) => Some("doesn't exist".to_owned()),
        Err(e) => Some(format!("can't be checked: {e}")),
    }
}

/// Whether `path` is absolute or starts with `~`, as a config path must.
pub(crate) fn is_config_path(path: &str) -> bool {
    let tilde = path
        .strip_prefix('~')
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(path::is_separator));
    tilde || Path::new(path).is_absolute()
}

/// Whether two paths are the same, comparing components and ignoring case.
fn same_path(a: &Path, b: &Path) -> bool {
    let fold = |c: Component<'_>| c.as_os_str().to_string_lossy().to_lowercase();
    a.components().map(fold).eq(b.components().map(fold))
}

/// Whether two paths as config writes them name the same place, the way kept and
/// excluded paths are matched: `~` expanded from `home`, then by components, ignoring
/// case and a trailing separator.
pub(crate) fn same_config_path(a: &Path, b: &Path, home: Option<&Path>) -> bool {
    same_path(&full_path(a, home), &full_path(b, home))
}

/// Whether `path` is `base` or inside it, both as config writes them, compared as
/// [`same_config_path`] compares them.
pub(crate) fn within_config_path(path: &Path, base: &Path, home: Option<&Path>) -> bool {
    is_within(&full_path(path, home), &full_path(base, home))
}

/// `path` with `~` expanded from `home`, or as it is when it can't be.
fn full_path(path: &Path, home: Option<&Path>) -> PathBuf {
    expand(&path.to_string_lossy(), home).unwrap_or_else(|| path.to_owned())
}

/// `kipple config path`: the file, when its directory is known.
///
/// # Errors
/// When writing to `out` fails.
pub fn write_path(out: &mut dyn Write, location: &Location) -> io::Result<()> {
    if let Some(path) = &location.path {
        writeln!(out, "{}", escaped(&path.display().to_string()))?;
    }
    out.flush()
}

/// `kipple config show`: where the config is and, when it loaded, every setting it gives
/// with the defaults filled in, and the `[keys]` entries that change a default.
///
/// # Errors
/// When writing to `out` fails.
pub fn write_show(
    out: &mut dyn Write,
    location: &Location,
    loaded: Option<&Loaded>,
) -> io::Result<()> {
    let file = location
        .path
        .as_ref()
        .map_or_else(|| "unknown".to_owned(), |path| path.display().to_string());
    let header = format!("{file} (from {})", location.source);
    show_line(out, "config file", escaped(&header))?;
    let Some(loaded) = loaded else {
        return out.flush();
    };
    let config = &loaded.config;
    if !loaded.exists {
        show_line(out, "", "not present, the defaults apply")?;
    }
    let level = config.level.unwrap_or(Level::Balanced);
    let method = config.method.map_or("delete (default)", |m| m.name());
    show_line(out, "care.level", level)?;
    show_line(out, "clean.method", method)?;
    let roots = config.project_roots.as_ref().map_or_else(
        || "the defaults that exist".to_owned(),
        |roots| roots.join(", "),
    );
    show_line(out, "scan.project_roots", escaped(&roots))?;
    for path in &config.exclude_paths {
        show_line(out, "exclude.paths", escaped(path))?;
    }
    for rule in &config.exclude_rules {
        show_line(out, "exclude.rules", rule)?;
    }
    for Kept { path, note } in &config.keep {
        let line = note
            .as_ref()
            .map_or_else(|| path.clone(), |note| format!("{path}  ({note})"));
        show_line(out, "keep.paths", escaped(&line))?;
    }
    for RuleConfig { id: rule, settings } in &config.rules {
        let mut parts = Vec::new();
        if settings.disabled {
            parts.push("enabled = false".to_owned());
        }
        if let Some(own) = settings.level {
            parts.push(format!("level = {own}"));
        }
        if let Some(days) = settings.recent_days {
            parts.push(format!("recent_days = {days}"));
        }
        show_line(out, &format!("rules.{rule}"), parts.join(", "))?;
    }
    if let Some(above) = config.confirm_by_typing_above {
        show_line(
            out,
            "review.confirm_by_typing_above",
            crate::human::receipt::threshold(above),
        )?;
    }
    config.ui.write_show(out)?;
    config.keys.write_show(out)?;
    out.flush()
}

/// One line of `kipple config show`: the key in its column, then always a space before
/// the value, however long the key.
fn show_line(out: &mut dyn Write, key: &str, value: impl fmt::Display) -> io::Result<()> {
    writeln!(out, "{key:<21} {value}")
}
