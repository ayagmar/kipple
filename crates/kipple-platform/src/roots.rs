//! Symbolic roots, resolved upstream-first with their sources (`docs/05-architecture.md`
//! §1), and kipple's own directories (`docs/03-functional-spec.md` §6, D-037).

use std::path::PathBuf;

use kipple_core::{Roots, SymbolicRoot, UnresolvedRoot};

use crate::environment::Environment;
use crate::os;

/// The config file's name inside kipple's config directory.
pub const CONFIG_FILE: &str = "config.toml";

/// Resolves every root this OS has. A malformed explicit setting leaves its root
/// unresolved; it never falls back to the default.
#[must_use]
pub fn resolve_roots(env: &Environment) -> Roots {
    os::resolve_roots(env)
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
