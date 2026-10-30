//! The walker (D-036): std `read_dir`, metadata from each entry without following links,
//! and one task per directory on a bounded rayon pool.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::SystemTime;

use kipple_core::{
    Allocated, Apparent, CancelToken, Completeness, FileIdentity, Measurement, ProbeError, Sizer,
    SpaceEstimate,
};

use crate::fs_probe::exact;
use crate::os::{self, HardLink};

/// Sizes trees on its own bounded thread pool. It reads no ignore file or git config,
/// skips nothing hidden, never follows a link or junction, and stays on the file system
/// of the item it sizes.
///
/// A directory it leaves out for being on another file system is reported in the
/// measurement.
#[derive(Debug)]
pub struct Walker {
    pool: rayon::ThreadPool,
    device: DeviceOf,
}

/// The file system an entry lives on, where the platform reports one. A seam so a test
/// can put a directory on another device without a real mount.
type DeviceOf = fn(&Path, &fs::Metadata) -> Option<u64>;

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
        Ok(Self {
            pool,
            device: |_, meta| os::device(meta),
        })
    }
}

impl Sizer for Walker {
    fn size(&self, path: &Path, cancel: &CancelToken) -> Result<Measurement, ProbeError> {
        let item = exact(path);
        let meta = fs::symlink_metadata(&item).map_err(|e| ProbeError::io(path, e))?;
        let tally = Tally::default();
        let mut top = Counts::default();
        top.add(&meta);
        tally.merge(top);
        if meta.is_dir() {
            let walk = Walk {
                device: (self.device)(&item, &meta),
                device_of: self.device,
                tally: &tally,
                cancel,
            };
            self.pool.scope(|scope| visit(scope, &item, walk));
        }
        Ok(tally.measurement())
    }
}

/// What every task of one walk shares.
#[derive(Clone, Copy)]
struct Walk<'scan> {
    /// The item's file system.
    device: Option<u64>,
    device_of: DeviceOf,
    tally: &'scan Tally,
    cancel: &'scan CancelToken,
}

/// Sizes the entries of `dir` and schedules each subdirectory on the same device as its
/// own task. A subdirectory on another device is noted, not entered. Once cancelled, no
/// entry is read and no directory is scheduled anymore.
fn visit<'s>(scope: &rayon::Scope<'s>, dir: &Path, walk: Walk<'s>) {
    let Walk { tally, cancel, .. } = walk;
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
        if entry.file_name() == ".git" {
            counts.repository(dir);
        }
        if !meta.is_dir() {
            continue;
        }
        let sub = entry.path();
        if (walk.device_of)(&sub, &meta) == walk.device {
            scope.spawn(move |scope| visit(scope, &sub, walk));
        } else {
            counts.skipped_mounts += 1;
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
    newest: Newest,
    repository: Option<PathBuf>,
    skipped_mounts: u64,
}

impl Counts {
    /// Records that `dir` holds a `.git` entry.
    fn repository(&mut self, dir: &Path) {
        self.repository = Some(dir.to_owned());
    }

    fn add(&mut self, meta: &fs::Metadata) {
        self.newest.add(meta.modified().ok());
        let apparent = if meta.is_dir() { 0 } else { meta.len() };
        let allocated = os::allocated(meta);
        self.allocation_unknown |= allocated.is_none();
        let allocated = allocated.unwrap_or(0);
        let Some(HardLink { identity, names }) = os::hard_link(meta) else {
            self.apparent += apparent;
            self.allocated += allocated;
            return;
        };
        let linked = Linked {
            links: names,
            names_seen: 1,
            apparent,
            allocated,
        };
        self.linked.push((identity, linked));
    }
}

/// The newest modification time seen. One entry without a time leaves it unknown.
#[derive(Debug, Default, Clone, Copy)]
struct Newest {
    seen: Option<SystemTime>,
    unknown: bool,
}

impl Newest {
    fn add(&mut self, modified: Option<SystemTime>) {
        match modified {
            Some(time) => self.seen = Some(self.seen.map_or(time, |seen| seen.max(time))),
            None => self.unknown = true,
        }
    }

    fn merge(&mut self, other: Self) {
        self.unknown |= other.unknown;
        if let Some(time) = other.seen {
            self.add(Some(time));
        }
    }

    const fn time(self) -> Option<SystemTime> {
        if self.unknown { None } else { self.seen }
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
    skipped_mounts: AtomicU64,
    linked: Mutex<HashMap<FileIdentity, Linked>>,
    newest: Mutex<Newest>,
    repository: Mutex<Option<PathBuf>>,
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
        self.skipped_mounts
            .fetch_add(counts.skipped_mounts, Ordering::Relaxed);
        if counts.allocation_unknown {
            self.allocation_unknown.store(true, Ordering::Relaxed);
        }
        let mut newest = self.newest.lock().unwrap_or_else(PoisonError::into_inner);
        newest.merge(counts.newest);
        drop(newest);
        if let Some(found) = counts.repository {
            let mut repository = self
                .repository
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // The first in path order, whichever task met it first.
            if repository.as_ref().is_none_or(|known| found < *known) {
                *repository = Some(found);
            }
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

    fn measurement(self) -> Measurement {
        let modified = self
            .newest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .time();
        let repository = self
            .repository
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        Measurement {
            size: self.estimate(),
            modified,
            repository,
        }
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
        let skipped_mounts = self.skipped_mounts.into_inner();
        let completeness = if self.cancelled.into_inner() {
            Completeness::Cancelled
        } else if unreadable > 0 || skipped_mounts > 0 {
            Completeness::Incomplete {
                unreadable,
                skipped_mounts,
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Puts any directory named `mounted` on a device of its own.
    #[expect(clippy::unnecessary_wraps, reason = "it stands in for the device seam")]
    fn mounted_apart(path: &Path, _: &fs::Metadata) -> Option<u64> {
        Some(if path.ends_with("mounted") { 2 } else { 1 })
    }

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![7_u8; bytes]).unwrap();
    }

    #[test]
    fn a_directory_on_another_file_system_is_left_out_and_makes_the_size_a_lower_bound() {
        let temp = tempfile::TempDir::new().unwrap();
        let item = temp.path().join("item");
        write(&item.join("own"), 100);
        write(&item.join("sub/mounted/big"), 1 << 20);
        write(&item.join("sub/mounted/checkout/.git/HEAD"), 10);
        let walker = Walker {
            device: mounted_apart,
            ..Walker::new().unwrap()
        };

        let measured = walker.size(&item, &CancelToken::new()).unwrap();

        assert_eq!(
            measured.size.completeness,
            Completeness::Incomplete {
                unreadable: 0,
                skipped_mounts: 1
            }
        );
        assert!(measured.size.apparent < Apparent(1 << 20));
        assert_eq!(measured.repository, None);
    }
}
