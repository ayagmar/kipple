//! What a mutating command writes: its events as they happen (`--events jsonl`), and its
//! receipt when it ends, for people or as `kipple.receipt/v1`.

use std::io::{self, Write};
use std::process::ExitCode;

use kipple_core::format::{ApplyEventOut, ReceiptOut};
use kipple_core::{
    ApplyEvent, CancelToken, DroppedItem, Ending, EventSink, Finding, Finished, ItemEvent, Policy,
    Receipt, Reconciled, RefusedItem, Status,
};
use kipple_platform::OsNames;

use super::Format;
use crate::human::{self, outcome, receipt as words};
use crate::theme::{Ink, Role};
use crate::{CANCELLED, say};

/// The exit code of the crash a `test-roots` build makes at [`crate::crash_point`]: the
/// process ends with the item's intent durable and nothing more recorded.
const CRASHED: i32 = 99;

/// A line for people, and the role it is painted in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Line {
    pub(super) role: Role,
    pub(super) text: String,
}

impl Line {
    pub(super) const fn new(role: Role, text: String) -> Self {
        Self { role, text }
    }
}

/// The Review findings a command line clean leaves to the TUI, and the policy that put
/// them there.
#[derive(Debug, Clone, Copy)]
pub(super) struct Shown<'scan> {
    pub(super) review: &'scan [&'scan Finding],
    pub(super) policy: &'scan Policy,
    /// Whether the person was already told what was left out.
    pub(super) told: Told,
}

/// Whether the prompt already said what was left out of the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Told {
    /// The person saw it before answering.
    InPrompt,
    /// Nobody was asked.
    NotYet,
}

/// The lines saying what wasn't planned: the selected findings planning refused, a plan
/// file's items left out, and the Review findings left to the TUI.
pub(super) fn left_out(
    refused: &[RefusedItem],
    dropped: &[DroppedItem],
    shown: &Shown<'_>,
) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut section = |heading: &str, items: Vec<String>| {
        if items.is_empty() {
            return;
        }
        lines.push(Line::new(Role::Title, heading.to_owned()));
        lines.extend(items.into_iter().map(|text| Line::new(Role::Text, text)));
    };
    section(
        words::NOT_PLANNED,
        refused
            .iter()
            .map(|item| words::left_line(&item.path, &outcome::skip(&item.skip)))
            .collect(),
    );
    section(
        words::DROPPED,
        dropped
            .iter()
            .map(|item| words::left_line(&item.path, &outcome::dropped(&item.why)))
            .collect(),
    );
    section(
        words::NEEDS_TUI,
        shown
            .review
            .iter()
            .map(|finding| {
                let why = finding
                    .verdict(shown.policy)
                    .map(|verdict| human::reasons(&verdict))
                    .unwrap_or_default();
                words::left_line(&finding.path, &why)
            })
            .collect(),
    );
    lines
}

/// What a command's events go to while it runs, and how it finishes.
#[derive(Debug)]
pub(super) struct Events {
    format: Format,
    /// How its lines for people are painted.
    ink: Ink,
    /// The next event's `seq`, counted on from the scan's.
    seq: u64,
    /// Cancelled when the event stream can't be written.
    cancel: CancelToken,
    /// What to say on stderr once it finishes: earlier runs reconciled or unreadable.
    notices: Vec<String>,
    /// How writing the stream went.
    written: io::Result<()>,
    /// The item to crash at, in a `test-roots` build ([`crate::crash_point`]).
    crash_at: Option<u32>,
}

impl Events {
    pub(super) const fn new(format: Format, seq: u64, cancel: CancelToken, ink: Ink) -> Self {
        Self {
            format,
            ink,
            seq,
            cancel,
            notices: Vec::new(),
            written: Ok(()),
            crash_at: None,
        }
    }

    /// Ends the process as a crash would once item `crash_at`'s intent is durable.
    pub(super) fn crashing_at(self, crash_at: Option<u32>) -> Self {
        Self { crash_at, ..self }
    }

    /// Where the command reports its events, streaming them to `stdout` when asked to.
    pub(super) fn sink<'sink>(&'sink mut self, stdout: &'sink mut dyn Write) -> Sink<'sink> {
        Sink {
            events: self,
            stdout,
        }
    }

    /// Says on `stderr` what happened to earlier runs.
    pub(super) fn say_notices(&self, stderr: &mut dyn Write) {
        for notice in &self.notices {
            say(stderr, notice);
        }
    }

    /// Writes what `finished` did: the receipt, and on stderr what happened to earlier
    /// runs. Exits as `finished` ended, or 130 when stdout closed.
    pub(super) fn finish(
        self,
        finished: &Finished,
        shown: &Shown<'_>,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> ExitCode {
        self.say_notices(stderr);
        if let Ending::Refused(why) = &finished.ending {
            say(stderr, &outcome::store_refusal(why));
        }
        let written = self.written.and_then(|()| match self.format {
            Format::Human => write_human(stdout, finished, shown, &self.ink),
            Format::Json => crate::write_json(stdout, &ReceiptOut::of(finished, &OsNames)),
            Format::Events => write_line(stdout, &ReceiptOut::of(finished, &OsNames)),
        });
        match written {
            Ok(()) => ExitCode::from(finished.ending.code()),
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::from(CANCELLED),
            Err(error) => crate::exit_code(Err(error), stderr),
        }
    }
}

/// Reports each event of a running command.
pub(super) struct Sink<'sink> {
    events: &'sink mut Events,
    stdout: &'sink mut dyn Write,
}

impl EventSink for Sink<'_> {
    fn event(&mut self, event: ApplyEvent) {
        let events = &mut *self.events;
        if let ApplyEvent::Item {
            event: ItemEvent::Started { item },
            ..
        } = &event
            && events.crash_at == Some(item.0)
        {
            std::process::exit(CRASHED);
        }
        if let ApplyEvent::Unreadable { run, why } = &event {
            events.notices.push(words::unreadable_run(
                run.as_str(),
                outcome::unreadable(why),
            ));
        }
        if events.format != Format::Events {
            if let ApplyEvent::Reconciled { run, reconciled } = &event {
                events
                    .notices
                    .push(reconciled_notice(run.as_str(), reconciled));
            }
            return;
        }
        if events.written.is_err() {
            return;
        }
        let line = ApplyEventOut::of(&event, events.seq, &OsNames);
        events.seq += 1;
        events.written = write_line(self.stdout, &line);
        if events.written.is_err() {
            // A closed stream cancels the command, as it cancels a scan.
            events.cancel.cancel();
        }
    }
}

fn reconciled_notice(run: &str, reconciled: &Reconciled) -> String {
    match reconciled {
        Reconciled::Item { item, outcome } => words::reconciled(run, item.0, outcome),
        Reconciled::Restore { item, result } => {
            words::reconciled_restore(run, item.0, &outcome::restore_result(result))
        }
    }
}

/// Writes `value` as one JSON line and flushes it.
fn write_line(out: &mut dyn Write, value: &impl serde::Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *out, value)?;
    writeln!(out)?;
    out.flush()
}

/// Writes the receipt for people: each item, the totals, what wasn't planned and how it
/// ended.
fn write_human(
    out: &mut dyn Write,
    finished: &Finished,
    shown: &Shown<'_>,
    ink: &Ink,
) -> io::Result<()> {
    if let Some(run) = &finished.run {
        writeln!(
            out,
            "{}",
            ink.paint(Role::Title, &words::run_heading(run.as_str()))
        )?;
        write_items(out, &finished.receipt, ink)?;
        let totals = words::totals(&finished.receipt.totals());
        if !totals.is_empty() {
            writeln!(out, "{totals}")?;
        }
    }
    if shown.told == Told::NotYet {
        for line in left_out(&finished.refused, &finished.dropped, shown) {
            writeln!(out, "{}", ink.paint(line.role, &line.text))?;
        }
    }
    if !matches!(finished.ending, Ending::Refused(_)) {
        let role = match finished.ending.code() {
            0 => Role::Text,
            _ => Role::Warning,
        };
        writeln!(
            out,
            "{}",
            ink.paint(role, &words::ending_line(&finished.ending))
        )?;
    }
    out.flush()
}

/// Writes each item of `receipt`: its number, method, path and outcome, and its last
/// restore.
pub(super) fn write_items(out: &mut dyn Write, receipt: &Receipt, ink: &Ink) -> io::Result<()> {
    for item in &receipt.items {
        let role = role_of(item.outcome.status());
        let line = words::item_line(item.item.0, &item.outcome, item.method, &item.path);
        writeln!(out, "{}", ink.paint(role, &line))?;
        if let Some(result) = &item.restored {
            let restored = words::restored_line(&outcome::restore_result(result));
            writeln!(out, "{}", ink.paint(Role::Muted, &restored))?;
        }
    }
    Ok(())
}

/// How an item's line is painted: what went wrong stands out.
const fn role_of(status: Status) -> Role {
    match status {
        Status::Done => Role::Text,
        Status::Skipped | Status::Cancelled => Role::Muted,
        Status::Partial | Status::Indeterminate => Role::Warning,
        Status::Failed => Role::Error,
    }
}
