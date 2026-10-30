//! The run store port: where every mutating command keeps its runs (spec M3b pinned 6–10,
//! D-065). Implemented only by `kipple-platform`.
//!
//! The store keeps bytes; the core decides what they say. Each run is a directory under
//! `<data>/runs/` holding its stored plan (`kipple.stored-plan/v1`, written once) and its
//! journal (`kipple.journal/v1`, appended). The core encodes every record, decides which
//! journal bytes are sound, and tells the store where the next record goes: the store
//! never parses a record.
//!
//! One mutating command runs at a time, under `<data>/runs/.lock`. Every call that
//! changes the store takes the [`Locked`] proof of it.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cancel::CancelToken;
use crate::mutation::Unsupported;
use crate::outcome::{ApplyError, OsError};

/// How long a mutating command waits for another to release the lock before it refuses,
/// so a `history` that probes the lock never fails a scheduled `clean`.
pub const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// The most bytes a stored plan's header line may take.
pub const HEADER_LIMIT: u64 = 64 * 1024 * 1024;

/// The most bytes a run's journal may take.
pub const JOURNAL_LIMIT: u64 = 256 * 1024 * 1024;

/// A run's ID, such as `20261030T142501Z-0000`.
///
/// When it was created, in UTC, and a counter for runs created in the same second. IDs sort in the order runs were created,
/// unless the clock stepped back; the store then bumps the counter until the ID is new.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunId(String);

impl RunId {
    /// The ID of a run created at `time` with `counter`. A time before 1970 counts as
    /// 1970; the counter wraps at 10 000.
    #[must_use]
    pub fn at(time: SystemTime, counter: u16) -> Self {
        let secs = time
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX);
        let (year, month, day) = civil(days);
        let rest = secs % 86_400;
        Self(format!(
            "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z-{:04}",
            rest / 3600,
            rest / 60 % 60,
            rest % 60,
            counter % 10_000,
        ))
    }

    /// The ID `text` writes, if it is one: what [`RunId::at`] writes.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        let shape = bytes.len() == 21
            && bytes.iter().enumerate().all(|(index, byte)| match index {
                8 => *byte == b'T',
                15 => *byte == b'Z',
                16 => *byte == b'-',
                _ => byte.is_ascii_digit(),
            });
        shape.then(|| Self(text.to_owned()))
    }

    /// The ID as written: the run's directory name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0)
    }
}

/// The civil date `days` after 1970-01-01 (proleptic Gregorian).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

/// Proof that this process holds the store's lock. The lock is released when it drops.
pub struct Locked(Box<dyn fmt::Debug + Send>);

impl Locked {
    /// The proof of a lock that `guard` holds until it drops. Only a [`RunStore`] makes
    /// one.
    #[must_use]
    pub fn new(guard: impl fmt::Debug + Send + 'static) -> Self {
        Self(Box::new(guard))
    }
}

impl fmt::Debug for Locked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Locked").field(&self.0).finish()
    }
}

named! {
    /// Whether another process holds the store's lock right now.
    LockState {
        /// Nobody does.
        Free = "free",
        /// Another kipple does: a run without its outcomes is running, not interrupted.
        Held = "held",
    }
}

/// Why the store refuses to change anything. The command changes nothing and exits 1.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreRefusal {
    /// Another kipple held the lock for longer than [`LOCK_WAIT`].
    #[error("another kipple apply is running")]
    Busy,
    /// Taking the lock failed for another reason, such as a file system without locks.
    #[error("the run store's lock failed")]
    Lock(Option<OsError>),
    /// kipple's data directory isn't known.
    #[error("kipple's data directory isn't known")]
    NoDataDir,
    /// The data directory is on a file system kipple doesn't change (D-061), network
    /// homes included: its syncs and locks can't be relied on.
    #[error("kipple's data directory is on an unsupported file system")]
    FileSystem(Unsupported),
    /// Too little free space to store the plan.
    #[error("too little free space for the plan record")]
    Space {
        /// Bytes the plan needs.
        needed: u64,
        /// Bytes free.
        free: u64,
    },
    /// Creating the run, or writing it durably, failed.
    #[error("the run couldn't be stored")]
    Write(ApplyError),
    /// This build has no run store.
    #[error("this build can't store runs")]
    Unsupported,
}

/// Where kipple keeps its runs. Implemented by the platform, faked in tests.
///
/// Every write is durable before it returns: on Linux and macOS each file and the
/// directories its name is in are synced with `fsync` (`F_FULLFSYNC` on macOS); on Windows
/// files are written through and renamed with `MOVEFILE_WRITE_THROUGH`, with NTFS's
/// metadata journal standing for the directory sync (D-065).
pub trait RunStore: fmt::Debug + Send + Sync {
    /// Takes the lock for a mutating command, waiting up to [`LOCK_WAIT`] while another
    /// process holds it. Also refuses when the data directory isn't on a supported local
    /// file system.
    ///
    /// # Errors
    /// [`StoreRefusal::Busy`] when another holds it longer, another refusal when the lock
    /// can't be taken at all or the store can't be used.
    fn lock(&self, cancel: &CancelToken) -> Result<Locked, StoreRefusal>;

    /// Whether another process holds the lock now, without waiting and without keeping
    /// it. For `history`.
    ///
    /// # Errors
    /// When that can't be told.
    fn lock_state(&self) -> Result<LockState, StoreRefusal>;

    /// Every run, by ID, in order. A directory whose name isn't a [`RunId`] isn't a run.
    ///
    /// # Errors
    /// When the runs can't be listed. No store yet is no runs.
    fn runs(&self) -> Result<Vec<RunId>, ApplyError>;

    /// The run's stored plan up to and including its first newline, at most
    /// [`HEADER_LIMIT`] bytes: its header. `None` when the run has no plan: it never
    /// started.
    ///
    /// # Errors
    /// When it can't be read, or the header is longer than the limit.
    fn header(&self, run: &RunId) -> Result<Option<Vec<u8>>, ApplyError>;

    /// The run's whole stored plan, `None` when it has none.
    ///
    /// # Errors
    /// When it can't be read.
    fn plan(&self, run: &RunId) -> Result<Option<Vec<u8>>, ApplyError>;

    /// The run's journal as stored, torn tail and all, at most [`JOURNAL_LIMIT`] bytes.
    /// Empty when it has none.
    ///
    /// # Errors
    /// When it can't be read or is longer than the limit.
    fn journal(&self, run: &RunId) -> Result<Vec<u8>, ApplyError>;

    /// Creates a run, under the lock: its directory with a new ID (mode 0700, created
    /// exclusively, the counter bumped while the ID exists), the stored `plan` (mode 0600,
    /// written to a temporary name, synced and renamed), then its journal holding `first`
    /// (mode 0600), each synced with its directory before the next step and before this
    /// returns.
    ///
    /// # Errors
    /// When there is less free space than `plan` needs, or any step fails. A run whose
    /// plan was never renamed into place is a run that never started.
    fn create(&self, lock: &Locked, plan: &[u8], first: &[u8]) -> Result<RunId, StoreRefusal>;

    /// Appends `line` to `run`'s journal at byte `at`, under the lock, and returns the
    /// journal's new length. Bytes past `at` are a torn or untrusted tail: the journal is
    /// first truncated to `at` and synced. The line is then written and synced; a write or
    /// sync that fails truncates back to `at`.
    ///
    /// # Errors
    /// When the journal is shorter than `at`, or the line isn't durable.
    fn append(&self, lock: &Locked, run: &RunId, at: u64, line: &[u8]) -> Result<u64, ApplyError>;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Catches run IDs that don't sort in the order runs were created, as a wrong civil
    /// date across a month, year or leap day would, which would list `history` out of
    /// order and reconcile an older run after a newer one.
    #[test]
    fn run_ids_sort_by_when_they_were_created() {
        let times = [
            0,
            59,
            86_399,
            951_782_399, // 2000-02-28T23:59:59Z
            951_782_400, // 2000-02-29, a leap day
            951_868_800, // 2000-03-01
            1_798_761_599,
            1_798_761_600, // 2027-01-01
        ];
        let ids: Vec<RunId> = times
            .iter()
            .map(|secs| RunId::at(UNIX_EPOCH + Duration::from_secs(*secs), 0))
            .collect();
        assert!(ids.is_sorted(), "ids sort by time: {ids:?}");
        assert_eq!(ids[4].as_str(), "20000229T000000Z-0000");
        assert_eq!(ids[7].as_str(), "20270101T000000Z-0000");
    }

    /// Catches a run directory kipple created that `runs` then skips as not a run, or a
    /// stray directory read as one.
    #[test]
    fn a_run_id_parses_only_as_written() {
        let id = RunId::at(UNIX_EPOCH + Duration::from_secs(1_793_370_301), 42);
        assert_eq!(RunId::parse(id.as_str()), Some(id));
        for stray in [
            "",
            ".lock",
            "20261030T142501Z-00000",
            "20261030-142501Z-0000",
            "2026103xT142501Z-0000",
        ] {
            assert_eq!(RunId::parse(stray), None, "{stray}");
        }
    }
}
