//! Symbolic roots and where each one resolved from (`docs/05-architecture.md` §1).

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::probe::FileIdentity;

named! {
    /// A location kipple knows by meaning, not by path. The platform resolves it.
    SymbolicRoot {
        /// The user's home directory.
        Home = "home",
        /// `$XDG_CONFIG_HOME`, default `~/.config`.
        XdgConfig = "xdg.config",
        /// `$XDG_CACHE_HOME`, default `~/.cache`.
        XdgCache = "xdg.cache",
        /// `$XDG_DATA_HOME`, default `~/.local/share`.
        XdgData = "xdg.data",
        /// `$XDG_STATE_HOME`, default `~/.local/state`.
        XdgState = "xdg.state",
        /// The Windows Known Folder `LocalAppData`.
        LocalAppData = "known.local-app-data",
        /// The Windows Known Folder `RoamingAppData`.
        RoamingAppData = "known.roaming-app-data",
        /// `~/Library/Caches` on macOS.
        MacosCaches = "macos.caches",
        /// The project roots: the defaults that exist, or the ones given. A scan may have
        /// several.
        ProjectRoots = "config.project-roots",
        /// The current user's trash: freedesktop's home trash on Linux, `~/.Trash` on
        /// macOS.
        Trash = "trash",
        /// Claude Code's configuration directory: `$CLAUDE_CONFIG_DIR`, default
        /// `~/.claude`.
        ClaudeConfig = "claude.config",
        /// pacman's package cache: `CacheDir` in `/etc/pacman.conf`, default
        /// `/var/cache/pacman/pkg`.
        PacmanCache = "pacman.cache",
        /// systemd-journald's persistent journal, `/var/log/journal`.
        Journal = "journald.journal",
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
    /// The absolute path of the directory kipple reads: where the configured path leads
    /// when that is a link. Unless `link` is set, nothing has checked that it exists.
    pub path: PathBuf,
    /// Where the configured path came from.
    pub source: RootSource,
    /// Set when the configured path is itself a link, resolved once (D-041).
    pub link: Option<RootLink>,
}

impl ResolvedRoot {
    /// A root at `path`, as `source` gave it, not yet checked for a link.
    #[must_use]
    pub const fn new(root: SymbolicRoot, path: PathBuf, source: RootSource) -> Self {
        Self {
            root,
            path,
            source,
            link: None,
        }
    }

    /// The path as configured: the link when the root was resolved through one.
    #[must_use]
    pub fn configured(&self) -> &PathBuf {
        self.link
            .as_ref()
            .map_or(&self.path, |link| &link.configured)
    }
}

/// A root whose configured path is a link, such as `~/.cache -> /mnt/ssd/cache`. Only the
/// root's own path is resolved; links below a root are never followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootLink {
    /// The configured path, which is the link.
    pub configured: PathBuf,
    /// The identity of the directory it led to when it was resolved. A scan reads the
    /// root only while the directory there still has this identity.
    pub identity: FileIdentity,
}

/// Why a root whose configured path is a link can't be used.
#[derive(Debug, Clone, thiserror::Error)]
pub enum LinkProblem {
    /// The link, or a link it leads to, can't be followed.
    #[error("can't be followed: {0}")]
    Unresolvable(Arc<std::io::Error>),
    /// It leads to something other than a directory.
    #[error("doesn't lead to a directory")]
    NotDirectory,
    /// It leads to a directory whose identity can't be read, so it can't be re-checked.
    #[error("leads to a directory whose identity can't be read")]
    NoIdentity,
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
    /// The configured path is a link that can't be used as the root.
    #[error("{} is a link that {problem}", path.display())]
    Link {
        /// The configured path.
        path: PathBuf,
        /// What is wrong with it.
        problem: LinkProblem,
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

/// Every root that exists on this platform, resolved or not. Most symbolic roots have one
/// path; [`SymbolicRoot::ProjectRoots`] may have several.
#[derive(Debug, Clone, Default)]
pub struct Roots(BTreeMap<SymbolicRoot, Vec<Result<ResolvedRoot, UnresolvedRoot>>>);

impl Roots {
    /// Records how `root` resolved, replacing what was recorded before.
    pub fn insert(&mut self, root: SymbolicRoot, resolution: Result<ResolvedRoot, UnresolvedRoot>) {
        self.0.insert(root, vec![resolution]);
    }

    /// Adds another path for `root`, such as one more project root.
    pub fn add(&mut self, root: SymbolicRoot, resolution: Result<ResolvedRoot, UnresolvedRoot>) {
        self.0.entry(root).or_default().push(resolution);
    }

    /// How `root` resolved, or `None` if it doesn't exist on this platform. For a root
    /// with several paths, the first.
    #[must_use]
    pub fn get(&self, root: SymbolicRoot) -> Option<&Result<ResolvedRoot, UnresolvedRoot>> {
        self.0
            .get(&root)
            .and_then(|resolutions| resolutions.first())
    }

    /// Every path recorded for `root`, in the order they were added.
    #[must_use]
    pub fn all(&self, root: SymbolicRoot) -> &[Result<ResolvedRoot, UnresolvedRoot>] {
        self.0.get(&root).map_or(&[], Vec::as_slice)
    }

    /// Every root on this platform, in a fixed order.
    pub fn iter(
        &self,
    ) -> impl Iterator<Item = (SymbolicRoot, &Result<ResolvedRoot, UnresolvedRoot>)> {
        self.0
            .iter()
            .flat_map(|(root, resolutions)| resolutions.iter().map(move |r| (*root, r)))
    }
}
