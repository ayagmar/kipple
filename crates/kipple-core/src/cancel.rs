//! Cooperative cancellation shared by the engine, its workers and the frontend.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set once by whoever wants the work to stop. Every clone sees the same flag.
///
/// Workers check it before scheduling new work, so a cancel is acknowledged at
/// the next directory or item boundary. Work already done stays done.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token that is not cancelled yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks every holder of this token to stop.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether [`cancel`](Self::cancel) has been called on any clone.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
