//! The commands that change the disk (`docs/03-functional-spec.md` §3, spec M3b pinned 1,
//! 2, 12, 17–19, M3c pinned 13–17, D-067): `clean`, `plan -o`, `apply FILE`, `history`
//! and `restore` over the runs they keep, and `profile`, the scopes `clean --profile`
//! runs unattended.
//!
//! Each composes the core's sessions with the platform: the run store under kipple's
//! data directory, the confined executor and trash, git, and the host's protections.
//! Planning, authorization, apply and reconciliation are the core's; this module only
//! asks the person, and writes what happened.

mod output;
mod profile;
mod prompt;
mod runs;

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use kipple_core::format::{PLAN_LIMIT, decode_plan_file, encode_plan_file};
use kipple_core::profile::{Profile, ProfileCheck};
use kipple_core::{
    ApplyFile, Bucket, CancelToken, CleanRequest, Consent, Finding, Method, PlanMode, PlanRequest,
    Policy, RefusedItem, RuleId, ScanOutcome, ScanReport, Selection, StoreRefusal, prepare_clean,
    run_apply, run_clean,
};
use kipple_platform::{OsNames, kipple_dirs, write_new};

pub use self::profile::ProfileAction;
pub(crate) use self::profile::profile;
pub use self::runs::{HistoryArgs, RestoreArgs};
pub(crate) use self::runs::{history, restore};
use crate::config::Settings;
use crate::human::{escaped, outcome, profile as profile_words, receipt as words};
use crate::json::EventBody;
use crate::machine::Machine;
use crate::theme::{Ink, Role};
use crate::{CANCELLED, EventsFormat, Host, Io, Person, USAGE, absolute, fail, say, scan};

/// The exit code when nothing ran: blocked, declined or stale (`docs/03-functional-spec.md`
/// §4).
const BLOCKED: u8 = 3;

/// `--method`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum MethodArg {
    /// Remove for good.
    Delete,
    /// Move to the trash, whole (Linux and macOS).
    Trash,
}

impl From<MethodArg> for Method {
    fn from(method: MethodArg) -> Self {
        match method {
            MethodArg::Delete => Self::Delete,
            MethodArg::Trash => Self::Trash,
        }
    }
}

/// What `clean` and `plan` select from the scan.
#[derive(Debug, Args)]
pub struct Choice {
    /// Only this rule's Safe findings. Repeat for several. Without it, every Safe finding.
    #[arg(long = "only", value_name = "RULE")]
    pub only: Vec<String>,
    /// How to remove them, over `clean.method` in the config.
    #[arg(long, value_enum, value_name = "METHOD")]
    pub method: Option<MethodArg>,
    /// Scan this project root instead of the configured ones. Repeat for several.
    #[arg(long = "root", value_name = "PATH")]
    pub roots: Vec<PathBuf>,
}

/// `kipple clean` options.
#[derive(Debug, Args)]
pub struct CleanArgs {
    /// What to clean.
    #[command(flatten)]
    pub choice: Choice,
    /// Clean what this profile authorizes: the Safe findings of its rules that are still
    /// as they were when it was saved, by the methods it saved. Without `--yes`, asks.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["only", "method", "roots"])]
    pub profile: Option<String>,
    /// Don't ask: clean the Safe findings of the rules `--only` names, by deleting them,
    /// or what `--profile` authorizes.
    #[arg(long)]
    pub yes: bool,
    /// Show the plan and what was left out of it, then stop: nothing is changed.
    #[arg(long, conflicts_with_all = ["json", "events"])]
    pub dry_run: bool,
    /// How to write what happened.
    #[command(flatten)]
    pub output: Output,
}

/// `kipple plan` options.
#[derive(Debug, Args)]
pub struct PlanArgs {
    /// What to plan.
    #[command(flatten)]
    pub choice: Choice,
    /// Write the plan to this new file, or `-` for stdout.
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    pub output: PathBuf,
}

/// `kipple apply` options.
#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// A plan file `kipple plan -o` wrote.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
    /// Scan this project root instead of the configured ones, as the plan was made with.
    #[arg(long = "root", value_name = "PATH")]
    pub roots: Vec<PathBuf>,
    /// Don't ask: delete the plan's items that are still Safe.
    #[arg(long)]
    pub yes: bool,
    /// How to write what happened.
    #[command(flatten)]
    pub output: Output,
}

/// How `clean` and `apply` write what happened.
#[derive(Debug, Args)]
pub struct Output {
    /// Print one `kipple.receipt/v1` envelope when it finishes.
    #[arg(long, conflicts_with = "events")]
    pub json: bool,
    /// Stream the scan's events, then the run's, one JSON object per line, then the
    /// receipt.
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub events: Option<EventsFormat>,
}

impl Output {
    const fn format(&self) -> Format {
        match (self.json, self.events) {
            (_, Some(EventsFormat::Jsonl)) => Format::Events,
            (true, None) => Format::Json,
            (false, None) => Format::Human,
        }
    }
}

/// How a mutating command's result is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Human,
    Json,
    Events,
}

/// Refuses a run whose store can't exist: with kipple's data folder unknown, the host's
/// protections hold every item, which would read as items the system protects.
fn store_known(settings: &Settings, stderr: &mut dyn Write) -> Result<(), ExitCode> {
    match kipple_dirs(&settings.roots).data.path {
        Ok(_) => Ok(()),
        Err(_) => Err(fail(
            stderr,
            &outcome::store_refusal(&StoreRefusal::NoDataDir),
        )),
    }
}

/// Loads the settings for `roots` and makes the machine they and `method` describe. A
/// config that can't be used exits 2.
fn machine(
    host: &Host<'_>,
    roots: &[PathBuf],
    method: Option<Method>,
    stderr: &mut dyn Write,
) -> Result<Machine, ExitCode> {
    let roots = absolute(roots).map_err(|error| fail(stderr, &format!("--root: {error}")))?;
    let loaded = host.settings(&roots, stderr)?;
    Machine::from_settings(host.builtin, host.env, loaded, method)
        .map_err(|message| fail(stderr, &message))
}

/// What a command scanned with: the machine it runs through, and the scan's report.
struct Scanned {
    machine: Machine,
    report: ScanReport,
    /// The `seq` the run's events continue from, after the scan's own.
    next_seq: u64,
}

/// Loads the settings for `roots`, makes the machine they and `method` describe, and
/// scans the roots, streaming the scan's events when `format` says so. A config that
/// can't be used exits 2, a cancelled scan 130.
fn scanned(
    host: &Host<'_>,
    roots: &[PathBuf],
    method: Option<Method>,
    format: Format,
    io: &mut Io<'_>,
) -> Result<Scanned, ExitCode> {
    let machine = machine(host, roots, method, io.stderr)?;
    scan_with(host, machine, format, io)
}

/// Scans the roots of `machine`'s settings, streaming the scan's events when `format`
/// says so. A cancelled scan exits 130.
fn scan_with(
    host: &Host<'_>,
    machine: Machine,
    format: Format,
    io: &mut Io<'_>,
) -> Result<Scanned, ExitCode> {
    let (engine, settings) = (machine.engine(), machine.settings());
    let lenses = host.builtin.lenses();
    let mut stream = scan::Stream {
        lenses: &lenses,
        policy: &settings.policy,
        found: std::collections::HashMap::new(),
    };
    let mut next_seq = 0;
    let stdout = &mut *io.stdout;
    let scan::Scanned { report, streamed } = scan::run(
        engine,
        settings,
        None,
        &mut |event| {
            next_seq = event.seq + 1;
            match format {
                Format::Events => scan::write_event(stdout, &event, &mut stream),
                Format::Human | Format::Json => Ok(()),
            }
        },
        &io.cancel,
    );
    let written = streamed.and_then(|()| match format {
        Format::Events => {
            let out = crate::json::ScanOut::new(&report, &lenses, &settings.policy);
            let completed = EventBody::Completed {
                report: Box::new(out),
            };
            let written = scan::write_record(stdout, next_seq, completed);
            next_seq += 1;
            written
        }
        Format::Human | Format::Json => Ok(()),
    });
    let code = crate::exit_code(written, io.stderr);
    if code != ExitCode::SUCCESS {
        return Err(code);
    }
    if report.outcome == ScanOutcome::Cancelled || io.cancel.is_cancelled() {
        return Err(ExitCode::from(CANCELLED));
    }
    Ok(Scanned {
        machine,
        report,
        next_seq,
    })
}

/// The rules `--only` names, each a built-in rule; `None` for every rule.
fn only(
    host: &Host<'_>,
    named: &[String],
    stderr: &mut dyn Write,
) -> Result<Option<BTreeSet<RuleId>>, ExitCode> {
    if named.is_empty() {
        return Ok(None);
    }
    let catalog = crate::config::catalog(host.builtin);
    named
        .iter()
        .map(|name| {
            catalog
                .iter()
                .find(|rule| rule.0 == *name)
                .cloned()
                .ok_or_else(|| {
                    say(stderr, &words::no_such_rule(name));
                    ExitCode::from(USAGE)
                })
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

/// What `clean` and `plan` take from a scan: every Safe finding of the rules chosen, and,
/// to list them, the Review ones the TUI's per-item review is for (pinned 1).
struct Picked<'scan> {
    selection: Selection,
    review: Vec<&'scan Finding>,
}

fn pick<'scan>(
    report: &'scan ScanReport,
    policy: &Policy,
    only: Option<&BTreeSet<RuleId>>,
) -> Picked<'scan> {
    let chosen = report
        .findings
        .iter()
        .filter(|finding| only.is_none_or(|only| only.contains(&finding.rule)));
    let mut picked = Picked {
        selection: Selection::default(),
        review: Vec::new(),
    };
    for finding in chosen {
        match finding.verdict(policy).map(|verdict| verdict.bucket()) {
            Some(Bucket::Safe) => {
                picked.selection.0.insert(finding.id);
            }
            Some(Bucket::Review) => picked.review.push(finding),
            Some(Bucket::Protected) | None => {}
        }
    }
    picked
}

/// `kipple clean`: plans the Safe findings chosen, or what `--profile` authorizes, has a
/// person confirm them on a terminal or takes `--yes`, then runs them as one stored run.
/// With `--dry-run` it shows the plan and stops.
pub(crate) fn clean(host: &Host<'_>, args: &CleanArgs, io: &mut Io<'_>) -> ExitCode {
    let ToClean {
        scanned,
        only,
        saved,
    } = match to_clean(host, args, io) {
        Ok(to_clean) => to_clean,
        Err(code) => return code,
    };
    let machine = &scanned.machine;
    let policy = &machine.settings().policy;
    let check = saved
        .as_ref()
        .map(|saved| profile::checked(saved, machine, &scanned.report, io.stderr));
    let picked = match &check {
        Some(check) => Picked {
            selection: check.selection().clone(),
            review: Vec::new(),
        },
        None => pick(&scanned.report, policy, only.as_ref()),
    };
    let ink = host.ink(io.stderr);
    let format = args.output.format();
    let mut events = output::Events::new(format, scanned.next_seq, io.cancel.clone(), ink.clone())
        .crashing_at(crate::crash_point(host.env));
    let shown = output::Shown {
        review: &picked.review,
        policy,
        told: output::Told::NotYet,
    };
    if let Some(check) = check.as_ref().filter(|check| check.grant().is_none()) {
        let finished = profile::none_active(check, io.stderr);
        return events.finish(&finished, &shown, io.stdout, io.stderr);
    }
    let request = CleanRequest {
        scan: &scanned.report,
        selection: &picked.selection,
    };
    if args.dry_run {
        let dry = DryRun {
            machine,
            request: &request,
            mode: dry_mode(args, check.as_ref()),
            shown: &shown,
            ink: &ink,
        };
        return dry.show(io.stdout, io.stderr, &io.cancel);
    }
    let Io {
        stdin,
        stdout,
        stderr,
        cancel,
        ..
    } = io;
    let cancel: &CancelToken = cancel;
    let (mut finished, told) = machine.with_session(|session| {
        let mut prompt = prompt::Prompt {
            input: &mut **stdin,
            out: &mut **stderr,
            ink: &ink,
            scan: &scanned.report,
            review: &picked.review,
            policy,
            above: machine.settings().loaded.config.confirm_by_typing_above,
            cancel,
            told: output::Told::NotYet,
        };
        let consent = consent(args, check.as_ref(), &mut prompt);
        let mut sink = events.sink(&mut **stdout);
        let finished = run_clean(&request, session, consent, &mut sink, cancel);
        (finished, prompt.told)
    });
    finished.profile = check.as_ref().map(ProfileCheck::summary);
    let shown = output::Shown { told, ..shown };
    events.finish(&finished, &shown, stdout, stderr)
}

/// What `clean` plans from: the scan, the rules `--only` names, and the profile
/// `--profile` names, read once before the scan.
struct ToClean {
    scanned: Scanned,
    only: Option<BTreeSet<RuleId>>,
    saved: Option<Profile>,
}

/// Refuses what `clean` can't run before anything is scanned, then reads the profile and
/// scans.
fn to_clean(host: &Host<'_>, args: &CleanArgs, io: &mut Io<'_>) -> Result<ToClean, ExitCode> {
    let only = only(host, &args.choice.only, io.stderr)?;
    let name = args
        .profile
        .as_deref()
        .map(|text| profile::name(text, io.stderr))
        .transpose()?;
    if args.yes && only.is_none() && name.is_none() {
        say(io.stderr, words::YES_NEEDS_ONLY);
        return Err(ExitCode::from(BLOCKED));
    }
    if !args.yes && !args.dry_run && io.person == Person::Absent {
        let why = match name {
            Some(_) => profile_words::RUN_NEEDS_YES,
            None => words::NO_TERMINAL,
        };
        say(io.stderr, why);
        return Err(ExitCode::from(BLOCKED));
    }
    let method = args.choice.method.map(Method::from);
    let machine = machine(host, &args.choice.roots, method, io.stderr)?;
    // Read once, before the scan: what is checked and run is this profile, whatever
    // happens to its file meanwhile.
    let saved = name
        .map(|name| profile::saved(&machine, &name, io.stderr))
        .transpose()?;
    let scanned = scan_with(host, machine, args.output.format(), io)?;
    store_known(scanned.machine.settings(), io.stderr)?;
    Ok(ToClean {
        scanned,
        only,
        saved,
    })
}

/// How `clean`'s plan is confirmed: `--yes` with a profile by its grant, `--yes` alone as
/// automation, and otherwise by the person at the prompt.
fn consent<'prompt>(
    args: &CleanArgs,
    check: Option<&ProfileCheck>,
    prompt: &'prompt mut prompt::Prompt<'_>,
) -> Consent<'prompt> {
    match (args.yes, check.and_then(ProfileCheck::grant)) {
        (true, Some(grant)) => Consent::Profile(grant.clone()),
        (true, None) => Consent::Yes,
        (false, _) => Consent::Ask(prompt),
    }
}

/// What `clean --dry-run` plans for: what the same command's consent may hold.
const fn dry_mode(args: &CleanArgs, check: Option<&ProfileCheck>) -> PlanMode {
    match (args.yes, check) {
        (true, None) => PlanMode::SafeOnly,
        (false, _) | (true, Some(_)) => PlanMode::Safe,
    }
}

/// `clean --dry-run`: the plan the same command would confirm, planned without the lock,
/// a run, or anything written.
struct DryRun<'run> {
    machine: &'run Machine,
    request: &'run CleanRequest<'run>,
    /// What the same command's consent may plan.
    mode: PlanMode,
    shown: &'run output::Shown<'run>,
    ink: &'run Ink,
}

impl DryRun<'_> {
    /// Plans it and writes the plan, what was left out and how a run would end on
    /// `stdout`. Exits 0, or 130 when cancelled or stdout closed.
    fn show(
        &self,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
        cancel: &CancelToken,
    ) -> ExitCode {
        let prepared = self
            .machine
            .with_session(|session| prepare_clean(self.request, session, self.mode, cancel));
        if cancel.is_cancelled() {
            return ExitCode::from(CANCELLED);
        }
        let mut lines = prompt::plan_lines(&prepared.review(), self.request.scan, self.shown);
        if let Some(ending) = prepared.ending() {
            lines.push(output::Line::new(Role::Text, words::ending_line(ending)));
        }
        lines.push(output::Line::new(Role::Muted, words::DRY_RUN.to_owned()));
        let mut write = || -> io::Result<()> {
            for line in &lines {
                writeln!(stdout, "{}", self.ink.paint(line.role, &line.text))?;
            }
            stdout.flush()
        };
        crate::exit_code(write(), stderr)
    }
}

/// `kipple plan -o FILE`: plans the Safe findings chosen, as `clean` would on a terminal,
/// and writes the plan as a new `kipple.plan/v1` file, or to stdout for `-`. Nothing
/// outside that file is written.
pub(crate) fn plan(host: &Host<'_>, args: &PlanArgs, io: &mut Io<'_>) -> ExitCode {
    let only = match only(host, &args.choice.only, io.stderr) {
        Ok(only) => only,
        Err(code) => return code,
    };
    let method = args.choice.method.map(Method::from);
    let scanned = match scanned(host, &args.choice.roots, method, Format::Human, io) {
        Ok(scanned) => scanned,
        Err(code) => return code,
    };
    let machine = &scanned.machine;
    let picked = pick(&scanned.report, &machine.settings().policy, only.as_ref());
    let planned = kipple_core::plan(
        &PlanRequest {
            scan: &scanned.report,
            selection: &picked.selection,
            mode: PlanMode::Safe,
        },
        &machine.judge(),
        &machine.mutation(),
        &io.cancel,
    );
    if io.cancel.is_cancelled() {
        return ExitCode::from(CANCELLED);
    }
    let bytes = match encode_plan_file(&planned.plan, &OsNames) {
        Ok(bytes) => bytes,
        Err(error) => return fail(io.stderr, &error.to_string()),
    };
    let to = (args.output != Path::new("-")).then_some(args.output.as_path());
    let written = match to {
        Some(file) => write_new(file, &bytes),
        None => io.stdout.write_all(&bytes).and_then(|()| io.stdout.flush()),
    };
    if let Err(error) = written {
        if error.kind() == io::ErrorKind::BrokenPipe {
            return ExitCode::from(CANCELLED);
        }
        let why = escaped(&error.to_string());
        return fail(io.stderr, &words::plan_not_written(&args.output, &why));
    }
    let ink = host.ink(io.stderr);
    let refused = RefusedItem::list(&scanned.report, &planned.refused);
    let shown = output::Shown {
        review: &picked.review,
        policy: &machine.settings().policy,
        told: output::Told::NotYet,
    };
    let lines = output::left_out(&refused, &[], &shown);
    let mut report = || -> io::Result<()> {
        for line in &lines {
            writeln!(io.stderr, "{}", ink.paint(line.role, &line.text))?;
        }
        writeln!(
            io.stderr,
            "{}",
            words::plan_written(to, planned.plan.entries().len())
        )
    };
    report().ok();
    ExitCode::SUCCESS
}

/// `kipple apply FILE`: plans the file's items again from a fresh scan under the current
/// config, so a file can only take items away, has a person confirm the fresh plan or
/// takes `--yes`, then runs it as one stored run.
pub(crate) fn apply(host: &Host<'_>, args: &ApplyArgs, io: &mut Io<'_>) -> ExitCode {
    if !args.yes && io.person == Person::Absent {
        say(io.stderr, words::NO_TERMINAL);
        return ExitCode::from(BLOCKED);
    }
    let imported = match read_plan(&args.file) {
        Ok(imported) => imported,
        Err(why) => {
            say(io.stderr, &words::not_a_plan(&args.file, &why));
            return ExitCode::from(USAGE);
        }
    };
    let format = args.output.format();
    let scanned = match scanned(host, &args.roots, None, format, io) {
        Ok(scanned) => scanned,
        Err(code) => return code,
    };
    let machine = &scanned.machine;
    if let Err(code) = store_known(machine.settings(), io.stderr) {
        return code;
    }
    let ink = host.ink(io.stderr);
    let mut events = output::Events::new(format, scanned.next_seq, io.cancel.clone(), ink.clone())
        .crashing_at(crate::crash_point(host.env));
    let Io {
        stdin,
        stdout,
        stderr,
        cancel,
        ..
    } = io;
    let cancel: &CancelToken = cancel;
    let finished = machine.with_session(|session| {
        let mut prompt = prompt::Prompt {
            input: &mut **stdin,
            out: &mut **stderr,
            ink: &ink,
            scan: &scanned.report,
            review: &[],
            policy: &machine.settings().policy,
            above: machine.settings().loaded.config.confirm_by_typing_above,
            cancel,
            told: output::Told::NotYet,
        };
        let consent = if args.yes {
            Consent::Yes
        } else {
            Consent::Ask(&mut prompt)
        };
        let file = ApplyFile {
            imported: &imported,
            scan: &scanned.report,
        };
        let mut sink = events.sink(&mut **stdout);
        let finished = run_apply(&file, session, consent, &mut sink, cancel);
        (finished, prompt.told)
    });
    let (finished, told) = finished;
    let shown = output::Shown {
        review: &[],
        policy: &machine.settings().policy,
        told,
    };
    events.finish(&finished, &shown, stdout, stderr)
}

/// The plan file at `path`, read whole up to the format's bound and decoded.
fn read_plan(path: &Path) -> Result<kipple_core::ImportedPlan, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(PLAN_LIMIT as u64 + 1).read_to_end(&mut bytes))
        .map_err(|error| escaped(&error.to_string()))?;
    decode_plan_file(&bytes, &OsNames).map_err(|error| error.to_string())
}
