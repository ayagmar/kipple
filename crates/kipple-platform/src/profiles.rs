//! The profile store (D-068): `<data>/profiles/<name>.json`.
//!
//! This build has none: every call refuses with [`ProfileStoreError::Unsupported`], so no
//! profile is read, written or removed (spec M3c pinned 11 replaces it).

use std::path::PathBuf;

use kipple_core::profile::{ProfileName, ProfileStore, ProfileStoreError};

/// The platform's [`ProfileStore`]: files under kipple's data directory.
#[derive(Debug, Clone)]
pub struct FileProfileStore {
    /// kipple's data directory, `None` when it didn't resolve.
    data: Option<PathBuf>,
}

impl FileProfileStore {
    /// The store under `data`, kipple's data directory, `None` when it isn't known.
    #[must_use]
    pub const fn new(data: Option<PathBuf>) -> Self {
        Self { data }
    }

    const fn refuse(&self) -> ProfileStoreError {
        match self.data {
            None => ProfileStoreError::NoDataDir,
            Some(_) => ProfileStoreError::Unsupported,
        }
    }
}

impl ProfileStore for FileProfileStore {
    fn read(&self, _: &ProfileName) -> Result<Option<Vec<u8>>, ProfileStoreError> {
        Err(self.refuse())
    }

    fn create_new(&self, _: &ProfileName, _: &[u8]) -> Result<(), ProfileStoreError> {
        Err(self.refuse())
    }

    fn replace(&self, _: &ProfileName, _: &[u8]) -> Result<(), ProfileStoreError> {
        Err(self.refuse())
    }

    fn remove(&self, _: &ProfileName) -> Result<(), ProfileStoreError> {
        Err(self.refuse())
    }

    fn list(&self) -> Result<Vec<ProfileName>, ProfileStoreError> {
        Err(self.refuse())
    }
}
