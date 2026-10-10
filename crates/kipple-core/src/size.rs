//! Byte counts and space estimates (`docs/04-safety-model.md` §8).

/// The sum of file lengths: what a listing shows, not what the disk holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Apparent(pub u64);

/// Storage actually allocated on disk. Sparse files hold less than their length.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Allocated(pub u64);

/// How much of an item was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completeness {
    /// Every entry below the item was read.
    Complete,
    /// Some entries could not be read, so the estimate is a lower bound.
    Incomplete {
        /// Directories or entries that could not be read.
        unreadable: u64,
    },
    /// The measurement stopped early because the scan was cancelled.
    Cancelled,
}

/// What removing an item would free, as far as kipple can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceEstimate {
    /// Total file length. Where the platform reports link counts, a hard-linked file
    /// counts once.
    pub apparent: Apparent,
    /// Allocated storage, where the platform reports it. A hard-linked file counts once.
    pub allocated: Option<Allocated>,
    /// Allocated storage that only this item holds: a file with a hard link outside the
    /// item stays on disk after the item is removed, so it is left out.
    pub unique_reclaim: Option<Allocated>,
    /// Whether every entry was measured.
    pub completeness: Completeness,
}
