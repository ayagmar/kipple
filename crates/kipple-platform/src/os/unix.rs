//! What Linux and macOS share: XDG roots, `stat` fields and process error codes.

use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use kipple_core::{
    DirAccess, EnvPath, FileIdentity, FileSystem, ProcessOwner, ResolvedRoot, RootSource, Roots,
    SymbolicRoot, Unread, UnresolvedRoot, UserId,
};

use crate::environment::Environment;
use crate::os::HardLink;
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
        file: u128::from(meta.ino()),
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
pub(crate) fn hard_link(meta: &fs::Metadata) -> Option<HardLink> {
    (!meta.is_dir() && meta.nlink() > 1).then(|| HardLink {
        identity: identity_of(meta),
        names: meta.nlink(),
    })
}

pub(crate) fn is_executable(meta: &fs::Metadata) -> bool {
    meta.permissions().mode() & 0o111 != 0
}

/// The variables a process is started with besides `LC_ALL=C`: none is needed.
pub(crate) const INHERITED_VARS: &[&str] = &[];

/// A read that failed, kept as unknown.
pub(crate) fn unread(error: io::Error) -> Unread {
    Unread::Failed(Arc::new(error))
}

/// The owner of a process with these user IDs (real, effective, saved and, on Linux,
/// filesystem), seen by a user whose ID is `current`. Only IDs that all agree name an
/// owner, apart from an effective root with the rest root's or `current`'s, which is
/// elevated; without a known `current` no ID can be told apart from it.
pub(crate) fn owner(uids: Option<&[u32]>, current: Option<u32>) -> ProcessOwner {
    let (Some(uids @ [first, rest @ ..]), Some(current)) = (uids, current) else {
        return ProcessOwner::Unknown;
    };
    if rest.iter().any(|uid| uid != first) {
        let elevated =
            uids.get(1) == Some(&0) && uids.iter().all(|&uid| uid == 0 || uid == current);
        return if elevated {
            ProcessOwner::Elevated
        } else {
            ProcessOwner::Unknown
        };
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
    let mut args = args.iter();
    let program = args.next().map(|arg| program(arg));
    let rest = args
        .filter(|arg| names_path(arg))
        .map(|arg| OsStr::from_bytes(arg).to_owned());
    program.into_iter().chain(rest).collect()
}

/// The program name in `argv0`, whole, so a path with spaces stays one path. A title
/// rewritten into it can hold the whole command line, so it is cut before its first word
/// holding `://`, as URLs, which can carry credentials, are dropped from every argument.
/// Other words of a title stay in memory; a match on a token holding whitespace is
/// never shown (docs/04 §8a).
fn program(argv0: &[u8]) -> OsString {
    let Some(at) = argv0.windows(3).position(|window| window == b"://") else {
        return OsStr::from_bytes(argv0).to_owned();
    };
    let before = argv0.get(..at).unwrap_or_default();
    // The URL's word starts after the last whitespace before it: any whitespace in text,
    // ASCII whitespace in anything else.
    let word = std::str::from_utf8(before).map_or_else(
        |_| before.iter().rposition(u8::is_ascii_whitespace),
        |text| text.rfind(char::is_whitespace),
    );
    let kept = word.map_or(before, |start| before.get(..start).unwrap_or_default());
    let kept = std::str::from_utf8(kept).map_or(kept, |text| text.trim_end().as_bytes());
    OsStr::from_bytes(kept).to_owned()
}

/// Whether an argument can name a file: it has a `/` and isn't a URL.
fn names_path(arg: &[u8]) -> bool {
    arg.contains(&b'/') && !contains(arg, b"://")
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

/// Who may enter the directory at `dir`, from its owner and mode bits, whether it has an
/// access control list that grants something, and its file system. A link is not followed.
pub(crate) fn dir_access(dir: &Path) -> io::Result<DirAccess> {
    let meta = fs::symlink_metadata(dir)?;
    let path = CString::new(dir.as_os_str().as_bytes())?;
    Ok(DirAccess {
        owned_by_current_user: meta.uid() == effective_uid(),
        open_to_others: meta.mode() & 0o011 != 0,
        acl: super::grants_acl(&path)?,
        file_system: if super::on_network(&path)? {
            FileSystem::Remote
        } else {
            FileSystem::Local
        },
    })
}

#[expect(unsafe_code, reason = "libc's geteuid is a raw call")]
fn effective_uid() -> u32 {
    // SAFETY: it always succeeds.
    unsafe { libc::geteuid() }
}

/// Creates `dir` and any missing parent, each readable by its owner only.
pub(crate) fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// Creates a new file at `path`, failing if anything is there, readable by its owner
/// only.
pub(crate) fn create_private_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Starts `command` in a process group of its own. The terminal sends Ctrl+C to its
/// foreground group, so while kipple's interrupt handler is in place it cancels kipple
/// and never kills the command midway.
pub(crate) fn own_process_group(command: &mut Command) {
    command.process_group(0);
}

/// Makes a rename inside `dir` durable.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
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
    fn a_program_path_with_spaces_stays_whole_and_a_title_loses_its_urls() {
        // A venv's interpreter in a directory with spaces must match as one path.
        let spaced = b"/home/u/My Cool App/.venv/bin/python\0-m\0http.server\0";
        let alone = b"/home/u/My Cool App/.venv/bin/python\0";
        let title = b"/home/u/p/.venv/bin/python manage.py --settings=conf/dev.py \
                      --broker=redis://u:s3cret@h --token=hunter2\0--x\0";
        let nbsp = b"celery worker\xc2\xa0--broker=amqp://u:s3cret@h\0";
        let url = b"redis://u:s3cret@h\0";

        let read = |cmdline: &[u8]| -> Vec<String> {
            let args = path_args(&nul_separated(cmdline));
            strings(&args).into_iter().map(str::to_owned).collect()
        };
        assert_eq!(read(spaced), ["/home/u/My Cool App/.venv/bin/python"]);
        assert_eq!(read(alone), ["/home/u/My Cool App/.venv/bin/python"]);
        assert_eq!(
            read(title),
            ["/home/u/p/.venv/bin/python manage.py --settings=conf/dev.py"]
        );
        assert_eq!(read(nbsp), ["celery worker"]);
        assert_eq!(read(url), ["redis"]);
        assert_eq!(read(b"\0\0"), [""]);
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
