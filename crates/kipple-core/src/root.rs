//! Symbolic roots and where each one resolved from (`docs/05-architecture.md` §1).

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

/// A location kipple knows by meaning, not by path. The platform resolves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SymbolicRoot {
    /// The user's home directory.
    Home,
    /// `$XDG_CONFIG_HOME`, default `~/.config`.
    XdgConfig,
    /// `$XDG_CACHE_HOME`, default `~/.cache`.
    XdgCache,
    /// `$XDG_DATA_HOME`, default `~/.local/share`.
    XdgData,
    /// `$XDG_STATE_HOME`, default `~/.local/state`.
    XdgState,
    /// The Windows Known Folder `LocalAppData`.
    LocalAppData,
    /// The Windows Known Folder `RoamingAppData`.
    RoamingAppData,
    /// `~/Library/Caches` on macOS.
    MacosCaches,
}

impl SymbolicRoot {
    /// The name rules and reports use for this root.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::XdgConfig => "xdg.config",
            Self::XdgCache => "xdg.cache",
            Self::XdgData => "xdg.data",
            Self::XdgState => "xdg.state",
            Self::LocalAppData => "known.local-app-data",
            Self::RoamingAppData => "known.roaming-app-data",
            Self::MacosCaches => "macos.caches",
        }
    }
}

impl fmt::Display for SymbolicRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Where a resolved root's path came from, so `doctor` can explain it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSource {
    /// An environment variable the upstream convention defines.
    Env(&'static str),
    /// The convention's default, relative to another root.
    Default {
        /// The root the default is relative to.
        base: SymbolicRoot,
        /// The path below `base`.
        relative: &'static str,
    },
    /// A Windows Known Folder, by its `FOLDERID_` name.
    KnownFolder(&'static str),
}

/// A symbolic root with the path it resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRoot {
    /// Which root this is.
    pub root: SymbolicRoot,
    /// Its absolute path. Nothing has checked that it exists.
    pub path: PathBuf,
    /// Where the path came from.
    pub source: RootSource,
}

/// Why a root has no path. Every one of these makes the root unknown: kipple never
/// falls back to a default when an explicit setting is unusable.
#[derive(Debug, Clone, thiserror::Error)]
pub enum UnresolvedRoot {
    /// The variable that defines the root is not set and has no default.
    #[error("{var} is not set")]
    NotSet {
        /// The variable.
        var: &'static str,
    },
    /// An explicit setting is not an absolute path.
    #[error("{var} is not an absolute path ({value:?})")]
    NotAbsolute {
        /// The variable.
        var: &'static str,
        /// Its value, lossily decoded.
        value: String,
    },
    /// The root is relative to another root that did not resolve.
    #[error("it is relative to {base}, which did not resolve")]
    BaseUnresolved {
        /// The root it depends on.
        base: SymbolicRoot,
    },
    /// The operating system could not say where the root is.
    #[error("the system could not resolve {folder}: {source}")]
    Os {
        /// What was asked for, such as a `FOLDERID_` name.
        folder: &'static str,
        /// The upstream error.
        source: Arc<std::io::Error>,
    },
}

/// Every root that exists on this platform, resolved or not.
#[derive(Debug, Clone, Default)]
pub struct Roots(BTreeMap<SymbolicRoot, Result<ResolvedRoot, UnresolvedRoot>>);

impl Roots {
    /// Records how `root` resolved.
    pub fn insert(&mut self, root: SymbolicRoot, resolution: Result<ResolvedRoot, UnresolvedRoot>) {
        self.0.insert(root, resolution);
    }

    /// How `root` resolved, or `None` if it doesn't exist on this platform.
    #[must_use]
    pub fn get(&self, root: SymbolicRoot) -> Option<&Result<ResolvedRoot, UnresolvedRoot>> {
        self.0.get(&root)
    }

    /// Every root on this platform, in a fixed order.
    pub fn iter(
        &self,
    ) -> impl Iterator<Item = (SymbolicRoot, &Result<ResolvedRoot, UnresolvedRoot>)> {
        self.0.iter().map(|(root, resolution)| (*root, resolution))
    }
}
