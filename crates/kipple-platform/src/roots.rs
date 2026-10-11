//! Symbolic roots, resolved upstream-first with their sources (`docs/05-architecture.md`
//! §1), and kipple's own directories (`docs/03-functional-spec.md` §6, D-037).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use kipple_core::{LinkProblem, ResolvedRoot, RootLink, Roots, SymbolicRoot, UnresolvedRoot};

use crate::environment::Environment;
use crate::fs_probe::exact;
use crate::os;

/// The config file's name inside kipple's config directory.
pub const CONFIG_FILE: &str = "config.toml";

/// Resolves every root this OS has.
///
/// A malformed explicit setting leaves its root unresolved; it never falls back to the
/// default. A root whose own path is a link is resolved to where the link leads (see
/// [`follow_root_links`]).
#[must_use]
pub fn resolve_roots(env: &Environment) -> Roots {
    follow_root_links(&os::resolve_roots(env))
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
        followed.insert(root, resolution.clone().and_then(follow_link));
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
    let target = fs::canonicalize(&configured)
        .map_err(|e| problem(LinkProblem::Unresolvable(Arc::new(e))))?;
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
