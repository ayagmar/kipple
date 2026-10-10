//! The bounded, cancellation-aware event stream (`docs/05-architecture.md` §2).

use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::{Receiver, SendTimeoutError, Sender, TrySendError};

use crate::cancel::CancelToken;
use crate::engine::{FindingId, Omission, ScanOutcome};
use crate::integration::IntegrationId;
use crate::root::SymbolicRoot;
use crate::size::SpaceEstimate;

/// How long a producer waits on a full channel before it checks for cancellation again.
/// It bounds how late a cancel is acknowledged while the consumer is stalled.
const SEND_POLL: Duration = Duration::from_millis(10);

/// Something that happened during a scan, in the order it happened.
#[derive(Debug, Clone)]
pub enum ScanEvent {
    /// The scan began.
    ScanStarted {
        /// How many integrations will run.
        integrations: usize,
    },
    /// An integration's root was found and will be read.
    RootOpened {
        /// The integration that reads it.
        integration: IntegrationId,
        /// Which root.
        root: SymbolicRoot,
        /// Where it is.
        path: PathBuf,
    },
    /// An integration found something.
    FindingAdded {
        /// Its ID within this scan.
        id: FindingId,
        /// The integration that found it.
        integration: IntegrationId,
        /// Where it is.
        path: PathBuf,
    },
    /// A finding was sized.
    FindingUpdated {
        /// Its ID within this scan.
        id: FindingId,
        /// What removing it would free.
        size: SpaceEstimate,
    },
    /// An integration finished, with what it could not cover.
    IntegrationCompleted {
        /// The integration.
        integration: IntegrationId,
        /// What it left out, and why.
        omissions: Vec<Omission>,
    },
    /// The scan finished. Sent while a consumer is still listening.
    ScanCompleted {
        /// Whether it ran to the end.
        outcome: ScanOutcome,
    },
}

/// An event with its position in the stream. Positions start at 0 and follow the order
/// events were emitted in.
#[derive(Debug, Clone)]
pub struct Sequenced {
    /// The position.
    pub seq: u64,
    /// The event.
    pub event: ScanEvent,
}

/// Why the engine could not deliver an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stopped {
    /// The scan was cancelled.
    Cancelled,
    /// The consumer dropped its receiver.
    ConsumerGone,
}

/// The engine's end of the stream. The scan borrows it; once the caller drops it too,
/// the receiver ends.
#[derive(Debug)]
pub struct EventTx(Sender<ScanEvent>);

/// The frontend's end of the stream. Dropping it before the stream ended cancels the
/// scan, even while the engine is busy and sends nothing.
#[derive(Debug)]
pub struct EventRx {
    receiver: Receiver<ScanEvent>,
    next_seq: u64,
    cancel: CancelToken,
    ended: bool,
}

/// A stream that holds at most `capacity` undelivered events, for a scan cancelled by
/// `cancel`. A full stream makes the engine wait: findings and outcomes are never dropped.
#[must_use]
pub fn event_channel(capacity: usize, cancel: &CancelToken) -> (EventTx, EventRx) {
    let (sender, receiver) = crossbeam_channel::bounded(capacity);
    (
        EventTx(sender),
        EventRx {
            receiver,
            next_seq: 0,
            cancel: cancel.clone(),
            ended: false,
        },
    )
}

impl EventTx {
    /// Delivers `event`, waiting while the stream is full. Gives up when the scan is
    /// cancelled, and cancels it when the consumer has gone.
    pub(crate) fn send(&self, event: ScanEvent, cancel: &CancelToken) -> Result<(), Stopped> {
        let mut event = event;
        loop {
            if cancel.is_cancelled() {
                return Err(Stopped::Cancelled);
            }
            match self.0.send_timeout(event, SEND_POLL) {
                Ok(()) => return Ok(()),
                Err(SendTimeoutError::Timeout(unsent)) => event = unsent,
                Err(SendTimeoutError::Disconnected(_)) => {
                    cancel.cancel();
                    return Err(Stopped::ConsumerGone);
                }
            }
        }
    }

    /// Delivers a closing record: an integration's or the scan's outcome. While the scan
    /// runs it waits like [`send`](Self::send). Once cancelled it is sent only if there is
    /// room right now, since nobody may be reading anymore.
    pub(crate) fn deliver(&self, event: ScanEvent, cancel: &CancelToken) {
        if !cancel.is_cancelled() && self.send(event.clone(), cancel).is_ok() {
            return;
        }
        match self.0.try_send(event) {
            Ok(()) | Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {}
        }
    }
}

impl Iterator for EventRx {
    type Item = Sequenced;

    /// The next event, waiting for one. `None` once the engine has finished, every
    /// [`EventTx`] is dropped and every event was received.
    fn next(&mut self) -> Option<Sequenced> {
        let Ok(event) = self.receiver.recv() else {
            self.ended = true;
            return None;
        };
        let seq = self.next_seq;
        self.next_seq += 1;
        Some(Sequenced { seq, event })
    }
}

impl Drop for EventRx {
    fn drop(&mut self) {
        if !self.ended {
            self.cancel.cancel();
        }
    }
}
