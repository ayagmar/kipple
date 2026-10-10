//! Read-only filesystem probes that never follow a link.

use std::fs;
use std::io;
use std::path::Path;

use kipple_core::{DirEntryMeta, EntryKind, EntryMeta, FsProbe, ProbeError};

use crate::os;

/// [`FsProbe`] on the real filesystem. Symbolic links, and on Windows junctions and
/// other name-surrogate reparse points, are reported as links and never followed.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoFollowFs;

impl FsProbe for NoFollowFs {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        let meta = fs::symlink_metadata(path).map_err(|e| ProbeError::io(path, e))?;
        Ok(EntryMeta {
            kind: kind_of(meta.file_type()),
            identity: os::identity(path, &meta),
        })
    }

    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        let read = || -> io::Result<Vec<DirEntryMeta>> {
            // `fs::read_dir` would list a link's target.
            if !fs::symlink_metadata(dir)?.is_dir() {
                return Err(io::ErrorKind::NotADirectory.into());
            }
            fs::read_dir(dir)?
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
