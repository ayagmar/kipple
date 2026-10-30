//! The search for projects outside the project roots (D-053, `docs/03-functional-spec.md`
//! §6): a read-only, bounded walk of the home directory. What to suggest from what it
//! finds is decided by `kipple_core::suggest`.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kipple_core::{CancelToken, Clock, FileIdentity, HeldPath, is_within};

use crate::fs_probe::exact;
use crate::os;

/// How far the search may go before it stops early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchLimits {
    /// How many levels below home it looks.
    pub depth: usize,
    /// How many directories it reads.
    pub dirs: usize,
    /// How long it runs, by the clock it is given.
    pub time: Duration,
}

impl Default for SearchLimits {
    /// 4 levels, 50 000 directories, 2 seconds.
    fn default() -> Self {
        Self {
            depth: 4,
            dirs: 50_000,
            time: Duration::from_secs(2),
        }
    }
}

/// What to search and what to leave out.
#[derive(Debug, Clone, Copy)]
pub struct ProjectSearch<'config> {
    /// The resolved home directory the search starts from. It is never a project itself.
    pub home: &'config Path,
    /// The file names that mark a project besides `.git`: the markers of the built-in
    /// `project-artifact` rules.
    pub markers: &'config [OsString],
    /// Directories never entered, with everything inside them: every form of the existing
    /// project roots, and kipple's own directories. Compared by path and by identity, so a
    /// root given by its physical path is skipped when home is reached through a link.
    pub skip: &'config [PathBuf],
    /// The config's `exclude.paths`, compared by path and by identity.
    pub exclude: &'config [HeldPath],
    /// When it stops early.
    pub limits: SearchLimits,
}

/// The projects the search found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectsFound {
    /// Each directory holding `.git` or a marker, sorted.
    pub projects: Vec<PathBuf>,
    /// Whether it looked everywhere it may: `false` when a limit or a cancel stopped it,
    /// or home itself couldn't be read, so more projects may exist.
    pub complete: bool,
    /// How many directories it couldn't read, and so skipped.
    pub unreadable: usize,
}

/// Searches `search.home` for projects, breadth first, reading only.
///
/// It skips hidden directories (a name starting with `.`, or hidden by the OS), never
/// follows a link, stays on home's file system, and doesn't enter a project it found, a
/// directory `search.skip` or `search.exclude` covers, or anything deeper than
/// `search.limits.depth` below home. It stops early once it read `limits.dirs`
/// directories, once `limits.time` passed by `clock`, or once `cancel` is set.
#[must_use]
pub fn search_projects(
    search: &ProjectSearch<'_>,
    clock: &dyn Clock,
    cancel: &CancelToken,
) -> ProjectsFound {
    let home = exact(search.home);
    // A home that can't be read was never searched, so the list may be incomplete.
    let unread = ProjectsFound {
        complete: false,
        unreadable: 1,
        ..ProjectsFound::default()
    };
    let Ok(meta) = fs::symlink_metadata(&home) else {
        return unread;
    };
    if !meta.is_dir() {
        return unread;
    }
    let mut found = ProjectsFound {
        complete: true,
        ..ProjectsFound::default()
    };
    let walk = Walk {
        search,
        device: os::device(&meta),
        deadline: clock.now().checked_add(search.limits.time),
        held: held(search),
    };
    let mut queue = VecDeque::from([(home, 0)]);
    let mut read = 0;
    while let Some((dir, depth)) = queue.pop_front() {
        if cancel.is_cancelled() || read >= search.limits.dirs || walk.expired(clock) {
            found.complete = false;
            break;
        }
        read += 1;
        let Some(listing) = walk.list(&dir) else {
            found.unreadable += 1;
            if depth == 0 {
                found.complete = false;
            }
            continue;
        };
        if depth > 0 && listing.project {
            found.projects.push(dir);
        } else if depth < search.limits.depth {
            queue.extend(listing.subdirs.into_iter().map(|sub| (sub, depth + 1)));
        }
    }
    found.projects.sort();
    found
}

/// What one directory holds, as far as the search cares.
struct Listing {
    /// It holds `.git` or a marker.
    project: bool,
    /// The subdirectories the search may enter.
    subdirs: Vec<PathBuf>,
}

/// One search's settings, as each directory is read.
struct Walk<'search> {
    search: &'search ProjectSearch<'search>,
    /// Home's file system.
    device: Option<u64>,
    /// When the time limit runs out; `None` when it is too far to represent.
    deadline: Option<SystemTime>,
    /// The identities of the directories skipped and excluded, as far as they could be
    /// read.
    held: Vec<FileIdentity>,
}

/// The identities of `search.skip`, read now, and those `search.exclude` carries.
fn held(search: &ProjectSearch<'_>) -> Vec<FileIdentity> {
    let skipped = search.skip.iter().filter_map(|skip| {
        let meta = fs::symlink_metadata(skip).ok()?;
        os::identity(skip, &meta)
    });
    let excluded = search.exclude.iter().filter_map(|held| held.lineage.own());
    skipped.chain(excluded).collect()
}

impl Walk<'_> {
    fn expired(&self, clock: &dyn Clock) -> bool {
        self.deadline
            .is_some_and(|deadline| clock.now() >= deadline)
    }

    /// The listing of `dir`, or `None` when it or one of its entries can't be read: a
    /// partial listing could miss the marker that makes it a project.
    fn list(&self, dir: &Path) -> Option<Listing> {
        let mut listing = Listing {
            project: false,
            subdirs: Vec::new(),
        };
        for entry in fs::read_dir(dir).ok()? {
            let entry = entry.ok()?;
            let name = entry.file_name();
            // `.git` counts whatever it is: a worktree's is a file, and a link is not
            // followed, only seen.
            if name == ".git" || self.search.markers.contains(&name) {
                listing.project = true;
            }
            let meta = entry.metadata().ok()?;
            let sub = entry.path();
            if self.enters(&name, &sub, &meta) {
                listing.subdirs.push(sub);
            }
        }
        Some(listing)
    }

    /// Whether the entry `name` at `path` with the no-follow `meta` is a directory the
    /// search may enter.
    fn enters(&self, name: &OsStr, path: &Path, meta: &fs::Metadata) -> bool {
        meta.is_dir()
            && !name.to_string_lossy().starts_with('.')
            && !os::hidden(meta)
            && os::device(meta) == self.device
            && !self.search.skip.iter().any(|skip| is_within(path, skip))
            && !self
                .search
                .exclude
                .iter()
                .any(|excluded| excluded.covers(path))
            && !self.held_by_identity(path, meta)
    }

    /// Whether `path` is a skipped or excluded directory by its identity, as the project
    /// walk compares excluded paths: reached by another path, it is still the same one.
    fn held_by_identity(&self, path: &Path, meta: &fs::Metadata) -> bool {
        if self.held.is_empty() {
            return false;
        }
        os::identity(path, meta).is_some_and(|own| self.held.contains(&own))
    }
}
