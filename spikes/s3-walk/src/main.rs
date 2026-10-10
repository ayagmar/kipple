//! Spike S3 (throwaway, branch `spike/m1-s2-s3` only): walk and size a tree with one of
//! three variants and print the totals, so the variants can be checked against each other
//! and timed by `.github/scripts/bench.py`.
//!
//! Usage: `s3-walk <ignore|dua|rayon> <dir> [threads]`
//!
//! Every variant counts the entries below `dir` (not `dir` itself), never follows
//! symlinks, stays on `dir`'s file system on Linux, and sums the apparent size of
//! everything that is not a directory.
#![forbid(unsafe_code)]

use std::fs;
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Debug, Default, Clone, Copy)]
struct Totals {
    dirs: u64,
    files: u64,
    bytes: u64,
    errors: u64,
}

impl Totals {
    const fn add_entry(&mut self, is_dir: bool, len: u64) {
        if is_dir {
            self.dirs += 1;
        } else {
            self.files += 1;
            self.bytes += len;
        }
    }

    const fn merge(&mut self, other: Self) {
        self.dirs += other.dirs;
        self.files += other.files;
        self.bytes += other.bytes;
        self.errors += other.errors;
    }
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(variant), Some(dir)) = (args.get(1), args.get(2)) else {
        return Err(io::Error::other(
            "usage: s3-walk <ignore|dua|rayon> <dir> [threads]",
        ));
    };
    let threads = match args.get(3) {
        Some(n) => n.parse().map_err(io::Error::other)?,
        None => std::thread::available_parallelism()?.get(),
    };
    let root = Path::new(dir);
    let start = Instant::now();
    let totals = match variant.as_str() {
        "ignore" => walk_ignore(root, threads),
        "dua" => walk_dua(root, threads),
        "rayon" => walk_rayon(root, threads)?,
        other => return Err(io::Error::other(format!("unknown variant {other}"))),
    };
    let elapsed = start.elapsed();
    writeln!(
        io::stdout(),
        "entries={} dirs={} files={} bytes={} errors={} threads={threads} elapsed_ms={}",
        totals.dirs + totals.files,
        totals.dirs,
        totals.files,
        totals.bytes,
        totals.errors,
        elapsed.as_millis()
    )
}

/// `ignore`'s parallel walker, configured as in docs/05-architecture.md §3.
fn walk_ignore(root: &Path, threads: usize) -> Totals {
    let shared = Mutex::new(Totals::default());
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .require_git(false)
        .follow_links(false)
        .same_file_system(true)
        .threads(threads)
        .build_parallel()
        .visit(&mut IgnoreVisitors { shared: &shared });
    shared
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct IgnoreVisitors<'a> {
    shared: &'a Mutex<Totals>,
}

impl<'s> ignore::ParallelVisitorBuilder<'s> for IgnoreVisitors<'s> {
    fn build(&mut self) -> Box<dyn ignore::ParallelVisitor + 's> {
        Box::new(IgnoreVisitor {
            local: Totals::default(),
            shared: self.shared,
        })
    }
}

/// Counts locally and merges into the shared totals when the walker drops it.
struct IgnoreVisitor<'a> {
    local: Totals,
    shared: &'a Mutex<Totals>,
}

impl ignore::ParallelVisitor for IgnoreVisitor<'_> {
    fn visit(&mut self, entry: Result<ignore::DirEntry, ignore::Error>) -> ignore::WalkState {
        match entry {
            Ok(entry) if entry.depth() == 0 => {}
            Ok(entry) => match entry.metadata() {
                Ok(meta) => self.local.add_entry(meta.is_dir(), meta.len()),
                Err(_) => self.local.errors += 1,
            },
            Err(_) => self.local.errors += 1,
        }
        ignore::WalkState::Continue
    }
}

impl Drop for IgnoreVisitor<'_> {
    fn drop(&mut self) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.merge(self.local);
        }
    }
}

/// `dua-core`'s work-stealing walker, with metadata.
fn walk_dua(root: &Path, threads: usize) -> Totals {
    let root_dev = root_device(root);
    let walk = dua_core::walk(
        root,
        threads,
        dua_core::Order::Completion,
        dua_core::Options::default(),
        move |entry| match (&entry.metadata, root_dev) {
            (Some(Ok(meta)), Some(dev)) => dua_device(meta) == Some(dev),
            _ => true,
        },
    );
    let mut totals = Totals::default();
    for entry in walk {
        match entry {
            Ok(entry) if entry.depth == 0 => {}
            Ok(entry) => match entry.metadata {
                Some(Ok(meta)) => totals.add_entry(entry.file_type.is_dir(), meta.len()),
                _ => totals.errors += 1,
            },
            Err(_) => totals.errors += 1,
        }
    }
    totals
}

/// Plain `std::fs::read_dir` plus `symlink_metadata` per entry, one rayon task per directory.
fn walk_rayon(root: &Path, threads: usize) -> io::Result<Totals> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(io::Error::other)?;
    let shared = SharedTotals::default();
    let root_dev = root_device(root);
    pool.scope(|scope| visit_dir(scope, root, root_dev, &shared));
    Ok(shared.load())
}

fn visit_dir<'s>(
    scope: &rayon::Scope<'s>,
    dir: &Path,
    root_dev: Option<u64>,
    shared: &'s SharedTotals,
) {
    let mut local = Totals::default();
    let Ok(entries) = fs::read_dir(dir) else {
        shared.add(Totals {
            errors: 1,
            ..Totals::default()
        });
        return;
    };
    for entry in entries {
        let Ok((path, meta)) = entry.and_then(|e| Ok((e.path(), e.metadata()?))) else {
            local.errors += 1;
            continue;
        };
        local.add_entry(meta.is_dir(), meta.len());
        if meta.is_dir() && (root_dev.is_none() || std_device(&meta) == root_dev) {
            scope.spawn(move |scope| visit_dir(scope, &path, root_dev, shared));
        }
    }
    shared.add(local);
}

#[derive(Default)]
struct SharedTotals {
    dirs: AtomicU64,
    files: AtomicU64,
    bytes: AtomicU64,
    errors: AtomicU64,
}

impl SharedTotals {
    fn add(&self, totals: Totals) {
        self.dirs.fetch_add(totals.dirs, Ordering::Relaxed);
        self.files.fetch_add(totals.files, Ordering::Relaxed);
        self.bytes.fetch_add(totals.bytes, Ordering::Relaxed);
        self.errors.fetch_add(totals.errors, Ordering::Relaxed);
    }

    fn load(&self) -> Totals {
        Totals {
            dirs: self.dirs.load(Ordering::Relaxed),
            files: self.files.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

/// The root's device on Linux, where the variants stay on one file system.
fn root_device(root: &Path) -> Option<u64> {
    std_device(&fs::symlink_metadata(root).ok()?)
}

#[cfg(target_os = "linux")]
#[expect(clippy::unnecessary_wraps, reason = "the other OSes return None")]
fn std_device(meta: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    Some(meta.dev())
}

#[cfg(not(target_os = "linux"))]
const fn std_device(_: &fs::Metadata) -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn dua_device(meta: &dua_core::Metadata) -> Option<u64> {
    std_device(meta)
}

#[cfg(not(target_os = "linux"))]
const fn dua_device(_: &dua_core::Metadata) -> Option<u64> {
    None
}
