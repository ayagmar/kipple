//! kipple's own folders under its data directory, which the run store and the profile
//! store write (D-065, D-070): each opened as a directory handle, never through a link, and
//! every name in it opened, renamed and removed relative to that handle without
//! following a link.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::fs::File;
use std::io;
use std::path::Path;

use kipple_core::{ApplyError, ErrorCode, FileSystemClass};

use crate::os;

#[cfg(unix)]
pub(crate) use unix::{Folder, file_system_at, is_link};
#[cfg(windows)]
pub(crate) use windows::{Folder, file_system_at, is_link};

/// The file system a folder in `data` is made on: that of `data`, or of the nearest
/// directory above it when it is missing, reached through links as making it reaches it.
pub(crate) fn made_on(data: &Path) -> io::Result<FileSystemClass> {
    let existing = data
        .ancestors()
        .find(|dir| dir.exists())
        .ok_or(io::ErrorKind::NotFound)?;
    file_system_at(existing)
}

/// Creates `folder` and any missing parent, each readable by its owner only and synced
/// into the directory that gained it.
pub(crate) fn create_synced(folder: &Path) -> io::Result<()> {
    let missing: Vec<&Path> = folder.ancestors().take_while(|dir| !dir.exists()).collect();
    os::create_private_dir(folder)?;
    for dir in missing {
        if let Some(parent) = dir.parent() {
            os::sync_dir(parent)?;
        }
    }
    Ok(())
}

/// `file`, if it is a regular file: never a directory, device or pipe.
pub(crate) fn regular(file: File) -> io::Result<File> {
    if file.metadata()?.is_file() {
        Ok(file)
    } else {
        Err(not_regular())
    }
}

fn not_regular() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")
}

/// A read or write that failed: through a link where kipple never follows one, or
/// another I/O error.
pub(crate) fn failed(error: io::Error) -> ApplyError {
    let code = if is_link(&error) {
        ErrorCode::ThroughLink
    } else {
        ErrorCode::Io
    };
    ApplyError::io(code, error)
}
