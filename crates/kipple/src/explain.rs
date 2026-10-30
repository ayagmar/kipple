//! `kipple explain RULE|PATH` (`docs/03-functional-spec.md` §3): what a rule selects and
//! why, or what kipple found at a path and the evidence for it. A path runs a scan; it
//! changes nothing.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kipple_core::{Operation, Pack, Policy, Rule, ScanOutcome, ScanReport, Selector};
use kipple_platform::PLATFORM;

use crate::human::{self, bytes, escaped, size};
use crate::json::{EvidenceOut, FindingOut};
use crate::setup::{Builtin, BuiltinRule};
use crate::theme::{Ink, Role};

/// The rule called `id`, with its pack.
pub(crate) fn find_rule<'b>(builtin: &'b Builtin, id: &str) -> Option<BuiltinRule<'b>> {
    builtin.rules().find(|found| found.rule.id.0 == id)
}

/// Writes what `rule` selects and why.
pub(crate) fn write_rule(
    out: &mut dyn Write,
    ink: &Ink,
    builtin: &Builtin,
    pack: &Pack,
    rule: &Rule,
) -> io::Result<()> {
    let lens_name = builtin
        .lenses()
        .get(&rule.integration)
        .map_or("?", |&l| l.name());
    let platforms: Vec<&str> = rule.platforms.iter().map(|p| p.name()).collect();
    let here = if rule.platforms.contains(&PLATFORM) {
        "runs here"
    } else {
        "not on this OS"
    };
    let rows = [
        (
            "Integration",
            format!(
                "{} ({lens_name} lens), from pack {} {}",
                rule.integration, pack.id, pack.version
            ),
        ),
        ("Category", rule.category.name().to_owned()),
        ("Platforms", format!("{} ({here})", platforms.join(", "))),
        ("Looks in", rule.root.name().to_owned()),
        ("Selects", selects(&rule.selector)),
        ("Why it's there", rule.text.why.clone()),
        ("What you lose", rule.text.loss.clone()),
        ("Operation", operation(&rule.operation)),
    ];
    let title = escaped(&format!("{}: {}", rule.id, rule.text.title));
    writeln!(out, "{}", ink.paint(Role::Title, &title))?;
    if !rule.text.description.is_empty() {
        writeln!(out, "  {}", escaped(&rule.text.description))?;
    }
    for (name, value) in rows {
        writeln!(out, "  {} {}", label(ink, name), escaped(&value))?;
    }
    for reference in &rule.text.references {
        writeln!(out, "  {} {}", label(ink, "Reference"), escaped(reference))?;
    }
    Ok(())
}

/// `name` padded to the label column, in the label role.
fn label(ink: &Ink, name: &str) -> String {
    ink.paint(Role::Label, &format!("{name:<15}"))
}

fn operation(operation: &Operation) -> String {
    match operation {
        Operation::Report => "report only: kipple shows it and never changes it".to_owned(),
        Operation::RemoveFiles(methods) => {
            let names: Vec<&str> = methods.iter().map(|m| m.name()).collect();
            format!(
                "remove its files ({}) once you select and confirm it; this version only \
                 finds and explains",
                names.join(" or ")
            )
        }
    }
}

fn selects(selector: &Selector) -> String {
    match selector {
        Selector::ProjectArtifact {
            markers,
            artifact,
            max_depth,
        } => {
            let markers: Vec<&str> = markers.iter().map(|m| m.0.as_str()).collect();
            format!(
                "a {} directory beside {}, at most {max_depth} directories down, never \
                 looking inside one",
                artifact.0,
                markers.join(" or ")
            )
        }
        Selector::DirsMatching { pattern, .. } => format!("directories at {}", pattern.0.join("/")),
        Selector::FilesMatching { pattern, .. } => format!("files at {}", pattern.0.join("/")),
        Selector::Integration(name) => format!("{name}, as its integration recognises them"),
    }
}

/// A scan's report and the policy its findings are judged by.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Judged<'report> {
    pub(crate) report: &'report ScanReport,
    pub(crate) policy: &'report Policy,
}

/// Writes what was found at `path`, or that nothing was.
pub(crate) fn write_path(
    out: &mut dyn Write,
    ink: &Ink,
    builtin: &Builtin,
    path: &Path,
    judged: Judged<'_>,
) -> io::Result<()> {
    let Judged { report, policy } = judged;
    let findings = &report.findings;
    let wanted = comparable(path);
    let at = findings.iter().find(|f| comparable(&f.path) == wanted);
    let Some(finding) = at else {
        let around = findings
            .iter()
            .find(|f| wanted.starts_with(comparable(&f.path)));
        writeln!(
            out,
            "kipple found nothing at {}.",
            escaped(&path.display().to_string())
        )?;
        if let Some(finding) = around {
            let line = format!(
                "It is inside {}, found by {}.",
                finding.path.display(),
                finding.rule
            );
            writeln!(out, "{}", escaped(&line))?;
        }
        write_gaps(out, report)?;
        return writeln!(
            out,
            "kipple only reports what its rules select: see `kipple rules`."
        );
    };
    let lens = builtin.lenses().get(&finding.integration).copied();
    let shown = FindingOut::new(finding, lens.unwrap_or(kipple_core::Lens::System), policy);
    writeln!(
        out,
        "{}",
        ink.paint(Role::Title, &escaped(&shown.path.display))
    )?;
    writeln!(
        out,
        "  {} {}",
        label(ink, "Storage"),
        size(shown.size.as_ref())
    )?;
    if let Some(measured) = &shown.size {
        let apparent = bytes(measured.apparent);
        writeln!(out, "  {} {apparent}", label(ink, "Apparent size"))?;
    }
    for evidence in &shown.evidence {
        let line = escaped(&evidence_line(evidence));
        writeln!(out, "  {} {line}", label(ink, "Evidence"))?;
    }
    if let Some(verdict) = finding.verdict(policy) {
        let reasons = human::reasons_in_full(&verdict);
        let name = human::bucket(verdict.bucket());
        let bucket = ink.paint(Role::bucket(verdict.bucket()), name);
        let level = policy.level_of(&finding.rule);
        let line = if reasons.is_empty() {
            format!("{bucket} at the {level} level")
        } else {
            format!("{bucket} at the {level} level: {reasons}")
        };
        writeln!(out, "  {} {line}", label(ink, "Bucket"))?;
    }
    for human::Fact { label: name, text } in human::facts(finding, policy) {
        writeln!(out, "  {} {text}", label(ink, name))?;
    }
    writeln!(out)?;
    match find_rule(builtin, &finding.rule.0) {
        Some(BuiltinRule { pack, rule }) => write_rule(out, ink, builtin, pack, rule),
        None => writeln!(out, "Found by {}.", finding.rule),
    }
}

/// What the scan couldn't cover, where the path may be after all.
fn write_gaps(out: &mut dyn Write, report: &ScanReport) -> io::Result<()> {
    let gaps: Vec<_> = report
        .integrations
        .iter()
        .flat_map(|i| i.omissions.iter().map(move |omission| (i.id, omission)))
        .collect();
    if report.outcome == ScanOutcome::Cancelled {
        writeln!(out, "The scan was cancelled before it finished.")?;
    }
    if gaps.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "The scan couldn't cover everything, so it may be there:"
    )?;
    for (integration, omission) in gaps {
        writeln!(out, "  {}", escaped(&format!("{integration}: {omission}")))?;
    }
    Ok(())
}

fn evidence_line(evidence: &EvidenceOut) -> String {
    match evidence {
        EvidenceOut::Marker { file } => format!("{file} beside it marks a project"),
        EvidenceOut::Signature { entry } => format!("{entry} inside it, which its tool writes"),
        EvidenceOut::NoSignature => {
            "nothing inside that its tool writes, so its layout is unknown".to_owned()
        }
        EvidenceOut::Root { root } => format!("it is the {root} root itself"),
    }
}

/// `path` with links resolved where it exists, so a configured path and the path a
/// root resolved to compare equal.
fn comparable(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_owned())
}
