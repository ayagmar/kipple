//! Symbolic roots, resolved upstream-first with their sources (`docs/05-architecture.md`
//! §1), and kipple's own directories (`docs/03-functional-spec.md` §6, D-037).

use std::collections::HashSet;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kipple_core::{
    LinkProblem, ResolvedRoot, RootLink, RootSource, Roots, SymbolicRoot, UnresolvedRoot,
};

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

/// Resolves every root this OS has. `project_roots`, when given, replace the default
/// project roots, as `--root` does.
///
/// A malformed explicit setting leaves its root unresolved; it never falls back to the
/// default. A root whose own path is a link is resolved to where the link leads (see
/// [`follow_root_links`]).
#[must_use]
pub fn resolve_roots(env: &Environment, project_roots: &[PathBuf]) -> Roots {
    resolve_roots_from(env, project_roots, RootSource::CommandLine)
}

/// The same, with `project_roots` given by `source`, such as the config's
/// `scan.project_roots`. They are resolved exactly as `--root` paths are.
#[must_use]
pub fn resolve_roots_from(
    env: &Environment,
    project_roots: &[PathBuf],
    source: RootSource,
) -> Roots {
    let mut roots = upstream_roots(env);
    if !project_roots.is_empty() {
        roots.remove(SymbolicRoot::ProjectRoots);
        for path in project_roots {
            roots.add(SymbolicRoot::ProjectRoots, command_line_root(path, source));
        }
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
    for (root, resolution) in roots.iter() {
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
    for resolved in default_project_roots(&roots) {
        roots.add(SymbolicRoot::ProjectRoots, resolved);
    }
    roots
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
    for (root, resolution) in roots.iter() {
        followed.add(root, resolution.clone().and_then(follow_link));
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
