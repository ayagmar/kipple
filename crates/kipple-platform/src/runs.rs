//! The run store (spec M3b pinned 6–10, D-065): where every mutating command keeps its
//! stored plan and journal, under one lock.
//!
//! Everything is under `<data>/runs/` (mode 0700): the lock file `.lock` (0600), which is
//! never removed or renamed, and one directory per run named by its [`RunId`] (0700),
//! holding `plan`, the stored plan (0600, a JSON header line and the trees' binary
//! blobs), and `journal.jsonl` (0600, one JSON line per record). `runs/`, each run's
//! directory and each file in it are opened without following a link. The store moves
//! bytes; the core decides what they say.
//!
//! Every write is durable before it returns: each file and each directory that gained a
//! name is synced (`fsync`, `F_FULLFSYNC` on macOS). Windows writes files through
//! (`FILE_WRITE_THROUGH`) and flushes each directory as the executor does.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::io::{self, BufRead as _, BufReader, Read as _, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use kipple_core::{
    ApplyError, CancelToken, Clock, ErrorCode, FileSystemClass, HEADER_LIMIT, JOURNAL_LIMIT,
    LOCK_WAIT, LockState, Locked, OsError, RunId, RunStore, StoreRefusal,
};

#[cfg(unix)]
use unix::{Folder, file_system_at, is_link};
#[cfg(windows)]
use windows::{Folder, file_system_at, is_link};

use crate::os;

/// The directory under the data directory that holds the runs.
const RUNS: &str = "runs";

/// The lock file in [`RUNS`].
const LOCK: &str = ".lock";

/// A run's stored plan.
const PLAN: &str = "plan";

/// What the stored plan is written as before it is renamed into place.
const PLAN_PARTIAL: &str = "plan.partial";

/// A run's journal.
const JOURNAL: &str = "journal.jsonl";

/// How long a command waits between two tries of a lock another holds.
const RETRY: Duration = Duration::from_millis(20);

/// How many runs can start in one second: [`RunId`]'s counter.
const PER_SECOND: u16 = 10_000;

/// The platform's [`RunStore`]: files under kipple's data directory.
#[derive(Debug, Clone)]
pub struct FileRunStore {
    /// kipple's data directory, `None` when it didn't resolve.
    data: Option<PathBuf>,
    /// When a run is created, for its ID.
    clock: Arc<dyn Clock>,
}

impl FileRunStore {
    /// The store under `data`, kipple's data directory, `None` when it isn't known. Its
    /// runs are named by `clock`.
    #[must_use]
    pub fn new(data: Option<PathBuf>, clock: Arc<dyn Clock>) -> Self {
        Self { data, clock }
    }

    /// `<data>/runs`, opened; `None` when there is none yet.
    ///
    /// # Errors
    /// When the data directory isn't known, or `runs` can't be opened, a link included.
    fn open_runs(&self) -> Result<Option<Folder>, ApplyError> {
        let data = self
            .data
            .as_deref()
            .ok_or_else(|| ApplyError::new(ErrorCode::Unsupported))?;
        match Folder::open(&data.join(RUNS)) {
            Ok(runs) => Ok(Some(runs)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(failed(error)),
        }
    }

    /// `run`'s directory, opened.
    fn open_run(&self, run: &RunId) -> Result<Folder, ApplyError> {
        let runs = self
            .open_runs()?
            .ok_or_else(|| ApplyError::new(ErrorCode::Io))?;
        runs.open_in(run.as_str()).map_err(failed)
    }

    /// `name` in `run`'s directory, opened for reading; `None` when there is none.
    fn read(&self, run: &RunId, name: &str) -> Result<Option<File>, ApplyError> {
        match self.open_run(run)?.read(name).and_then(regular) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(failed(error)),
        }
    }

    /// Creates a run in `runs`, on a file system with `free` bytes free: see
    /// [`RunStore::create`].
    fn create_in(
        &self,
        runs: &Folder,
        free: u64,
        plan: &[u8],
        first: &[u8],
    ) -> Result<RunId, StoreRefusal> {
        let needed = (plan.len() + first.len()) as u64;
        if free < needed {
            return Err(StoreRefusal::Space { needed, free });
        }
        let (id, dir) = self.new_run(runs)?;
        runs.sync().map_err(unsynced("syncing runs/"))?;
        write_new(&dir, PLAN_PARTIAL, plan).map_err(refused("writing plan.partial"))?;
        dir.rename(PLAN_PARTIAL, PLAN)
            .map_err(refused("renaming plan.partial to plan"))?;
        dir.sync()
            .map_err(unsynced("syncing the run after its plan"))?;
        write_new(&dir, JOURNAL, first).map_err(refused("writing the journal"))?;
        dir.sync()
            .map_err(unsynced("syncing the run after its journal"))?;
        Ok(id)
    }

    /// Creates a run's directory under `runs` with the first ID free for now.
    fn new_run(&self, runs: &Folder) -> Result<(RunId, Folder), StoreRefusal> {
        let now = self.clock.now();
        for counter in 0..PER_SECOND {
            let id = RunId::at(now, counter);
            match runs.create_dir(id.as_str()) {
                Ok(()) => {
                    let dir = runs
                        .open_in(id.as_str())
                        .map_err(refused("opening the run's directory"))?;
                    return Ok((id, dir));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(refused("creating the run's directory")(error)),
            }
        }
        Err(StoreRefusal::Write(ApplyError::new(
            ErrorCode::DestinationExists,
        )))
    }
}

impl RunStore for FileRunStore {
    fn lock(&self, cancel: &CancelToken) -> Result<Locked, StoreRefusal> {
        let data = self.data.as_deref().ok_or(StoreRefusal::NoDataDir)?;
        supported(made_on(data))?;
        let runs = make_runs(data).map_err(refused("making runs/"))?;
        supported(runs.file_system())?;
        let file = runs
            .lock_file(LOCK)
            .and_then(regular)
            .map_err(|error| lock_failed(&error))?;
        wait_for(&file, cancel)?;
        Ok(Locked::new(file))
    }

    fn lock_state(&self) -> Result<LockState, StoreRefusal> {
        if self.data.is_none() {
            return Err(StoreRefusal::NoDataDir);
        }
        let Some(runs) = self.open_runs().map_err(StoreRefusal::Write)? else {
            return Ok(LockState::Free);
        };
        let file = match runs.read(LOCK).and_then(regular) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(LockState::Free),
            Err(error) => return Err(lock_failed(&error)),
        };
        // Shared, so two `history` probes never stand in each other's way.
        match file.try_lock_shared() {
            Ok(()) => Ok(LockState::Free),
            Err(std::fs::TryLockError::WouldBlock) => Ok(LockState::Held),
            Err(std::fs::TryLockError::Error(error)) => Err(lock_failed(&error)),
        }
    }

    fn runs(&self) -> Result<Vec<RunId>, ApplyError> {
        let Some(runs) = self.open_runs()? else {
            return Ok(Vec::new());
        };
        let mut ids: Vec<RunId> = runs
            .names()
            .map_err(failed)?
            .iter()
            .filter_map(|name| name.to_str().and_then(RunId::parse))
            .collect();
        ids.sort();
        Ok(ids)
    }

    fn header(&self, run: &RunId) -> Result<Option<Vec<u8>>, ApplyError> {
        let Some(file) = self.read(run, PLAN)? else {
            return Ok(None);
        };
        let mut header = Vec::new();
        BufReader::new(file)
            .take(HEADER_LIMIT)
            .read_until(b'\n', &mut header)
            .map_err(failed)?;
        if header.last() != Some(&b'\n') && header.len() as u64 == HEADER_LIMIT {
            return Err(ApplyError::new(ErrorCode::TooLarge));
        }
        Ok(Some(header))
    }

    fn plan(&self, run: &RunId) -> Result<Option<Vec<u8>>, ApplyError> {
        let Some(mut file) = self.read(run, PLAN)? else {
            return Ok(None);
        };
        let mut plan = Vec::new();
        io::copy(&mut file, &mut plan).map_err(failed)?;
        Ok(Some(plan))
    }

    fn journal(&self, run: &RunId) -> Result<Vec<u8>, ApplyError> {
        let Some(file) = self.read(run, JOURNAL)? else {
            return Ok(Vec::new());
        };
        // Refused before reading when it is already too long; the read is bounded too, as
        // `history` reads without the lock while an apply appends.
        if file.metadata().map_err(failed)?.len() > JOURNAL_LIMIT {
            return Err(ApplyError::new(ErrorCode::TooLarge));
        }
        let mut journal = Vec::new();
        file.take(JOURNAL_LIMIT + 1)
            .read_to_end(&mut journal)
            .map_err(failed)?;
        if journal.len() as u64 > JOURNAL_LIMIT {
            return Err(ApplyError::new(ErrorCode::TooLarge));
        }
        Ok(journal)
    }

    fn create(&self, _: &Locked, plan: &[u8], first: &[u8]) -> Result<RunId, StoreRefusal> {
        let data = self.data.as_deref().ok_or(StoreRefusal::NoDataDir)?;
        let runs = Folder::open(&data.join(RUNS)).map_err(refused("opening runs/"))?;
        let free = runs.free().map_err(refused("reading the free space"))?;
        self.create_in(&runs, free, plan, first)
    }

    fn append(&self, _: &Locked, run: &RunId, at: u64, line: &[u8]) -> Result<u64, ApplyError> {
        let mut file = self
            .open_run(run)?
            .write(JOURNAL)
            .and_then(regular)
            .map_err(failed)?;
        append_at(&mut file, at, line)
    }
}

/// The file system `<data>/runs` is made on: that of `data`, or of the nearest directory
/// above it when it is missing, reached through links as making them reaches it.
fn made_on(data: &Path) -> io::Result<FileSystemClass> {
    let existing = data
        .ancestors()
        .find(|dir| dir.exists())
        .ok_or(io::ErrorKind::NotFound)?;
    file_system_at(existing)
}

/// Refuses a file system outside the executor's allowlist, or one that can't be told.
fn supported(class: io::Result<FileSystemClass>) -> Result<(), StoreRefusal> {
    match class {
        Ok(FileSystemClass::Supported(_)) => Ok(()),
        Ok(FileSystemClass::Unsupported(why)) => Err(StoreRefusal::FileSystem(why)),
        Err(error) => Err(lock_failed(&error)),
    }
}

/// Opens `<data>/runs`, creating it and any missing parent first (mode 0700), each synced
/// into the directory that gained it.
fn make_runs(data: &Path) -> io::Result<Folder> {
    let runs = data.join(RUNS);
    match Folder::open(&runs) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        opened => return opened,
    }
    let missing: Vec<&Path> = runs.ancestors().take_while(|dir| !dir.exists()).collect();
    os::create_private_dir(&runs)?;
    for dir in missing {
        if let Some(parent) = dir.parent() {
            os::sync_dir(parent)?;
        }
    }
    Folder::open(&runs)
}

/// Takes `file`'s lock, trying again for up to [`LOCK_WAIT`] while another process holds
/// it, or until `cancel`.
fn wait_for(file: &File, cancel: &CancelToken) -> Result<(), StoreRefusal> {
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => return Err(lock_failed(&error)),
        }
        if cancel.is_cancelled() || started.elapsed() >= LOCK_WAIT {
            return Err(StoreRefusal::Busy);
        }
        thread::park_timeout(RETRY);
    }
}

/// Writes `bytes` to a new file `name` in `dir` and syncs it. A file left part-written
/// is emptied, so a failed write doesn't keep the space it took.
fn write_new(dir: &Folder, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut file = dir.create(name)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if written.is_err() {
        file.set_len(0).ok();
    }
    written
}

/// What [`append_at`] needs of a journal file. A file that fails stands in for a full
/// disk in tests.
trait Log: Write + Seek {
    /// Its length.
    fn size(&self) -> io::Result<u64>;
    /// Cuts it to `len` bytes.
    fn cut(&self, len: u64) -> io::Result<()>;
    /// Makes it durable.
    fn sync(&self) -> io::Result<()>;
}

impl Log for File {
    fn size(&self) -> io::Result<u64> {
        self.metadata().map(|meta| meta.len())
    }

    fn cut(&self, len: u64) -> io::Result<()> {
        self.set_len(len)
    }

    fn sync(&self) -> io::Result<()> {
        self.sync_all()
    }
}

/// Writes `line` at byte `at` of `log` and syncs it, after cutting off what is past `at`
/// (a torn or untrusted tail) and syncing that. A line that isn't whole and durable is
/// cut off again. Returns the new length.
fn append_at(log: &mut impl Log, at: u64, line: &[u8]) -> Result<u64, ApplyError> {
    let journal = |error| ApplyError::io(ErrorCode::Journal, error);
    let size = log.size().map_err(journal)?;
    if size < at {
        return Err(ApplyError::new(ErrorCode::Journal));
    }
    if size > at {
        log.cut(at).and_then(|()| log.sync()).map_err(journal)?;
    }
    let written = log
        .seek(SeekFrom::Start(at))
        .and_then(|_| log.write_all(line))
        .and_then(|()| log.sync());
    if let Err(error) = written {
        log.cut(at).and_then(|()| log.sync()).ok();
        return Err(journal(error));
    }
    Ok(at + line.len() as u64)
}

/// `file`, if it is a regular file: never a directory, device or pipe.
fn regular(file: File) -> io::Result<File> {
    if file.metadata()?.is_file() {
        Ok(file)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ))
    }
}

/// A read or write that failed: through a link where the store never follows one, or
/// another I/O error.
fn failed(error: io::Error) -> ApplyError {
    let code = if is_link(&error) {
        ErrorCode::ThroughLink
    } else {
        ErrorCode::Io
    };
    ApplyError::io(code, error)
}

/// Creating or writing a run failed at `step`.
fn refused(step: &'static str) -> impl FnOnce(io::Error) -> StoreRefusal {
    move |error| StoreRefusal::Write(during(step, failed(error)))
}

/// A run was written but couldn't be made durable at `step`.
fn unsynced(step: &'static str) -> impl FnOnce(io::Error) -> StoreRefusal {
    move |error| StoreRefusal::Write(during(step, ApplyError::io(ErrorCode::Unsynced, error)))
}

/// `error` with `step` named in its detail, which is shown and never stored: the OS
/// error alone doesn't say which of the store's calls failed.
fn during(step: &'static str, mut error: ApplyError) -> ApplyError {
    if let Some(detail) = error.detail.take() {
        let named = io::Error::new(detail.kind(), format!("{step}: {detail}"));
        error.detail = Some(Arc::new(named));
    }
    error
}

fn lock_failed(error: &io::Error) -> StoreRefusal {
    StoreRefusal::Lock(Some(OsError::of(error)))
}
