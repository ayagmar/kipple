//! The workers a clean from the home screen runs on (spec M3c pinned 4, 7 and 8): one
//! plans the selection, one runs the plan the person confirmed. Each builds its own
//! [`Machine`] from the settings it is given, so no port crosses threads, runs under
//! `catch_unwind`, and is joined by the loop once it has ended. A worker that panicked
//! is [`Failed`]; its message goes to the panic hook [`quiet_workers`] installs, never
//! onto the screen.

use std::io::{self, Write as _};
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, ThreadId};

use kipple_core::{
    ApplyEvent, CancelToken, CleanRequest, Confirmed, Finished, PlanMode, Prepared, ScanReport,
    Selection, prepare_clean, run_prepared,
};
use kipple_platform::Environment;

use crate::config::Settings;
use crate::machine::{ChannelSink, Machine};
use crate::setup::Builtin;

/// A worker that panicked. What it was doing is left as it was: a plan wrote nothing,
/// and a run is reconciled by the next command that changes anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Failed;

/// What `body` returns, or [`Failed`] when it panicked.
pub(super) fn guarded<T>(body: impl FnOnce() -> T) -> Result<T, Failed> {
    panic::catch_unwind(AssertUnwindSafe(body)).map_err(|_| Failed)
}

/// What a worker needs to build its machine: the built-in rules and the machine kipple
/// runs on, with the settings read when the person asked.
pub(super) struct Ground<'env> {
    pub(super) builtin: &'env Builtin,
    pub(super) env: &'env Environment,
    pub(super) settings: Settings,
}

/// Plans `selection` of `report` for a person's review, as the review shows it: Safe and
/// Review items alike (pinned 2). Writes nothing.
///
/// # Errors
/// When the machine can't be made, worded for the person.
pub(super) fn plan(
    ground: Ground<'_>,
    report: &ScanReport,
    selection: &Selection,
    cancel: &CancelToken,
) -> Result<Prepared, String> {
    let machine = Machine::from_settings(ground.builtin, ground.env, ground.settings, None)?;
    let request = CleanRequest {
        scan: report,
        selection,
    };
    Ok(machine.with_session(|session| prepare_clean(&request, session, PlanMode::Review, cancel)))
}

/// Runs `prepared` as the person who saw it confirmed it, sending each event on
/// `events`.
///
/// # Errors
/// When the machine can't be made, worded for the person. Nothing ran.
pub(super) fn apply(
    ground: Ground<'_>,
    prepared: &Prepared,
    events: Sender<ApplyEvent>,
    cancel: &CancelToken,
) -> Result<Finished, String> {
    let machine = Machine::from_settings(ground.builtin, ground.env, ground.settings, None)?;
    let mut sink = ChannelSink(events);
    Ok(machine.with_session(|session| {
        run_prepared(prepared, Confirmed::Person, session, &mut sink, cancel)
    }))
}

/// The messages of the panics on other threads than the screen's, kept to print once
/// the terminal is restored.
pub(super) type Caught = Arc<Mutex<Vec<String>>>;

/// Replaces the panic hook `ratatui::try_init` installed, which restores the terminal
/// whichever thread panics, with one that does so only for the screen's thread `ui`. A
/// panic on any other thread, such as a worker's, keeps its message in `caught`
/// instead of writing it over the screen, which stays in raw mode and goes on.
pub(super) fn quiet_workers(ui: ThreadId, caught: Caught) {
    let restoring = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if thread::current().id() == ui {
            restoring(info);
        } else {
            caught
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(info.to_string());
        }
    }));
}

/// Writes what `caught` kept to stderr, once the terminal is restored.
pub(super) fn tell(caught: &Caught) {
    let messages = std::mem::take(&mut *caught.lock().unwrap_or_else(PoisonError::into_inner));
    let mut stderr = io::stderr().lock();
    for message in messages {
        writeln!(stderr, "kipple: {message}").ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Catches a worker's panic escaping `guarded` and panicking again at the scope's end,
    // or the panic hook writing it over the screen (ratatui's hook would restore the
    // terminal under a live screen) instead of keeping it to tell afterwards.
    #[test]
    fn a_worker_that_panics_is_failed_and_its_message_is_kept_for_later() {
        let previous = panic::take_hook();
        let caught: Caught = Arc::default();
        quiet_workers(thread::current().id(), Arc::clone(&caught));

        let answer = thread::scope(|scope| {
            scope
                .spawn(|| guarded(|| -> u8 { panic!("the worker broke") }))
                .join()
        });

        panic::set_hook(previous);
        assert_eq!(answer.ok(), Some(Err(Failed)));
        let kept = std::mem::take(&mut *caught.lock().unwrap_or_else(PoisonError::into_inner));
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(kept[0].contains("the worker broke"), "{kept:?}");
    }
}
