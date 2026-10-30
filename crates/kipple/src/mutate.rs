//! The commands that change the disk (`docs/03-functional-spec.md` §3, spec M3b pinned 1,
//! 2, 12, 17–19, D-067): `clean`, `plan -o`, `apply FILE`, and `history` and `restore`
//! over the runs they keep.
//!
//! Each composes the core's sessions with the platform: the run store under kipple's
//! data directory, the confined executor and trash, git, and the host's protections.
//! Planning, authorization, apply and reconciliation are the core's; this module only
//! asks the person, and writes what happened.

mod output;
mod prompt;
mod runs;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, ValueEnum};
use kipple_core::format::{PLAN_LIMIT, decode_plan_file, encode_plan_file};
use kipple_core::{
    ApplyFile, ApplyPorts, Bucket, CleanRequest, Consent, Engine, Finding, Judge, Method, Methods,
    MutationPorts, PlanMode, PlanRequest, Policy, Protections, RefusedItem, Rule, RuleId,
    ScanOutcome, ScanReport, Selection, Session, SymbolicRoot, run_apply, run_clean,
};
use kipple_platform::{
    ConfinedFs, ConfinedTrash, FileRunStore, GitQuery, OsNames, clock, kipple_dirs, write_new,
};

pub use self::runs::{HistoryArgs, RestoreArgs};
pub(crate) use self::runs::{history, restore};
use crate::config::{Settings, held_at};
use crate::human::{escaped, receipt as words};
use crate::json::EventBody;
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
    /// Don't ask: clean the Safe findings of the rules `--only` names, by deleting them.
    #[arg(long)]
    pub yes: bool,
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

/// What every mutating command removes and records through, besides the engine.
pub(crate) struct Machine {
    rules: Vec<Rule>,
    methods: Methods,
    protections: Protections,
    store: FileRunStore,
    trash: ConfinedTrash,
    git: GitQuery,
}

impl Machine {
    /// The machine `settings` and `--method` describe: the run store under kipple's data
    /// directory, the trash the roots resolve, and the host's protections, which hold
    /// that data directory and every state root.
    pub(crate) fn new(host: &Host<'_>, settings: &Settings, method: Option<Method>) -> Self {
        let roots = &settings.roots;
        let data = kipple_dirs(roots).data.path.ok();
        let clock: Arc<dyn kipple_core::Clock> = Arc::from(clock(host.env));
        let trash = match roots.get(SymbolicRoot::Trash) {
            Some(Ok(resolved)) => Some(resolved.path.clone()),
            Some(Err(_)) | None => None,
        };
        Self {
            rules: host.builtin.platform_rules(),
            methods: Methods {
                command_line: method,
                rules: BTreeMap::new(),
                config: settings.loaded.config.method,
            },
            protections: Protections::of_host(roots, data.as_deref(), &|path| held_at(path, roots)),
            store: FileRunStore::new(data, Arc::clone(&clock)),
            trash: ConfinedTrash::new(trash, clock),
            git: GitQuery::new(host.env),
        }
    }

    fn judge<'machine>(&'machine self, policy: &'machine Policy) -> Judge<'machine> {
        Judge {
            rules: &self.rules,
            policy,
            methods: &self.methods,
            protections: &self.protections,
        }
    }

    fn mutation(&self) -> MutationPorts<'_> {
        MutationPorts {
            freezer: &ConfinedFs,
            executor: &ConfinedFs,
            trash: &self.trash,
            tracked: &self.git,
        }
    }
}

/// Runs `then` with the session over `machine`, judging with `settings`.
fn with_session<R>(
    engine: &Engine,
    settings: &Settings,
    machine: &Machine,
    then: impl FnOnce(&Session<'_>) -> R,
) -> R {
    let request = scan::request(settings, None);
    let rescan = engine.rescan(&request);
    let mutation = machine.mutation();
    let session = Session {
        judge: machine.judge(&settings.policy),
        store: &machine.store,
        ports: ApplyPorts {
            rescan: &rescan,
            mutation: &mutation,
        },
        codec: &OsNames,
    };
    then(&session)
}

/// What a command scanned with: the settings, the engine and the scan's report.
struct Scanned {
    settings: Settings,
    engine: Engine,
    report: ScanReport,
    /// The `seq` the run's events continue from, after the scan's own.
    next_seq: u64,
}

/// Loads the settings for `roots`, and scans them, streaming the scan's events when
/// `format` says so. A config that can't be used exits 2, a cancelled scan 130.
fn scanned(
    host: &Host<'_>,
    roots: &[PathBuf],
    format: Format,
    io: &mut Io<'_>,
) -> Result<Scanned, ExitCode> {
    let roots = absolute(roots).map_err(|error| fail(io.stderr, &format!("--root: {error}")))?;
    let settings = host.settings(&roots, io.stderr)?;
    let engine = host.engine().map_err(|message| fail(io.stderr, &message))?;
    let lenses = host.builtin.lenses();
    let mut stream = scan::Stream {
        lenses: &lenses,
        policy: &settings.policy,
        found: std::collections::HashMap::new(),
    };
    let mut next_seq = 0;
    let stdout = &mut *io.stdout;
    let scan::Scanned { report, streamed } = scan::run(
        &engine,
        &settings,
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
        settings,
        engine,
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

/// `kipple clean`: plans the Safe findings chosen, has a person confirm them on a
/// terminal or takes `--yes`, then runs them as one stored run.
pub(crate) fn clean(host: &Host<'_>, args: &CleanArgs, io: &mut Io<'_>) -> ExitCode {
    let only = match only(host, &args.choice.only, io.stderr) {
        Ok(only) => only,
        Err(code) => return code,
    };
    if args.yes && only.is_none() {
        say(io.stderr, words::YES_NEEDS_ONLY);
        return ExitCode::from(BLOCKED);
    }
    if !args.yes && io.person == Person::Absent {
        say(io.stderr, words::NO_TERMINAL);
        return ExitCode::from(BLOCKED);
    }
    let format = args.output.format();
    let scanned = match scanned(host, &args.choice.roots, format, io) {
        Ok(scanned) => scanned,
        Err(code) => return code,
    };
    let picked = pick(&scanned.report, &scanned.settings.policy, only.as_ref());
    let method = args.choice.method.map(Method::from);
    let machine = Machine::new(host, &scanned.settings, method);
    let ink = host.ink(io.stderr);
    let request = CleanRequest {
        scan: &scanned.report,
        selection: &picked.selection,
    };
    let mut events = output::Events::new(format, scanned.next_seq, io.cancel.clone(), ink.clone())
        .crashing_at(crate::crash_point(host.env));
    let Io {
        stdin,
        stdout,
        stderr,
        cancel,
        ..
    } = io;
    let finished = with_session(&scanned.engine, &scanned.settings, &machine, |session| {
        let mut prompt = prompt::Prompt {
            input: &mut **stdin,
            out: &mut **stderr,
            ink: &ink,
            scan: &scanned.report,
            review: &picked.review,
            policy: &scanned.settings.policy,
            told: output::Told::NotYet,
        };
        let consent = if args.yes {
            Consent::Yes
        } else {
            Consent::Ask(&mut prompt)
        };
        let mut sink = events.sink(&mut **stdout);
        let finished = run_clean(&request, session, consent, &mut sink, cancel);
        (finished, prompt.told)
    });
    let (finished, told) = finished;
    let shown = output::Shown {
        review: &picked.review,
        policy: &scanned.settings.policy,
        told,
    };
    events.finish(&finished, &shown, stdout, stderr)
}

/// `kipple plan -o FILE`: plans the Safe findings chosen, as `clean` would on a terminal,
/// and writes the plan as a new `kipple.plan/v1` file, or to stdout for `-`. Nothing
/// outside that file is written.
pub(crate) fn plan(host: &Host<'_>, args: &PlanArgs, io: &mut Io<'_>) -> ExitCode {
    let only = match only(host, &args.choice.only, io.stderr) {
        Ok(only) => only,
        Err(code) => return code,
    };
    let scanned = match scanned(host, &args.choice.roots, Format::Human, io) {
        Ok(scanned) => scanned,
        Err(code) => return code,
    };
    let picked = pick(&scanned.report, &scanned.settings.policy, only.as_ref());
    let method = args.choice.method.map(Method::from);
    let machine = Machine::new(host, &scanned.settings, method);
    let planned = kipple_core::plan(
        &PlanRequest {
            scan: &scanned.report,
            selection: &picked.selection,
            mode: PlanMode::Safe,
        },
        &machine.judge(&scanned.settings.policy),
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
        policy: &scanned.settings.policy,
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
    let scanned = match scanned(host, &args.roots, format, io) {
        Ok(scanned) => scanned,
        Err(code) => return code,
    };
    let machine = Machine::new(host, &scanned.settings, None);
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
    let finished = with_session(&scanned.engine, &scanned.settings, &machine, |session| {
        let mut prompt = prompt::Prompt {
            input: &mut **stdin,
            out: &mut **stderr,
            ink: &ink,
            scan: &scanned.report,
            review: &[],
            policy: &scanned.settings.policy,
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
        policy: &scanned.settings.policy,
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
