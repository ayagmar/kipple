//! `kipple scan` (`docs/03-functional-spec.md` §3 and §4): human output, one `--json`
//! envelope, or the `--events jsonl` stream. Read-only.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;

use kipple_core::{
    CancelToken, Engine, IntegrationId, Lens, ScanEvent, ScanReport, ScanRequest, Sequenced,
    event_channel,
};
use kipple_platform::{Environment, resolve_roots};

use crate::human::{bytes, escaped, size};
use crate::json::{
    EVENT, EventBody, EventOut, FindingOut, LensOut, OmissionOut, PathOut, ScanOut, SizeOut,
};

/// How many undelivered events the stream holds before the scan waits.
const STREAM_CAPACITY: usize = 256;

/// Runs a scan of `roots` (or the default project roots) over `engine`, passing each
/// event to `on_event` as it arrives. When `on_event` fails, the scan is cancelled and
/// its error returned with the partial report.
pub(crate) fn run(
    engine: &Engine,
    env: &Environment,
    roots: &[PathBuf],
    lens: Option<Lens>,
    on_event: &mut dyn FnMut(Sequenced) -> io::Result<()>,
) -> (ScanReport, io::Result<()>) {
    let request = ScanRequest {
        roots: resolve_roots(env, roots),
        lens,
    };
    let cancel = CancelToken::new();
    let (events, mut received) = event_channel(STREAM_CAPACITY, &cancel);
    thread::scope(|scope| {
        let scan = scope.spawn(|| {
            let events = events;
            engine.scan(&request, &events, &cancel)
        });
        let mut delivered = Ok(());
        for event in &mut received {
            delivered = on_event(event);
            if delivered.is_err() {
                break;
            }
        }
        // Dropping the receiver cancels a scan that is still running.
        drop(received);
        let report = scan.join().unwrap_or_else(|_| ScanReport {
            outcome: kipple_core::ScanOutcome::Cancelled,
            integrations: Vec::new(),
            findings: Vec::new(),
        });
        (report, delivered)
    })
}

/// Writes each event as one JSONL record.
pub(crate) fn write_event(
    out: &mut dyn Write,
    sequenced: &Sequenced,
    lenses: &BTreeMap<IntegrationId, Lens>,
) -> io::Result<()> {
    let lens = |id| lenses.get(&id).copied().unwrap_or(Lens::System);
    let event = match &sequenced.event {
        ScanEvent::ScanStarted { integrations } => EventBody::ScanStarted {
            integrations: *integrations,
        },
        ScanEvent::RootOpened {
            integration,
            root,
            path,
        } => EventBody::RootOpened {
            integration: integration.0.to_owned(),
            root: root.name(),
            path: PathOut::new(path),
        },
        ScanEvent::FindingAdded { finding } => EventBody::FindingAdded {
            id: finding.id.0,
            finding: FindingOut::new(finding, lens(finding.integration)),
        },
        ScanEvent::FindingUpdated { id, size } => EventBody::FindingUpdated {
            id: id.0,
            size: SizeOut::from(size),
        },
        ScanEvent::IntegrationCompleted {
            integration,
            omissions,
        } => EventBody::IntegrationCompleted {
            integration: integration.0.to_owned(),
            omissions: omissions.iter().map(OmissionOut::from).collect(),
        },
        ScanEvent::ScanCompleted { outcome } => EventBody::ScanCompleted {
            outcome: (*outcome).into(),
        },
    };
    write_record(out, sequenced.seq, event)
}

/// Writes one JSONL record and flushes it, so a reader sees it at once.
pub(crate) fn write_record(out: &mut dyn Write, seq: u64, event: EventBody) -> io::Result<()> {
    let record = EventOut {
        schema: EVENT,
        seq,
        event,
    };
    serde_json::to_writer(&mut *out, &record)?;
    writeln!(out)?;
    out.flush()
}

/// Writes the report for people, grouped by lens.
pub(crate) fn write_human(out: &mut dyn Write, report: &ScanOut) -> io::Result<()> {
    writeln!(out, "kipple {} scan (read-only)", report.kipple)?;
    for lens in [LensOut::Agents, LensOut::Dev, LensOut::System] {
        let found: Vec<&FindingOut> = report.findings.iter().filter(|f| f.lens == lens).collect();
        writeln!(out, "\n{}", lens_title(lens))?;
        if found.is_empty() {
            writeln!(out, "  nothing found")?;
        }
        for finding in found {
            writeln!(
                out,
                "  {:>12}  {:<26}  {}",
                size(finding.size.as_ref()),
                finding.rule,
                escaped(&finding.path.display)
            )?;
        }
    }
    write_advice(out, report)?;
    write_omissions(out, report)?;
    let total: u64 = report
        .findings
        .iter()
        .filter_map(|f| f.size.as_ref())
        .map(|s| s.allocated.unwrap_or(s.apparent))
        .sum();
    writeln!(
        out,
        "\n{} findings, {} in all. Nothing was changed. `kipple explain PATH` says why each one is here.",
        report.findings.len(),
        bytes(total)
    )?;
    out.flush()
}

const fn lens_title(lens: LensOut) -> &'static str {
    match lens {
        LensOut::Agents => "Agents",
        LensOut::Dev => "Dev",
        LensOut::System => "System",
    }
}

fn write_advice(out: &mut dyn Write, report: &ScanOut) -> io::Result<()> {
    let advised = report
        .integrations
        .iter()
        .flat_map(|i| i.advice.iter().map(move |a| (&i.id, a)));
    let mut first = true;
    for (integration, advice) in advised {
        if first {
            writeln!(out, "\nAdvice (kipple never runs these)")?;
            first = false;
        }
        writeln!(out, "  {integration}: {}", escaped(&advice.title))?;
        writeln!(out, "    {}", escaped(&advice.text))?;
        for snippet in &advice.snippets {
            for line in snippet.lines() {
                writeln!(out, "      {}", escaped(line))?;
            }
        }
    }
    Ok(())
}

fn write_omissions(out: &mut dyn Write, report: &ScanOut) -> io::Result<()> {
    let omitted = report
        .integrations
        .iter()
        .flat_map(|i| i.omissions.iter().map(move |o| (&i.id, o)));
    let mut first = true;
    for (integration, omission) in omitted {
        if first {
            writeln!(out, "\nNot covered, so this report is partial")?;
            first = false;
        }
        writeln!(out, "  {integration}: {}", escaped(&omission.message))?;
    }
    Ok(())
}
