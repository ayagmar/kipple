//! The port M3b's store card builds, until it does. It refuses, so nothing is stored on
//! its account.

use kipple_core::{
    ApplyError, CancelToken, ErrorCode, LockState, Locked, RunId, RunStore, StoreRefusal,
};

/// The run store, unbuilt: it refuses every mutating command and holds no runs.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRunStore;

impl RunStore for NoRunStore {
    fn lock(&self, _: &CancelToken) -> Result<Locked, StoreRefusal> {
        Err(StoreRefusal::Unsupported)
    }

    fn lock_state(&self) -> Result<LockState, StoreRefusal> {
        Err(StoreRefusal::Unsupported)
    }

    fn runs(&self) -> Result<Vec<RunId>, ApplyError> {
        Ok(Vec::new())
    }

    fn header(&self, _: &RunId) -> Result<Option<Vec<u8>>, ApplyError> {
        Ok(None)
    }

    fn plan(&self, _: &RunId) -> Result<Option<Vec<u8>>, ApplyError> {
        Ok(None)
    }

    fn journal(&self, _: &RunId) -> Result<Vec<u8>, ApplyError> {
        Ok(Vec::new())
    }

    fn create(&self, _: &Locked, _: &[u8], _: &[u8]) -> Result<RunId, StoreRefusal> {
        Err(StoreRefusal::Unsupported)
    }

    fn append(&self, _: &Locked, _: &RunId, _: u64, _: &[u8]) -> Result<u64, ApplyError> {
        Err(ApplyError::new(ErrorCode::Unsupported))
    }
}
