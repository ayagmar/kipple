//! The walker (D-036): std `read_dir`, metadata from each entry without following links,
//! and one task per directory on a bounded rayon pool.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;

use kipple_core::{
    Allocated, Apparent, CancelToken, Completeness, FileIdentity, ProbeError, Sizer, SpaceEstimate,
};

use crate::fs_probe::exact;
use crate::os;

/// Sizes trees on its own bounded thread pool. It reads no ignore file or git config,
/// skips nothing hidden, never follows a link or junction, and stays on the file system
/// of the item it sizes.
#[derive(Debug)]
pub struct Walker {
    pool: rayon::ThreadPool,
}

impl Walker {
    /// A walker with one thread per available CPU.
    ///
    /// # Errors
    /// When the thread pool can't be started.
    pub fn new() -> std::io::Result<Self> {
        let threads = thread::available_parallelism()?.get();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|n| format!("kipple-walk-{n}"))
            .build()
            .map_err(std::io::Error::other)?;
        Ok(Self { pool })
    }
}

impl Sizer for Walker {
    fn size(&self, path: &Path, cancel: &CancelToken) -> Result<SpaceEstimate, ProbeError> {
        let item = exact(path);
        let meta = fs::symlink_metadata(&item).map_err(|e| ProbeError::io(path, e))?;
        let tally = Tally::default();
        let mut top = Counts::default();
        top.add(&meta);
        tally.merge(top);
        if meta.is_dir() {
            let device = os::device(&meta);
            self.pool
                .scope(|scope| visit(scope, &item, device, &tally, cancel));
        }
        Ok(tally.estimate())
    }
}

/// Sizes the entries of `dir` and schedules each subdirectory on the same device as its
/// own task. Once cancelled, no entry is read and no directory is scheduled anymore.
fn visit<'s>(
    scope: &rayon::Scope<'s>,
    dir: &Path,
    device: Option<u64>,
    tally: &'s Tally,
    cancel: &'s CancelToken,
) {
    if cancel.is_cancelled() {
        tally.cancelled.store(true, Ordering::Relaxed);
        return;
    }
    let mut counts = Counts::default();
    let Ok(entries) = fs::read_dir(dir) else {
        counts.unreadable += 1;
        tally.merge(counts);
        return;
    };
    for entry in entries {
        if cancel.is_cancelled() {
            tally.cancelled.store(true, Ordering::Relaxed);
            break;
        }
        let Ok((entry, meta)) = entry.and_then(|e| e.metadata().map(|meta| (e, meta))) else {
            counts.unreadable += 1;
            continue;
        };
        counts.add(&meta);
        if meta.is_dir() && os::device(&meta) == device {
            let sub = entry.path();
            scope.spawn(move |scope| visit(scope, &sub, device, tally, cancel));
        }
    }
    tally.merge(counts);
}

/// A hard-linked file, counted once however many of its names the walk meets.
#[derive(Debug, Clone, Copy)]
struct Linked {
    links: u64,
    names_seen: u64,
    apparent: u64,
    allocated: u64,
}

/// What one directory contributed. Merged into the shared [`Tally`] once.
#[derive(Debug, Default)]
struct Counts {
    apparent: u64,
    allocated: u64,
    allocation_unknown: bool,
    unreadable: u64,
    linked: Vec<(FileIdentity, Linked)>,
}

impl Counts {
    fn add(&mut self, meta: &fs::Metadata) {
        let apparent = if meta.is_dir() { 0 } else { meta.len() };
        let allocated = os::allocated(meta);
        self.allocation_unknown |= allocated.is_none();
        let allocated = allocated.unwrap_or(0);
        let Some((identity, links)) = os::hard_link(meta) else {
            self.apparent += apparent;
            self.allocated += allocated;
            return;
        };
        let linked = Linked {
            links,
            names_seen: 1,
            apparent,
            allocated,
        };
        self.linked.push((identity, linked));
    }
}

/// Totals shared by every task of one walk.
#[derive(Debug, Default)]
struct Tally {
    apparent: AtomicU64,
    allocated: AtomicU64,
    allocation_unknown: AtomicBool,
    unreadable: AtomicU64,
    cancelled: AtomicBool,
    linked: Mutex<HashMap<FileIdentity, Linked>>,
}

impl Tally {
    fn merge(&self, counts: Counts) {
        let (apparent, allocated) = self.first_names(counts.linked);
        self.apparent
            .fetch_add(counts.apparent + apparent, Ordering::Relaxed);
        self.allocated
            .fetch_add(counts.allocated + allocated, Ordering::Relaxed);
        self.unreadable
            .fetch_add(counts.unreadable, Ordering::Relaxed);
        if counts.allocation_unknown {
            self.allocation_unknown.store(true, Ordering::Relaxed);
        }
    }

    /// Records hard-linked names and returns the apparent and allocated bytes of the files
    /// not met before, so each file counts once.
    fn first_names(&self, names: Vec<(FileIdentity, Linked)>) -> (u64, u64) {
        if names.is_empty() {
            return (0, 0);
        }
        let mut linked = self.linked.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut apparent, mut allocated) = (0, 0);
        for (identity, name) in names {
            match linked.entry(identity) {
                Entry::Occupied(mut known) => known.get_mut().names_seen += 1,
                Entry::Vacant(slot) => {
                    apparent += name.apparent;
                    allocated += name.allocated;
                    slot.insert(name);
                }
            }
        }
        (apparent, allocated)
    }

    fn estimate(self) -> SpaceEstimate {
        let allocated = self.allocated.into_inner();
        let shared: u64 = self
            .linked
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .into_values()
            .filter(|file| file.names_seen < file.links)
            .map(|file| file.allocated)
            .sum();
        let known = !self.allocation_unknown.into_inner();
        let unreadable = self.unreadable.into_inner();
        let completeness = if self.cancelled.into_inner() {
            Completeness::Cancelled
        } else if unreadable > 0 {
            Completeness::Incomplete { unreadable }
        } else {
            Completeness::Complete
        };
        SpaceEstimate {
            apparent: Apparent(self.apparent.into_inner()),
            allocated: known.then_some(Allocated(allocated)),
            unique_reclaim: known.then_some(Allocated(allocated.saturating_sub(shared))),
            completeness,
        }
    }
}
