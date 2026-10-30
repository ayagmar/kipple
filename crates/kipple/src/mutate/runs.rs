//! `kipple history` and `kipple restore` (J4, spec M3b pinned 10, 12): the runs kept
//! under kipple's data directory, and moving a run's trashed items back.

use std::io;
use std::process::ExitCode;

use clap::Args;
use kipple_core::format::HistoryOut;
use kipple_core::{
    Depth, ItemId, Platform, RestoreAsk, RunId, RunRead, RunState, RunStore as _, read_run,
    run_restore,
};
use kipple_platform::{OsNames, PLATFORM};

use super::output::{Events, write_items};
use super::{BLOCKED, Format, Machine, with_session};
use crate::human::{outcome, receipt as words};
use crate::theme::Role;
use crate::{Host, Io, USAGE, fail, say};

/// `kipple history` options.
#[derive(Debug, Args)]
pub struct HistoryArgs {
    /// Print one `kipple.history/v1` envelope.
    #[arg(long)]
    pub json: bool,
}

/// `kipple restore` options.
#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// The run, as `kipple history` lists it.
    #[arg(value_name = "RUN")]
    pub run: String,
    /// The items to move back, by number. Without any, every item the run moved to the
    /// trash.
    #[arg(value_name = "ITEM")]
    pub items: Vec<u32>,
}

/// `kipple history`: every run kept, oldest first, each item with what happened to it.
/// A run another kipple is applying now shows as running. Read-only.
pub(crate) fn history(host: &Host<'_>, args: &HistoryArgs, io: &mut Io<'_>) -> ExitCode {
    let settings = match host.settings(&[], io.stderr) {
        Ok(settings) => settings,
        Err(code) => return code,
    };
    let machine = Machine::new(host, &settings, None);
    let held = match machine.store.lock_state() {
        Ok(held) => held,
        Err(why) => return fail(io.stderr, &outcome::store_refusal(&why)),
    };
    let runs = match kipple_core::history(&machine.store, held, &OsNames) {
        Ok(runs) => runs,
        Err(error) => return fail(io.stderr, &outcome::error_text(&error)),
    };
    for run in runs.iter().filter(|run| run.state == RunState::Unreadable) {
        let why = match read_run(&machine.store, &run.id, Depth::Headers, &OsNames) {
            RunRead::Unreadable(_, why) => outcome::unreadable(&why),
            RunRead::Read(_) | RunRead::NeverStarted(_) => outcome::run_state(run.state),
        };
        say(io.stderr, &words::unreadable_run(run.id.as_str(), why));
    }
    let written = if args.json {
        crate::write_json(io.stdout, &HistoryOut::of(&runs, &OsNames))
    } else {
        let ink = host.ink(io.stderr);
        let mut write = || -> io::Result<()> {
            if runs.is_empty() {
                writeln!(io.stdout, "{}", words::NO_RUNS)?;
            }
            for run in &runs {
                let line = words::run_line(run.id.as_str(), run.state, run.receipt.items.len());
                writeln!(io.stdout, "{}", ink.paint(Role::Title, &line))?;
                write_items(io.stdout, &run.receipt, &ink)?;
            }
            io.stdout.flush()
        };
        write()
    };
    crate::exit_code(written, io.stderr)
}

/// `kipple restore RUN [ITEM]...`: under the lock, after reconciling, moves the items of
/// `RUN` it moved to the trash back where they were, recording each restore in the run.
pub(crate) fn restore(host: &Host<'_>, args: &RestoreArgs, io: &mut Io<'_>) -> ExitCode {
    if PLATFORM == Platform::Windows {
        say(io.stderr, words::NO_TRASH_HERE);
        return ExitCode::from(BLOCKED);
    }
    let Some(id) = RunId::parse(&args.run) else {
        say(io.stderr, &words::not_a_run(&args.run));
        return ExitCode::from(USAGE);
    };
    let settings = match host.settings(&[], io.stderr) {
        Ok(settings) => settings,
        Err(code) => return code,
    };
    let engine = match host.engine() {
        Ok(engine) => engine,
        Err(message) => return fail(io.stderr, &message),
    };
    let machine = Machine::new(host, &settings, None);
    let ask = RestoreAsk {
        run: id,
        items: args.items.iter().copied().map(ItemId).collect(),
    };
    let ink = host.ink(io.stderr);
    let mut events = Events::new(Format::Human, 0, io.cancel.clone(), ink.clone());
    let finished = with_session(&engine, &settings, &machine, |session| {
        let mut sink = events.sink(&mut *io.stdout);
        run_restore(&ask, session, &mut sink, &io.cancel)
    });
    events.say_notices(io.stderr);
    let stored = match read_run(&machine.store, &ask.run, Depth::Headers, &OsNames) {
        RunRead::Read(stored) => Some(stored),
        RunRead::NeverStarted(_) | RunRead::Unreadable(..) => None,
    };
    let mut write = || -> io::Result<()> {
        for item in &finished.items {
            let path = stored
                .as_ref()
                .and_then(|stored| stored.plan().item(item.item))
                .map(|item| item.path().as_path());
            let what = outcome::restoring(&item.restoring);
            let line = words::restore_line(item.item.0, path, &what);
            writeln!(io.stdout, "{line}")?;
        }
        if let kipple_core::Ending::Refused(why) = &finished.ending {
            say(io.stderr, &outcome::store_refusal(why));
        } else {
            let role = if finished.ending.code() == 0 {
                Role::Text
            } else {
                Role::Warning
            };
            writeln!(
                io.stdout,
                "{}",
                ink.paint(role, &words::ending_line(&finished.ending))
            )?;
        }
        io.stdout.flush()
    };
    match crate::exit_code(write(), io.stderr) {
        code if code == ExitCode::SUCCESS => ExitCode::from(finished.ending.code()),
        code => code,
    }
}
