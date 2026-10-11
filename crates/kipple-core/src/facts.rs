//! The facts the host establishes for each finding once it is measured: whether a git
//! repository could track files inside it, and whether a running program uses it
//! (`docs/04-safety-model.md` §2a and §4). Integrations never supply these.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::policy::{Activity, ActivityGap, Tracked};
use crate::probe::{FsProbe, ProbeError};
use crate::process::{Inventory, ProcessClass, ProcessOutcome, ProcessProbe, ProcessSnapshot};

/// What one directory holds, as far as git goes.
#[derive(Debug, Clone)]
enum GitEntry {
    Absent,
    Present,
    Unchecked(ProbeError),
}

/// The repository checks of one scan. Findings share most of their ancestors, so each
/// directory is checked once.
#[derive(Debug, Default)]
pub(crate) struct Repositories(Mutex<HashMap<PathBuf, GitEntry>>);

impl Repositories {
    /// Looks for `.git` in `item` and in every directory above it, up to the file system's
    /// root. Only when each one could be checked and none has it is nothing inside
    /// tracked.
    pub(crate) fn tracked(&self, fs: &dyn FsProbe, item: &Path) -> Tracked {
        for dir in item.ancestors() {
            match self.entry(fs, dir) {
                GitEntry::Absent => {}
                GitEntry::Present => {
                    return Tracked::InsideRepository {
                        work_tree: dir.to_owned(),
                    };
                }
                GitEntry::Unchecked(error) => return Tracked::Unchecked(error),
            }
        }
        Tracked::NoRepository
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

/// The running executables of one scan, listed once when the first finding needs them.
#[derive(Debug, Default)]
pub(crate) struct Processes(OnceLock<Result<ProcessSnapshot, ProbeError>>);

impl Processes {
    /// Whether a program runs a file inside `item`. Any process kipple couldn't identify
    /// might be the one, so the answer is then unknown, never idle.
    pub(crate) fn activity(&self, probe: &dyn ProcessProbe, item: &Path) -> Activity {
        let snapshot = match self.0.get_or_init(|| probe.snapshot()) {
            Ok(snapshot) => snapshot,
            Err(error) => return Activity::Unknown(ActivityGap::NoListing(error.clone())),
        };
        let mut uninspected = 0;
        for process in &snapshot.processes {
            if let Some(path) = executable(&process.outcome)
                && path.starts_with(item)
            {
                return Activity::InUse {
                    pid: process.pid,
                    executable: path.to_owned(),
                };
            }
            if process.outcome.class() == ProcessClass::Unknown {
                uninspected += 1;
            }
        }
        if let Inventory::Partial { reason } = &snapshot.inventory {
            return Activity::Unknown(ActivityGap::Partial(reason.clone()));
        }
        if uninspected > 0 {
            return Activity::Unknown(ActivityGap::Uninspected(uninspected));
        }
        Activity::Idle
    }
}

/// The path of the file a process runs, when the OS reported one.
const fn executable(outcome: &ProcessOutcome) -> Option<&PathBuf> {
    match outcome {
        ProcessOutcome::Identified { path, .. } | ProcessOutcome::PathOnly { path, .. } => {
            Some(path)
        }
        ProcessOutcome::Denied(_)
        | ProcessOutcome::Exited
        | ProcessOutcome::Gone
        | ProcessOutcome::NoExecutable
        | ProcessOutcome::Other(_) => None,
    }
}
