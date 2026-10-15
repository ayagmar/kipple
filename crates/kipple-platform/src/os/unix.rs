//! What Linux and macOS share: XDG roots, `stat` fields and process error codes.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kipple_core::{
    EnvPath, FileIdentity, ProcessOwner, ResolvedRoot, RootSource, Roots, SymbolicRoot, Unread,
    UnresolvedRoot, UserId,
};

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

/// The directory an absolute `path` names, with its links and `..` resolved: a `..` after
/// a link climbs from where the link leads.
pub(crate) fn physical(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path)
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

/// A read that failed, kept as unknown.
pub(crate) fn unread(error: io::Error) -> Unread {
    Unread::Failed(Arc::new(error))
}

/// The owner of a process with these user IDs (real, effective, saved and, on Linux,
/// filesystem), seen by a user whose ID is `current`. Only IDs that all agree name an
/// owner, and without a known `current` no ID can be told apart from it.
pub(crate) fn owner(uids: Option<&[u32]>, current: Option<u32>) -> ProcessOwner {
    let (Some([first, rest @ ..]), Some(current)) = (uids, current) else {
        return ProcessOwner::Unknown;
    };
    if rest.iter().any(|uid| uid != first) {
        return ProcessOwner::Unknown;
    }
    match *first {
        uid if uid == current => ProcessOwner::CurrentUser,
        0 => ProcessOwner::Root,
        uid => ProcessOwner::OtherUser(UserId::Uid(uid)),
    }
}

/// The NUL-terminated strings in `bytes`, with or without a final NUL.
pub(crate) fn nul_separated(bytes: &[u8]) -> Vec<&[u8]> {
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    if bytes.is_empty() {
        return Vec::new();
    }
    bytes.split(|&b| b == 0).collect()
}

/// The arguments that can name a file: the program name and every argument with a `/`.
/// The rest, and URLs, which can carry credentials, are dropped: a command line often
/// holds secrets (docs/04 §8a).
pub(crate) fn path_args(args: &[&[u8]]) -> Vec<OsString> {
    args.iter()
        .enumerate()
        .filter(|&(i, arg)| i == 0 || (arg.contains(&b'/') && !contains(arg, b"://")))
        .map(|(_, arg)| OsStr::from_bytes(arg).to_owned())
        .collect()
}

/// Every absolute path that the `NAME=value` entries of an environment hold, a value
/// being a `:`-separated list. Everything else is dropped, since environments hold
/// secrets (docs/04 §8a): relative entries, and pieces starting with `//`, which is how
/// the credentials part of a URL such as `scheme://user:password@host` splits.
pub(crate) fn env_paths(entries: &[&[u8]]) -> Vec<EnvPath> {
    let mut paths = Vec::new();
    for entry in entries {
        let mut parts = entry.splitn(2, |&b| b == b'=');
        let (Some(var), Some(value)) = (parts.next(), parts.next()) else {
            continue;
        };
        for piece in value.split(|&b| b == b':') {
            if piece.starts_with(b"/") && !piece.starts_with(b"//") {
                paths.push(EnvPath {
                    var: OsStr::from_bytes(var).to_owned(),
                    path: PathBuf::from(OsStr::from_bytes(piece)),
                });
            }
        }
    }
    paths
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[OsString]) -> Vec<&str> {
        values.iter().map(|v| v.to_str().unwrap()).collect()
    }

    #[test]
    fn a_command_line_keeps_its_program_and_path_arguments_and_drops_secrets() {
        let cmdline = b"python3\0./manage.py\0runserver\0--token=hunter2\0\
                        --settings=conf/dev.py\0https://u:s3cret@example.test/x\0\0../up\0";

        let args = path_args(&nul_separated(cmdline));

        assert_eq!(
            strings(&args),
            ["python3", "./manage.py", "--settings=conf/dev.py", "../up"]
        );
    }

    #[test]
    fn a_command_line_with_no_final_nul_or_none_at_all_is_still_read() {
        assert_eq!(nul_separated(b"node\0bin/www"), [&b"node"[..], b"bin/www"]);
        assert_eq!(nul_separated(b""), Vec::<&[u8]>::new());
        assert_eq!(nul_separated(b"\0"), Vec::<&[u8]>::new());
        assert_eq!(nul_separated(b"a\0\0b\0"), [&b"a"[..], b"", b"b"]);
    }

    #[test]
    fn an_environment_keeps_only_absolute_paths_and_drops_secrets() {
        let environ = b"PATH=/home/u/p/.venv/bin:./node_modules/.bin::/usr/bin\0\
                        VIRTUAL_ENV=/home/u/p/.venv\0API_KEY=sk-not-a-real-key\0\
                        DATABASE_URL=postgres://u:s3cret@db/app\0TERM=xterm\0NOEQUALS\0\
                        PYTHONPATH=/srv/lib:/srv/vendor\0";

        let paths = env_paths(&nul_separated(environ));

        let found: Vec<(&str, &str)> = paths
            .iter()
            .map(|p| (p.var.to_str().unwrap(), p.path.to_str().unwrap()))
            .collect();
        assert_eq!(
            found,
            [
                ("PATH", "/home/u/p/.venv/bin"),
                ("PATH", "/usr/bin"),
                ("VIRTUAL_ENV", "/home/u/p/.venv"),
                ("PYTHONPATH", "/srv/lib"),
                ("PYTHONPATH", "/srv/vendor"),
            ]
        );
    }
}
