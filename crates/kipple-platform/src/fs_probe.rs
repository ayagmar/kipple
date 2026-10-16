//! Read-only filesystem probes that never follow a link.

use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use kipple_core::{DirAccess, DirEntryMeta, EntryKind, EntryMeta, FsProbe, ProbeError};

use crate::os;

/// [`FsProbe`] on the real filesystem. Symbolic links, and on Windows junctions and
/// other name-surrogate reparse points, are reported as links and never followed.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoFollowFs;

impl FsProbe for NoFollowFs {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        let exact = exact(path);
        let meta = fs::symlink_metadata(&exact).map_err(|e| ProbeError::io(path, e))?;
        Ok(EntryMeta {
            kind: kind_of(meta.file_type()),
            identity: os::identity(&exact, &meta),
        })
    }

    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        let read = || -> io::Result<Vec<DirEntryMeta>> {
            open(dir)?
                .map(|entry| {
                    let entry = entry?;
                    Ok(DirEntryMeta {
                        kind: kind_of(entry.file_type()?),
                        name: entry.file_name(),
                    })
                })
                .collect()
        };
        read().map_err(|e| ProbeError::io(dir, e))
    }

    fn open_dir(&self, dir: &Path) -> Result<(), ProbeError> {
        open(dir).map(drop).map_err(|e| ProbeError::io(dir, e))
    }

    fn read_file(&self, path: &Path, limit: u64) -> Result<Vec<u8>, ProbeError> {
        let read = || -> io::Result<Vec<u8>> {
            let file = exact(path);
            if !fs::symlink_metadata(&file)?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "not a regular file",
                ));
            }
            let mut bytes = Vec::new();
            fs::File::open(file)?.take(limit).read_to_end(&mut bytes)?;
            Ok(bytes)
        };
        read().map_err(|e| ProbeError::io(path, e))
    }

    fn access(&self, dir: &Path) -> Result<DirAccess, ProbeError> {
        os::dir_access(&exact(dir)).map_err(|e| ProbeError::io(dir, e))
    }
}

/// `path` without a trailing separator or `.` component, either of which would make the
/// OS follow a link in the last component.
pub(crate) fn exact(path: &Path) -> PathBuf {
    path.components().collect()
}

/// The directory at `dir`, opened for listing. A link is refused: `fs::read_dir` would
/// list its target.
fn open(dir: &Path) -> io::Result<fs::ReadDir> {
    let dir = exact(dir);
    if !fs::symlink_metadata(&dir)?.is_dir() {
        return Err(io::ErrorKind::NotADirectory.into());
    }
    fs::read_dir(dir)
}

/// What a no-follow file type is. On Windows std reports a junction as a link, not as a
/// directory, so it is never descended either.
fn kind_of(file_type: fs::FileType) -> EntryKind {
    if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    }
}
