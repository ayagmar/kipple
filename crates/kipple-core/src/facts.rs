//! The facts the host establishes for each finding once it is measured: whether a git
//! repository could track files inside it (`docs/04-safety-model.md` §2a), and in
//! [`activity`](crate::activity) whether a running program uses it (§4). Integrations
//! never supply these.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::probe::{FsProbe, ProbeError};

/// What one directory holds, as far as git goes.
#[derive(Debug, Clone)]
enum GitEntry {
    Absent,
    Present,
    Unchecked(ProbeError),
}

/// What is above an item, as far as git goes.
#[derive(Debug, Clone)]
pub(crate) enum Above {
    /// No `.git` in it or any directory above it, each checked.
    Nothing,
    /// A `.git` in this directory, the nearest.
    Repository(PathBuf),
    /// A directory couldn't be checked.
    Unchecked(ProbeError),
}

/// The repository checks of one scan. Findings share most of their ancestors, so each
/// directory is checked once.
#[derive(Debug, Default)]
pub(crate) struct Repositories(Mutex<HashMap<PathBuf, GitEntry>>);

impl Repositories {
    /// Looks for `.git` in `item` and in every directory above it, up to the file system's
    /// root, crossing mounts. Only when each one could be checked and none has it is
    /// nothing inside tracked.
    pub(crate) fn above(&self, fs: &dyn FsProbe, item: &Path) -> Above {
        for dir in item.ancestors() {
            match self.entry(fs, dir) {
                GitEntry::Absent => {}
                GitEntry::Present => return Above::Repository(dir.to_owned()),
                GitEntry::Unchecked(error) => return Above::Unchecked(error),
            }
        }
        Above::Nothing
    }

    fn entry(&self, fs: &dyn FsProbe, dir: &Path) -> GitEntry {
        let known = |cache: &Self| {
            let cache = cache.0.lock().unwrap_or_else(PoisonError::into_inner);
            cache.get(dir).cloned()
        };
        if let Some(entry) = known(self) {
            return entry;
        }
        let entry = match fs.metadata(&dir.join(".git")) {
            // A `.git` file, link or directory all mean a repository.
            Ok(_) => GitEntry::Present,
            Err(error) if error.io_kind() == Some(io::ErrorKind::NotFound) => GitEntry::Absent,
            Err(error) => GitEntry::Unchecked(error),
        };
        let mut cache = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        cache.insert(dir.to_owned(), entry.clone());
        entry
    }
}
