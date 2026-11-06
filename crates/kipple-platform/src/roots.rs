//! Symbolic roots, resolved upstream-first with their sources (`docs/05-architecture.md`
//! §1), and kipple's own directories (`docs/03-functional-spec.md` §6, D-037).

mod agent_config;
mod tool_path;

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kipple_core::{
    LinkProblem, Resolution, ResolvedRoot, RootLink, RootSource, Roots, SymbolicRoot,
    UnresolvedRoot,
};

use self::agent_config::ConfigKey;
use self::tool_path::Tool;
use crate::environment::Environment;
use crate::fs_probe::exact;
use crate::os;

/// The config file's name inside kipple's config directory.
pub const CONFIG_FILE: &str = "config.toml";

/// The default project roots below home, whichever exist (`docs/03-functional-spec.md`
/// §6).
const PROJECT_ROOTS: [&str; 10] = [
    "projects",
    "Projects",
    "code",
    "src",
    "dev",
    "work",
    "repos",
    "git",
    "Developer",
    "source/repos",
];

/// A root at a fixed place below another root of this OS.
#[derive(Debug, Clone, Copy)]
struct Placed {
    root: SymbolicRoot,
    base: SymbolicRoot,
    relative: &'static str,
}

/// Agents' stores that only their tool's default path places. One whose `base` this OS
/// doesn't have is absent here, as XDG roots are on Windows.
const AGENT_ROOTS: [Placed; 11] = [
    Placed {
        root: SymbolicRoot::ClaudeHome,
        base: SymbolicRoot::Home,
        relative: ".claude",
    },
    // With `$CLAUDE_CONFIG_DIR` set the live file is inside `claude.config`, which is closed
    // too, so this is the one path no other root covers.
    Placed {
        root: SymbolicRoot::ClaudeConfigFile,
        base: SymbolicRoot::Home,
        relative: ".claude.json",
    },
    Placed {
        root: SymbolicRoot::ClaudeVersions,
        base: SymbolicRoot::XdgData,
        relative: "claude",
    },
    Placed {
        root: SymbolicRoot::ClaudeInstallState,
        base: SymbolicRoot::XdgState,
        relative: "claude",
    },
    Placed {
        root: SymbolicRoot::CodexDefaultHome,
        base: SymbolicRoot::Home,
        relative: ".codex",
    },
    Placed {
        root: SymbolicRoot::PiHome,
        base: SymbolicRoot::Home,
        relative: ".pi",
    },
    Placed {
        root: SymbolicRoot::CursorConfig,
        base: SymbolicRoot::Home,
        relative: ".cursor",
    },
    Placed {
        root: SymbolicRoot::CursorServer,
        base: SymbolicRoot::Home,
        relative: ".cursor-server",
    },
    if cfg!(windows) {
        Placed {
            root: SymbolicRoot::NpmDefaultCache,
            base: SymbolicRoot::LocalAppData,
            relative: "npm-cache",
        }
    } else {
        Placed {
            root: SymbolicRoot::NpmDefaultCache,
            base: SymbolicRoot::Home,
            relative: ".npm",
        }
    },
    if cfg!(windows) {
        Placed {
            root: SymbolicRoot::CursorAppData,
            base: SymbolicRoot::RoamingAppData,
            relative: "Cursor",
        }
    } else if cfg!(target_os = "macos") {
        Placed {
            root: SymbolicRoot::CursorAppData,
            base: SymbolicRoot::Home,
            relative: "Library/Application Support/Cursor",
        }
    } else {
        Placed {
            root: SymbolicRoot::CursorAppData,
            base: SymbolicRoot::Home,
            relative: ".config/Cursor",
        }
    },
    if cfg!(windows) {
        Placed {
            root: SymbolicRoot::CursorCli,
            base: SymbolicRoot::LocalAppData,
            relative: "cursor-agent",
        }
    } else {
        Placed {
            root: SymbolicRoot::CursorCli,
            base: SymbolicRoot::Home,
            relative: ".local/share/cursor-agent",
        }
    },
];

/// Where agents keep git worktrees outside any repository. Claude Code's and Gemini CLI's
/// are inside one, so project roots reach them. Each is `git.agent-locations` only where it
/// exists.
const AGENT_LOCATIONS: [(SymbolicRoot, &str); 5] = [
    (SymbolicRoot::CodexHome, "worktrees"),
    (SymbolicRoot::Home, ".cursor/worktrees"),
    (SymbolicRoot::Home, "conductor/workspaces"),
    (SymbolicRoot::Home, ".herdr/worktrees"),
    (SymbolicRoot::XdgData, "opencode/worktree"),
];

/// Codex's config files that can set `sqlite_home`, in its home.
const CODEX_HOME_CONFIG: [&str; 3] = ["config.toml", "managed_config.toml", "requirements.toml"];

/// The same, in the system's own configuration.
const CODEX_SYSTEM_CONFIG: [&str; 3] = [
    "/etc/codex/config.toml",
    "/etc/codex/managed_config.toml",
    "/etc/codex/requirements.toml",
];

/// Resolves every root this OS has. `project_roots`, when given (`--root`), replace the
/// default project roots; none given means the defaults.
///
/// A malformed explicit setting leaves its root unresolved; it never falls back to the
/// default. A root whose own path is a link is resolved to where the link leads (see
/// [`follow_root_links`]).
#[must_use]
pub fn resolve_roots(env: &Environment, project_roots: &[PathBuf]) -> Roots {
    if project_roots.is_empty() {
        return each_path_once(&follow_root_links(&upstream_roots(env)));
    }
    resolve_roots_from(env, project_roots, RootSource::CommandLine)
}

/// The same, with `project_roots` given by `source`, such as the config's
/// `scan.project_roots`. They are resolved exactly as `--root` paths are.
///
/// They replace the default project roots even when there are none: an empty list means
/// no project roots.
#[must_use]
pub fn resolve_roots_from(
    env: &Environment,
    project_roots: &[PathBuf],
    source: RootSource,
) -> Roots {
    let mut roots = upstream_roots(env);
    roots.remove(SymbolicRoot::ProjectRoots);
    for path in project_roots {
        roots.add(SymbolicRoot::ProjectRoots, command_line_root(path, source));
    }
    each_path_once(&follow_root_links(&roots))
}

/// A project root given on the command line or in config, below the directory its parent names: the
/// parent's links and `..` resolved, so the same directory given two ways is one root and
/// nothing below it has a `..` component. Its own name is kept, so a root that is itself
/// a link is resolved once, keeping its identity (D-041). A root that doesn't exist is
/// unresolved: the user named it, so finding nothing there must not read as clean.
fn command_line_root(path: &Path, source: RootSource) -> Result<ResolvedRoot, UnresolvedRoot> {
    if !path.is_absolute() {
        return Err(UnresolvedRoot::NotAbsolute {
            var: "--root",
            value: path.to_string_lossy().into_owned(),
        });
    }
    let resolved = in_physical_parent(path).and_then(|resolved| {
        match fs::symlink_metadata(exact(&resolved)) {
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => Err(e),
            // Anything else is reported when the root is opened.
            _ => Ok(resolved),
        }
    });
    let resolved = resolved.map_err(|e| UnresolvedRoot::Unresolvable {
        path: path.to_owned(),
        source: Arc::new(e),
    })?;
    let root = SymbolicRoot::ProjectRoots;
    Ok(ResolvedRoot::new(root, resolved, source))
}

/// An absolute `path` below the directory its parent names, the parent's links and `..`
/// resolved. Its own name is kept, so when it is a link [`follow_root_links`] resolves it
/// and records its identity (D-041).
pub(crate) fn in_physical_parent(path: &Path) -> io::Result<PathBuf> {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => os::physical(parent).map(|parent| parent.join(name)),
        // The file system root, or a path ending in `..`.
        _ => os::physical(path),
    }
}

/// `roots` with each resolved path of a root once, such as two default project roots
/// that are links to the same directory. Whichever comes first, the one kept carries a
/// link's identity check when either has one.
fn each_path_once(roots: &Roots) -> Roots {
    let mut kept: Vec<(SymbolicRoot, Result<ResolvedRoot, UnresolvedRoot>)> = Vec::new();
    for Resolution {
        root,
        resolved: resolution,
    } in roots.iter()
    {
        if let Ok(resolved) = resolution {
            let earlier = kept.iter_mut().find_map(|(r, k)| {
                k.as_mut()
                    .ok()
                    .filter(|k| *r == root && k.path == resolved.path)
            });
            if earlier.is_some_and(|earlier| absorbed(earlier, resolved)) {
                continue;
            }
        }
        kept.push((root, resolution.clone()));
    }
    let mut once = Roots::default();
    for (root, resolution) in kept {
        once.add(root, resolution);
    }
    once
}

/// Whether `resolved` adds nothing to `earlier`, a root at the same path, once `earlier`
/// carries whichever link identity check either has. Two different identities are both
/// kept, so the scan re-checks each.
fn absorbed(earlier: &mut ResolvedRoot, resolved: &ResolvedRoot) -> bool {
    match (&earlier.link, &resolved.link) {
        (_, None) => true,
        (None, Some(_)) => {
            resolved.clone_into(earlier);
            true
        }
        (Some(first), Some(second)) => first.identity == second.identity,
    }
}

/// Every root as the OS and the tools' own conventions place it.
fn upstream_roots(env: &Environment) -> Roots {
    #[cfg(feature = "test-roots")]
    if let Some(file) = crate::test_host::file(env) {
        return crate::test_host::roots(file);
    }
    let mut roots = os::resolve_roots(env);
    let claude = match absolute(env, "CLAUDE_CONFIG_DIR") {
        Ok(Some(path)) => Ok(ResolvedRoot::new(
            SymbolicRoot::ClaudeConfig,
            path,
            RootSource::Env("CLAUDE_CONFIG_DIR"),
        )),
        Ok(None) => under(
            &roots,
            SymbolicRoot::Home,
            SymbolicRoot::ClaudeConfig,
            ".claude",
        ),
        Err(unusable) => Err(unusable),
    };
    roots.insert(SymbolicRoot::ClaudeConfig, claude);
    agent_roots(env, &mut roots);
    for resolved in default_project_roots(&roots) {
        roots.add(SymbolicRoot::ProjectRoots, resolved);
    }
    roots
}

/// Adds every agent's stores and the places agents keep worktrees.
fn agent_roots(env: &Environment, roots: &mut Roots) {
    for Placed {
        root,
        base,
        relative,
    } in AGENT_ROOTS
    {
        if roots.get(base).is_some() {
            let resolved = under(roots, base, root, relative);
            roots.insert(root, resolved);
        }
    }
    let home = codex_home(env, roots);
    roots.insert(SymbolicRoot::CodexHome, home);
    let store = under(
        roots,
        SymbolicRoot::CodexHome,
        SymbolicRoot::CodexReleaseStore,
        "packages",
    );
    roots.insert(SymbolicRoot::CodexReleaseStore, store);
    for resolved in codex_sqlite_homes(env, roots) {
        roots.add(SymbolicRoot::CodexSqliteHome, resolved);
    }
    let agent = pi_agent(env, roots);
    roots.insert(SymbolicRoot::PiAgent, agent);
    for resolved in pi_session_dirs(env, roots) {
        roots.add(SymbolicRoot::PiSessionDir, resolved);
    }
    for resolved in agent_locations(roots) {
        roots.add(SymbolicRoot::GitAgentLocations, resolved);
    }
}

/// `$CODEX_HOME`, which must be a directory because upstream refuses anything else, then
/// `~/.codex`. Below a link, roots are never followed, so what is derived from this root
/// needs it resolved now.
fn codex_home(env: &Environment, roots: &Roots) -> Result<ResolvedRoot, UnresolvedRoot> {
    let root = SymbolicRoot::CodexHome;
    let var = "CODEX_HOME";
    let Some(path) = absolute(env, var)? else {
        return under(roots, SymbolicRoot::Home, root, ".codex").and_then(follow_link);
    };
    let unresolvable = |source: io::Error| UnresolvedRoot::Unresolvable {
        path: path.clone(),
        source: Arc::new(source),
    };
    let meta = fs::metadata(exact(&path)).map_err(unresolvable)?;
    if !meta.is_dir() {
        return Err(unresolvable(ErrorKind::NotADirectory.into()));
    }
    let physical = in_physical_parent(&path).map_err(unresolvable)?;
    follow_link(ResolvedRoot::new(root, physical, RootSource::Env(var)))
}

/// Where Codex keeps its databases, past its home: each place one of its settings names.
/// Which of them wins needs no modelling, since each is only kept closed.
fn codex_sqlite_homes(
    env: &Environment,
    roots: &Roots,
) -> Vec<Result<ResolvedRoot, UnresolvedRoot>> {
    let root = SymbolicRoot::CodexSqliteHome;
    let var = "CODEX_SQLITE_HOME";
    let mut found = Vec::new();
    if let Some(value) = trimmed(env, var) {
        // Upstream reads a relative value from its working directory, which kipple can't know.
        let path = expanded(value, var, Tool::Codex, roots);
        found.push(path.and_then(|path| configured_dir(root, path, RootSource::Env(var))));
    }
    let home = match roots.get(SymbolicRoot::CodexHome) {
        Some(Ok(home)) => Some(home.path.as_path()),
        Some(Err(_)) | None => None,
    };
    let in_home = CODEX_HOME_CONFIG
        .into_iter()
        .filter_map(|file| Some((file, home?.join(file))));
    let in_system = CODEX_SYSTEM_CONFIG
        .into_iter()
        .map(|file| (file, env.system_path(Path::new(file))));
    for (file, path) in in_home.chain(in_system) {
        let key = ConfigKey {
            file,
            key: "sqlite_home",
            tool: Tool::Codex,
        };
        found.extend(setting_dirs(key, &path, root, roots));
    }
    found
}

/// `$PI_CODING_AGENT_DIR` as upstream reads it, then `~/.pi/agent`.
fn pi_agent(env: &Environment, roots: &Roots) -> Result<ResolvedRoot, UnresolvedRoot> {
    let root = SymbolicRoot::PiAgent;
    let var = "PI_CODING_AGENT_DIR";
    match env.get(var) {
        Some(value) => {
            let path = expanded(value, var, Tool::Pi, roots)?;
            configured_dir(root, path, RootSource::Env(var))
        }
        None => under(roots, SymbolicRoot::Home, root, ".pi/agent"),
    }
}

/// Pi's session directories, past its agent directory: the variable's and the global
/// setting's. A project's setting and `--session-dir` can't be known.
fn pi_session_dirs(env: &Environment, roots: &Roots) -> Vec<Result<ResolvedRoot, UnresolvedRoot>> {
    let root = SymbolicRoot::PiSessionDir;
    let var = "PI_CODING_AGENT_SESSION_DIR";
    let mut found = Vec::new();
    if let Some(value) = env.get(var) {
        let path = expanded(value, var, Tool::Pi, roots);
        found.push(path.and_then(|path| configured_dir(root, path, RootSource::Env(var))));
    }
    if let Some(Ok(agent)) = roots.get(SymbolicRoot::PiAgent) {
        let key = ConfigKey {
            file: "settings.json",
            key: "sessionDir",
            tool: Tool::Pi,
        };
        found.extend(setting_dirs(key, &agent.path.join(key.file), root, roots));
    }
    found
}

/// The directories `key` names in the file at `path`, each as `root`.
fn setting_dirs(
    key: ConfigKey,
    path: &Path,
    root: SymbolicRoot,
    roots: &Roots,
) -> Vec<Result<ResolvedRoot, UnresolvedRoot>> {
    let values = match key.values(path) {
        Ok(values) => values,
        Err(unusable) => return vec![Err(unusable)],
    };
    let source = RootSource::Setting {
        file: key.file,
        key: key.key,
    };
    values
        .iter()
        .map(|value| {
            let value = value.as_os_str();
            let mut dir = decoded(value, key.tool, roots)?;
            // Codex reads a relative value from the file's directory. Pi reads it from its
            // working directory, which kipple can't know, so that one stays refused.
            if let (Tool::Codex, Some(beside)) = (key.tool, path.parent()) {
                dir = beside.join(dir);
            }
            configured_dir(root, in_full(dir, value, key.key)?, source)
        })
        .collect()
}

/// The value of `var` with the white space around it gone, `None` when nothing is left.
fn trimmed<'env>(env: &'env Environment, var: &str) -> Option<&'env OsStr> {
    let value = env.get(var)?;
    let value = value.to_str().map_or(value, |text| OsStr::new(text.trim()));
    (!value.is_empty()).then_some(value)
}

/// `value` as an absolute path, read the way `tool` reads it.
fn expanded(
    value: &OsStr,
    var: &'static str,
    tool: Tool,
    roots: &Roots,
) -> Result<PathBuf, UnresolvedRoot> {
    in_full(decoded(value, tool, roots)?, value, var)
}

/// What `tool` means by `value`, which may still be relative.
fn decoded(value: &OsStr, tool: Tool, roots: &Roots) -> Result<PathBuf, UnresolvedRoot> {
    let Some(text) = value.to_str() else {
        return Ok(PathBuf::from(value));
    };
    let Some(rest) = tool.below_home(text, os::PLATFORM) else {
        return Ok(tool.plain(text, os::PLATFORM));
    };
    let Some(Ok(home)) = roots.get(SymbolicRoot::Home) else {
        let base = SymbolicRoot::Home;
        return Err(UnresolvedRoot::BaseUnresolved { base });
    };
    Ok(home.path.join(rest))
}

/// `path` when it is absolute. `value` and `var` say what gave it, for the error.
fn in_full(path: PathBuf, value: &OsStr, var: &'static str) -> Result<PathBuf, UnresolvedRoot> {
    if path.is_absolute() {
        return Ok(path);
    }
    Err(UnresolvedRoot::NotAbsolute {
        var,
        value: value.to_string_lossy().into_owned(),
    })
}

/// A directory a person configured, below the directory its parent names. One that doesn't
/// exist yet is an absent root, because its tool makes it on first run; something else
/// there is an error, since the tool couldn't use it either.
fn configured_dir(
    root: SymbolicRoot,
    path: PathBuf,
    source: RootSource,
) -> Result<ResolvedRoot, UnresolvedRoot> {
    let unresolvable = |error: io::Error| UnresolvedRoot::Unresolvable {
        path: path.clone(),
        source: Arc::new(error),
    };
    match fs::symlink_metadata(exact(&path)) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            Ok(ResolvedRoot::new(root, path, source))
        }
        Err(error) if error.kind() == ErrorKind::NotADirectory => Err(unresolvable(error)),
        Ok(meta) if !meta.is_dir() && !meta.file_type().is_symlink() => {
            Err(unresolvable(ErrorKind::NotADirectory.into()))
        }
        // A link is resolved, and anything unreadable reported, later.
        Ok(_) | Err(_) => {
            let physical = in_physical_parent(&path).map_err(unresolvable)?;
            Ok(ResolvedRoot::new(root, physical, source))
        }
    }
}

/// The places agents keep worktrees that exist here. One whose base is unknown stays in
/// the list as unknown: a location kipple can't place must not read as an empty one.
fn agent_locations(roots: &Roots) -> Vec<Result<ResolvedRoot, UnresolvedRoot>> {
    let root = SymbolicRoot::GitAgentLocations;
    AGENT_LOCATIONS
        .into_iter()
        .filter(|(base, _)| roots.get(*base).is_some())
        .map(|(base, relative)| under(roots, base, root, relative))
        .filter(|location| location.as_ref().map_or(true, may_exist))
        .collect()
}

/// Whether a directory, or a link to one, may be at `resolved`: reading it says why not.
fn may_exist(resolved: &ResolvedRoot) -> bool {
    match fs::symlink_metadata(exact(&resolved.path)) {
        Ok(meta) => meta.is_dir() || meta.file_type().is_symlink(),
        Err(e) => !matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory),
    }
}

/// The default project roots that exist, each directory once even where the file system
/// ignores case and `~/projects` is `~/Projects`.
fn default_project_roots(roots: &Roots) -> Vec<Result<ResolvedRoot, UnresolvedRoot>> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for relative in PROJECT_ROOTS {
        let resolved = under(
            roots,
            SymbolicRoot::Home,
            SymbolicRoot::ProjectRoots,
            relative,
        );
        let Ok(resolved) = resolved else {
            // Home is unknown, so the project roots are too.
            return vec![resolved];
        };
        let meta = match fs::symlink_metadata(exact(&resolved.path)) {
            Ok(meta) => meta,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                continue;
            }
            // It may exist: opening it reports why it can't be read.
            Err(_) => {
                found.push(Ok(resolved));
                continue;
            }
        };
        // A link is resolved, or reported as unusable, by `follow_root_links`.
        if meta.file_type().is_symlink() {
            found.push(Ok(resolved));
            continue;
        }
        let identity = os::identity(&resolved.path, &meta);
        if meta.is_dir() && identity.is_none_or(|identity| seen.insert(identity)) {
            found.push(Ok(resolved));
        }
    }
    found
}

/// `root` as the default `relative` path (`/`-separated) under the root `base`.
pub(crate) fn under(
    roots: &Roots,
    base: SymbolicRoot,
    root: SymbolicRoot,
    relative: &'static str,
) -> Result<ResolvedRoot, UnresolvedRoot> {
    let Some(Ok(resolved)) = roots.get(base) else {
        return Err(UnresolvedRoot::BaseUnresolved { base });
    };
    let path = relative
        .split('/')
        .fold(resolved.path.clone(), |path, part| path.join(part));
    let source = RootSource::Default { base, relative };
    Ok(ResolvedRoot::new(root, path, source))
}

/// The value of `var` as an absolute path, `None` when unset or empty.
pub(crate) fn absolute(
    env: &Environment,
    var: &'static str,
) -> Result<Option<PathBuf>, UnresolvedRoot> {
    let Some(value) = env.get(var) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Ok(Some(path));
    }
    Err(UnresolvedRoot::NotAbsolute {
        var,
        value: value.to_string_lossy().into_owned(),
    })
}

/// Resolves, once, each root whose configured path is itself a link (D-041).
///
/// The identity of the directory the link leads to is recorded so a scan can re-check
/// it. Links below a root are never followed. A link that can't be followed, or doesn't
/// lead to a directory, leaves its root unresolved.
#[must_use]
pub fn follow_root_links(roots: &Roots) -> Roots {
    let mut followed = Roots::default();
    for Resolution { root, resolved } in roots.iter() {
        followed.add(root, resolved.clone().and_then(follow_link));
    }
    followed
}

fn follow_link(resolved: ResolvedRoot) -> Result<ResolvedRoot, UnresolvedRoot> {
    let configured = exact(&resolved.path);
    // Missing or unreadable is reported when the root is opened, like any root's.
    if !fs::symlink_metadata(&configured).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Ok(resolved);
    }
    let problem = |problem| UnresolvedRoot::Link {
        path: configured.clone(),
        problem,
    };
    let target =
        os::physical(&configured).map_err(|e| problem(LinkProblem::Unresolvable(Arc::new(e))))?;
    let meta = fs::symlink_metadata(&target)
        .map_err(|e| problem(LinkProblem::Unresolvable(Arc::new(e))))?;
    if !meta.is_dir() {
        return Err(problem(LinkProblem::NotDirectory));
    }
    let identity = os::identity(&target, &meta).ok_or_else(|| problem(LinkProblem::NoIdentity))?;
    Ok(ResolvedRoot {
        path: target,
        link: Some(RootLink {
            configured,
            identity,
        }),
        ..resolved
    })
}

/// One of kipple's own directories.
#[derive(Debug, Clone)]
pub struct KippleDir {
    /// The root it lives under, whose source explains where it came from.
    pub base: SymbolicRoot,
    /// Its path, or why its base did not resolve.
    pub path: Result<PathBuf, UnresolvedRoot>,
}

/// kipple's own directories. Every frontend uses the same ones.
#[derive(Debug, Clone)]
pub struct KippleDirs {
    /// Config.
    pub config: KippleDir,
    /// Receipts, packs and grants.
    pub data: KippleDir,
    /// The optional scan cache.
    pub cache: KippleDir,
}

/// kipple's own directories under `roots`.
#[must_use]
pub fn kipple_dirs(roots: &Roots) -> KippleDirs {
    let [config, data, cache] = os::KIPPLE_DIRS.map(|(base, relative)| KippleDir {
        base,
        path: match roots.get(base) {
            Some(Ok(resolved)) => Ok(resolved.path.join(relative)),
            Some(Err(_)) | None => Err(UnresolvedRoot::BaseUnresolved { base }),
        },
    });
    KippleDirs {
        config,
        data,
        cache,
    }
}
