//! What Linux and macOS share: XDG roots, `stat` fields and process error codes.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

use kipple_core::{FileIdentity, ResolvedRoot, RootSource, Roots, SymbolicRoot, UnresolvedRoot};

use crate::environment::Environment;
use crate::roots::{absolute, under};

/// kipple's own directories: under the XDG roots on Linux and macOS alike (D-015).
pub(crate) const KIPPLE_DIRS: [(SymbolicRoot, &str); 3] = [
    (SymbolicRoot::XdgConfig, "kipple"),
    (SymbolicRoot::XdgData, "kipple"),
    (SymbolicRoot::XdgCache, "kipple"),
];

/// The XDG base directories, each with the `$HOME`-relative default from the spec.
const XDG: [(SymbolicRoot, &str, &str); 4] = [
    (SymbolicRoot::XdgConfig, "XDG_CONFIG_HOME", ".config"),
    (SymbolicRoot::XdgCache, "XDG_CACHE_HOME", ".cache"),
    (SymbolicRoot::XdgData, "XDG_DATA_HOME", ".local/share"),
    (SymbolicRoot::XdgState, "XDG_STATE_HOME", ".local/state"),
];

pub(crate) fn raw_bytes(text: &OsStr) -> Vec<u8> {
    text.as_bytes().to_vec()
}

pub(crate) fn env_name_matches(key: &OsStr, name: &str) -> bool {
    key == name
}

/// The home root, from `HOME`.
fn home(env: &Environment) -> Result<ResolvedRoot, UnresolvedRoot> {
    let path = absolute(env, "HOME")?.ok_or(UnresolvedRoot::NotSet { var: "HOME" })?;
    Ok(ResolvedRoot::new(
        SymbolicRoot::Home,
        path,
        RootSource::Env("HOME"),
    ))
}

/// The home root, then the XDG roots: each variable when set, its default under home
/// otherwise. A relative value leaves the root unknown instead of falling back.
pub(crate) fn xdg_roots(env: &Environment) -> Roots {
    let mut roots = Roots::default();
    roots.insert(SymbolicRoot::Home, home(env));
    for (root, var, relative) in XDG {
        let resolved = match absolute(env, var) {
            Ok(Some(path)) => Ok(ResolvedRoot::new(root, path, RootSource::Env(var))),
            Ok(None) => under(&roots, SymbolicRoot::Home, root, relative),
            Err(unusable) => Err(unusable),
        };
        roots.insert(root, resolved);
    }
    roots
}

#[expect(clippy::unnecessary_wraps, reason = "Windows can't report it")]
pub(crate) fn identity(_: &Path, meta: &fs::Metadata) -> Option<FileIdentity> {
    Some(identity_of(meta))
}

pub(crate) fn identity_of(meta: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        volume: meta.dev(),
        file: meta.ino(),
    }
}

#[expect(clippy::unnecessary_wraps, reason = "Windows can't report it")]
pub(crate) fn device(meta: &fs::Metadata) -> Option<u64> {
    Some(meta.dev())
}

/// `st_blocks` counts 512-byte units on Linux and macOS, whatever the block size.
#[expect(clippy::unnecessary_wraps, reason = "Windows can't report it")]
pub(crate) fn allocated(meta: &fs::Metadata) -> Option<u64> {
    Some(meta.blocks().saturating_mul(512))
}

/// A file (not a directory) with more than one name, and how many it has.
pub(crate) fn hard_link(meta: &fs::Metadata) -> Option<(FileIdentity, u64)> {
    (!meta.is_dir() && meta.nlink() > 1).then(|| (identity_of(meta), meta.nlink()))
}

pub(crate) fn is_executable(meta: &fs::Metadata) -> bool {
    meta.permissions().mode() & 0o111 != 0
}

/// Nothing beyond `LC_ALL=C` is needed to start a process.
pub(crate) const fn process_env(_: &Environment) -> Vec<(&'static str, OsString)> {
    Vec::new()
}
