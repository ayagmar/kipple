//! `kipple scan` (`docs/03-functional-spec.md` §3 and §4): human output, one `--json`
//! envelope, or the `--events jsonl` stream. Read-only.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;

use kipple_core::{
    CancelToken, Engine, FindingId, IntegrationId, Lens, Policy, RuleId, ScanEvent, ScanReport,
    ScanRequest, Sequenced, event_channel,
};

use crate::config::Settings;
use crate::human::{bytes, escaped, reason, size, snippet_lines};
use crate::json::{
    BucketOut, EVENT, EventBody, EventOut, FindingOut, LensOut, OmissionOut, PathOut, ScanOut,
    SizeOut, VerdictOut,
};
use crate::theme::{Ink, Role};

/// How many undelivered events the stream holds before the scan waits.
const STREAM_CAPACITY: usize = 256;

/// Runs a scan of the roots of `settings` over `engine`, never entering a path the config
/// excludes, passing each event to `on_event` as it arrives. When `on_event` fails, the
/// scan is cancelled and its error returned with the partial report.
pub(crate) fn run(
    engine: &Engine,
    settings: &Settings,
    lens: Option<Lens>,
    on_event: &mut dyn FnMut(Sequenced) -> io::Result<()>,
) -> (ScanReport, io::Result<()>) {
    let request = ScanRequest {
        roots: settings.roots.clone(),
        lens,
        exclude: settings.policy.exclude.clone(),
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
            unchecked_system: 0,
            by_command_line: 0,
        });
        (report, delivered)
    })
}

/// What the event stream needs to write each event: each finding's verdict comes from the
/// policy, with the rule and path its `FindingAdded` record carried.
#[derive(Debug)]
pub(crate) struct Stream<'a> {
    pub(crate) lenses: &'a BTreeMap<IntegrationId, Lens>,
    pub(crate) policy: &'a Policy,
    pub(crate) found: HashMap<FindingId, (RuleId, PathBuf)>,
}

/// Writes each event as one JSONL record.
pub(crate) fn write_event(
    out: &mut dyn Write,
    sequenced: &Sequenced,
    stream: &mut Stream<'_>,
) -> io::Result<()> {
    let lenses = stream.lenses;
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
        ScanEvent::FindingAdded { finding } => {
            let place = (finding.rule.clone(), finding.path.clone());
            stream.found.insert(finding.id, place);
            EventBody::FindingAdded {
                id: finding.id.0,
                finding: Box::new(FindingOut::new(
                    finding,
                    lens(finding.integration),
                    stream.policy,
                )),
            }
        }
        ScanEvent::FindingUpdated { id, size, facts } => {
            // The engine adds every finding before it updates it.
            let (rule, path) = stream.found.get(id).ok_or_else(|| {
                io::Error::other(format!("finding {id} was updated before it was added"))
            })?;
            let verdict = VerdictOut::from(&stream.policy.verdict(rule, path, facts));
            EventBody::FindingUpdated {
                id: id.0,
                size: size.as_ref().map(SizeOut::from),
                verdict,
            }
        }
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
pub(crate) fn write_human(out: &mut dyn Write, report: &ScanOut, ink: &Ink) -> io::Result<()> {
    writeln!(out, "kipple {} scan (read-only)", report.kipple)?;
    for lens in [LensOut::Agents, LensOut::Dev, LensOut::System] {
        let found: Vec<&FindingOut> = report.findings.iter().filter(|f| f.lens == lens).collect();
        writeln!(out, "\n{}", ink.paint(Role::Title, lens_title(lens)))?;
        if found.is_empty() {
            writeln!(out, "  {}", ink.paint(Role::Muted, "nothing found"))?;
        }
        for finding in found {
            let bucket = finding.verdict.as_ref().map(|v| v.bucket);
            let word = bucket.map_or("unsized", bucket_word);
            let bucket = ink.paint(
                bucket.map_or(Role::Muted, bucket_role),
                &format!("{word:<9}"),
            );
            let reasons = finding
                .verdict
                .as_ref()
                .filter(|verdict| !verdict.reasons.is_empty())
                .map(|verdict| {
                    let words: Vec<&str> = verdict.reasons.iter().map(|&r| reason(r)).collect();
                    ink.paint(Role::Muted, &format!("  ({})", words.join(", ")))
                })
                .unwrap_or_default();
            let rule = ink.paint(Role::Muted, &format!("{:<26}", finding.rule));
            writeln!(
                out,
                "  {:>12}  {bucket}  {rule}  {}{reasons}",
                size(finding.size.as_ref()),
                escaped(&finding.path.display)
            )?;
        }
    }
    write_advice(out, report, ink)?;
    write_omissions(out, report, ink)?;
    let notices = process_notices(report);
    if !notices.is_empty() {
        writeln!(out)?;
    }
    for notice in notices {
        writeln!(out, "{notice}")?;
    }
    let total: u64 = report
        .findings
        .iter()
        .filter_map(|f| f.size.as_ref())
        .map(|s| s.allocated.unwrap_or(s.apparent))
        .sum();
    writeln!(
        out,
        "\n{} findings taking {} of storage in all. Nothing was changed. `kipple explain PATH` says why each one is here.",
        report.findings.len(),
        bytes(total)
    )?;
    out.flush()
}

/// Writes `kipple` without a terminal: each bucket's findings and size, and whether the
/// report is partial (`docs/03-functional-spec.md` §2 J1).
pub(crate) fn write_summary(out: &mut dyn Write, report: &ScanOut, ink: &Ink) -> io::Result<()> {
    writeln!(
        out,
        "kipple {} (read-only, level: {})",
        report.kipple, report.level
    )?;
    let rows = [
        ("Safe to clean", Some(BucketOut::Safe)),
        ("Review", Some(BucketOut::Review)),
        ("Protected", Some(BucketOut::Protected)),
        ("Not measured", None),
    ];
    for (title, bucket) in rows {
        let found: Vec<&FindingOut> = report
            .findings
            .iter()
            .filter(|f| f.verdict.as_ref().map(|v| v.bucket) == bucket)
            .collect();
        if bucket.is_none() && found.is_empty() {
            continue;
        }
        let total: u64 = found
            .iter()
            .filter_map(|f| f.size.as_ref())
            .map(|s| s.allocated.unwrap_or(s.apparent))
            .sum();
        let title = ink.paint(
            bucket.map_or(Role::Muted, bucket_role),
            &format!("{title:<14}"),
        );
        writeln!(
            out,
            "  {title} {:>12}  {}",
            bytes(total),
            crate::human::findings(found.len())
        )?;
    }
    let gaps: usize = report.integrations.iter().map(|i| i.omissions.len()).sum();
    if gaps > 0 {
        let partial =
            format!("Partial: {gaps} places couldn't be covered; `kipple scan` lists them.");
        writeln!(out, "{}", ink.paint(Role::Warning, &partial))?;
    }
    for notice in process_notices(report) {
        writeln!(out, "{notice}")?;
    }
    writeln!(
        out,
        "Run `kipple` in a terminal to browse them, or `kipple scan` to list every finding. Nothing was changed."
    )?;
    out.flush()
}

const fn bucket_word(bucket: BucketOut) -> &'static str {
    match bucket {
        BucketOut::Safe => "safe",
        BucketOut::Review => "review",
        BucketOut::Protected => "protected",
    }
}

/// The role a bucket is painted in, as on the home screen.
const fn bucket_role(bucket: BucketOut) -> Role {
    match bucket {
        BucketOut::Safe => Role::Safe,
        BucketOut::Review => Role::Review,
        BucketOut::Protected => Role::Protected,
    }
}

const fn lens_title(lens: LensOut) -> &'static str {
    match lens {
        LensOut::Agents => "Agents",
        LensOut::Dev => "Dev",
        LensOut::System => "System",
    }
}

fn write_advice(out: &mut dyn Write, report: &ScanOut, ink: &Ink) -> io::Result<()> {
    let advised = report
        .integrations
        .iter()
        .flat_map(|i| i.advice.iter().map(move |a| (&i.id, a)));
    let mut first = true;
    for (integration, advice) in advised {
        if first {
            let heading = ink.paint(Role::Title, "Advice (kipple never runs these)");
            writeln!(out, "\n{heading}")?;
            first = false;
        }
        writeln!(out, "  {integration}: {}", escaped(&advice.title))?;
        writeln!(out, "    {}", escaped(&advice.text))?;
        for line in advice.snippets.iter().flat_map(|s| snippet_lines(s)) {
            writeln!(out, "      {line}")?;
        }
    }
    Ok(())
}

fn write_omissions(out: &mut dyn Write, report: &ScanOut, ink: &Ink) -> io::Result<()> {
    let omitted = report
        .integrations
        .iter()
        .flat_map(|i| i.omissions.iter().map(move |o| (&i.id, o)));
    let mut first = true;
    for (integration, omission) in omitted {
        if first {
            let heading = ink.paint(Role::Warning, "Not covered, so this report is partial");
            writeln!(out, "\n{heading}")?;
            first = false;
        }
        writeln!(out, "  {integration}: {}", escaped(&omission.message))?;
    }
    Ok(())
}

/// The D-050 and D-052 notices about processes that couldn't be fully inspected.
fn process_notices(report: &ScanOut) -> Vec<String> {
    let by_command_line = report.by_command_line_processes.unwrap_or_default();
    [
        crate::human::unchecked_system(report.unchecked_system_processes),
        crate::human::by_command_line(by_command_line),
    ]
    .into_iter()
    .flatten()
    .collect()
}
