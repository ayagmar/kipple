//! Ports M3b's store and git cards build, until they do. Each refuses or answers unknown,
//! so nothing is judged safe and nothing is stored on their account.

use std::path::Path;

use kipple_core::{
    Anchor, ApplyError, CancelToken, ErrorCode, FrozenTree, GitGap, LockState, Locked, NodeId,
    RunId, RunStore, StoreRefusal, TrackedQuery,
};

/// The git query, unbuilt: every item inside a repository is held as unknown.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoGit;

impl TrackedQuery for NoGit {
    fn count(&self, _: &Path, _: &Path, _: &CancelToken) -> Result<u64, GitGap> {
        Err(GitGap::Unsupported)
    }

    fn entries(
        &self,
        _: &Path,
        _: &Anchor,
        _: &FrozenTree,
        _: &CancelToken,
    ) -> Result<Vec<NodeId>, GitGap> {
        Err(GitGap::Unsupported)
    }
}

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
