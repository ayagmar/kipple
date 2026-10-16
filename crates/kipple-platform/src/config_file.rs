//! kipple's own config file (`docs/03-functional-spec.md` §6 and §7): read it, and
//! replace it in one atomic write. It is the only file kipple writes so far.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::os;

/// The largest config file kipple reads.
const LIMIT: u64 = 1024 * 1024;

/// Reads the config file at `path`, following a link to it.
///
/// `Ok(None)` only when nothing at all is there, neither a file nor a link.
///
/// # Errors
/// When it can't be read, is a link to nothing, isn't a regular file, is larger than
/// 1 MiB or isn't UTF-8.
pub fn read_config(path: &Path) -> io::Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let meta = fs::metadata(path).map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            io::Error::new(ErrorKind::NotFound, "it is a link to nothing")
        } else {
            error
        }
    })?;
    if meta.is_dir() {
        return Err(io::Error::new(ErrorKind::InvalidInput, "it is a directory"));
    }
    if !meta.is_file() {
        let message = "it is not a regular file";
        return Err(io::Error::new(ErrorKind::InvalidInput, message));
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > LIMIT {
        let message = "it is larger than 1 MiB";
        return Err(io::Error::new(ErrorKind::InvalidData, message));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| io::Error::new(ErrorKind::InvalidData, "it is not UTF-8"))
}

/// Why the config file wasn't written.
#[derive(Debug)]
pub enum WriteError<E> {
    /// The edit refused the content on disk, or the change.
    Edit(E),
    /// The file couldn't be read, written or replaced. The old one is left as it was.
    Io {
        /// The file.
        path: PathBuf,
        /// The upstream error.
        source: io::Error,
    },
}

impl<E: fmt::Display> fmt::Display for WriteError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Edit(error) => error.fmt(f),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

/// Replaces the config file at `path` with what `edit` makes of the content on disk now
/// (`None` when there is no file yet), in one atomic write, and returns what was written.
///
/// It reads the file again here, so changes an editor made since it was loaded survive.
/// A link is written through: it is resolved once and its target replaced. A temporary
/// file beside the target is created new, written and synced, renamed over the target,
/// and the directory synced. The file keeps its mode; a new one is readable by its owner
/// only. The first write creates the config directory.
///
/// # Errors
/// [`WriteError::Edit`] when `edit` refuses, and [`WriteError::Io`] when the file can't
/// be read or replaced, including a link that leads nowhere.
pub fn write_config<E>(
    path: &Path,
    edit: impl FnOnce(Option<&str>) -> Result<String, E>,
) -> Result<String, WriteError<E>> {
    let io_error = |file: &Path| {
        let owned = file.to_owned();
        move |source| WriteError::Io {
            path: owned,
            source,
        }
    };
    let target = through_link(path).map_err(io_error(path))?;
    let current = read_config(&target).map_err(io_error(&target))?;
    let text = edit(current.as_deref()).map_err(WriteError::Edit)?;
    replace(&target, &text, &|from, to| {
        #[expect(
            clippy::disallowed_methods,
            reason = "the atomic replace of kipple's own config file, its only write"
        )]
        fs::rename(from, to)
    })
    .map_err(io_error(&target))?;
    Ok(text)
}

/// Where a write to `path` goes: `path` itself, or the file a link there leads to.
fn through_link(path: &Path) -> io::Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => os::physical(path),
        Ok(_) => Ok(path.to_owned()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(path.to_owned()),
        Err(error) => Err(error),
    }
}

/// Writes `text` to a new temporary file beside `target` and renames it over `target`
/// with `rename`. When anything fails, `target` is untouched and the temporary file gone.
fn replace(
    target: &Path,
    text: &str,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let dir = target
        .parent()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "it has no directory"))?;
    os::create_private_dir(dir)?;
    let mode = match fs::metadata(target) {
        Ok(meta) => Some(meta.permissions()),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let temp = temporary(target);
    let mut file = os::create_private_file(&temp)?;
    let written = (|| {
        file.write_all(text.as_bytes())?;
        if let Some(mode) = mode {
            file.set_permissions(mode)?;
        }
        file.sync_all()?;
        drop(file);
        rename(&temp, target)
    })();
    if let Err(error) = written {
        #[expect(
            clippy::disallowed_methods,
            reason = "removes the temporary file this write created, never anything else"
        )]
        fs::remove_file(&temp).ok();
        return Err(error);
    }
    os::sync_dir(dir)
}

/// A name beside `target` no other write of this process uses.
fn temporary(target: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = OsString::from(".");
    name.push(target.file_name().unwrap_or_default());
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    name.push(format!(".{}.{unique}.tmp", std::process::id()));
    target.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_before_the_rename_leaves_the_old_file_and_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(&file, "[care]\nlevel = \"careful\"\n").unwrap();

        let failed = replace(&file, "[care]\nlevel = \"thorough\"\n", &|_, _| {
            Err(io::Error::other("injected before the rename"))
        });

        assert!(failed.is_err());
        let kept = fs::read_to_string(&file).unwrap();
        assert_eq!(kept, "[care]\nlevel = \"careful\"\n");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["config.toml"]);
    }

    #[test]
    fn only_nothing_at_all_reads_as_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");

        assert!(read_config(&file).unwrap().is_none());
        assert!(
            read_config(dir.path()).is_err(),
            "a directory is not a config"
        );
        fs::write(&file, b"level = \"menu\xe9\"").unwrap();
        assert!(read_config(&file).is_err(), "not UTF-8");
    }

    #[test]
    fn the_first_write_creates_the_directory_and_a_file_only_its_owner_reads() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("kipple").join("config.toml");

        let written = write_config(&file, |current| {
            assert_eq!(current, None);
            Ok::<_, String>("[care]\n".to_owned())
        });

        assert_eq!(written.unwrap(), "[care]\n");
        assert_eq!(fs::read_to_string(&file).unwrap(), "[care]\n");
        assert_private(&file);
    }

    #[test]
    fn a_refused_edit_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        fs::write(&file, "old").unwrap();

        let written = write_config(&file, |_| Err("mid-edit"));

        assert!(matches!(written, Err(WriteError::Edit("mid-edit"))));
        assert_eq!(fs::read_to_string(&file).unwrap(), "old");
    }

    #[cfg(unix)]
    fn assert_private(file: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(windows)]
    fn assert_private(_: &Path) {}

    #[cfg(unix)]
    #[test]
    fn a_write_keeps_the_files_mode_and_replaces_what_a_link_leads_to() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dotfiles").join("kipple.toml");
        fs::create_dir(target.parent().unwrap()).unwrap();
        fs::write(&target, "old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.path().join("config.toml");
        symlink(&target, &link).unwrap();

        write_config(&link, |current| {
            assert_eq!(current, Some("old"));
            Ok::<_, String>("new".to_owned())
        })
        .unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        let mode = fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_nothing_is_an_error_not_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(dir.path().join("gone.toml"), &link).unwrap();

        let read = read_config(&link);

        assert!(read.is_err());
        assert!(write_config(&link, |_| Ok::<_, String>(String::new())).is_err());
        assert!(fs::symlink_metadata(dir.path().join("gone.toml")).is_err());
    }
}
