//! The `kipple` command-line interface.
//!
//! The binary is the composition root: it captures the environment once and wires the
//! platform and the integrations into each command. This library target also lets
//! `cargo xtask gen-docs` render the CLI reference and the JSON schemas from the same
//! definitions.
#![forbid(unsafe_code)]

pub mod doctor;
mod explain;
mod human;
pub mod json;
pub mod rules;
mod scan;
pub mod setup;
pub mod support;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use kipple_core::{Lens, ScanOutcome};
use kipple_platform::Environment;

use crate::json::{EventBody, ScanOut};
use crate::setup::Builtin;

/// Command-line arguments for `kipple`.
#[derive(Debug, Parser)]
#[command(name = "kipple", version, about, subcommand_required = true)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// kipple's commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Find what your tools left behind. Read-only: nothing is changed.
    Scan(ScanArgs),
    /// List the integrations and rules, and which of them run on this OS.
    Rules {
        /// Print one JSON envelope, with the capability catalog rules may use.
        #[arg(long)]
        json: bool,
    },
    /// Explain a rule, or what kipple found at a path and why. A path runs a scan.
    Explain {
        /// A rule ID such as `builtin.cargo.target`, or a path.
        target: String,
    },
    /// Show where kipple looks, what it can read and which tools it found. Read-only.
    Doctor {
        /// Print one JSON envelope.
        #[arg(long)]
        json: bool,
    },
}

/// `kipple scan` options.
#[derive(Debug, Args)]
pub struct ScanArgs {
    /// Only this lens.
    #[arg(long, value_enum)]
    pub lens: Option<LensArg>,
    /// Scan this project root instead of the defaults. Repeat for several.
    #[arg(long = "root", value_name = "PATH")]
    pub roots: Vec<PathBuf>,
    /// Print one JSON envelope when the scan finishes.
    #[arg(long, conflicts_with = "events")]
    pub json: bool,
    /// Stream versioned events as they happen, one JSON object per line.
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub events: Option<EventsFormat>,
}

/// A home-screen lens.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LensArg {
    /// What AI coding agents leave behind.
    Agents,
    /// Developer tools and projects.
    Dev,
    /// The operating system and its package managers.
    System,
}

impl From<LensArg> for Lens {
    fn from(lens: LensArg) -> Self {
        match lens {
            LensArg::Agents => Self::Agents,
            LensArg::Dev => Self::Dev,
            LensArg::System => Self::System,
        }
    }
}

/// An event stream format.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum EventsFormat {
    /// One JSON object per line.
    Jsonl,
}

/// The exit code when stdout closes before the output is written (`docs/03-functional-
/// spec.md` §4): the same as a cancel.
pub const CANCELLED: u8 = 130;

/// The exit code for a usage error, such as an unknown rule (`docs/03-functional-spec.md`
/// §4).
pub const USAGE: u8 = 2;

/// Runs `cli`, writing data to `stdout` and diagnostics to `stderr`.
pub fn run(
    cli: &Cli,
    env: &Environment,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    let builtin = match Builtin::load() {
        Ok(builtin) => builtin,
        Err(message) => return fail(stderr, &message),
    };
    match &cli.command {
        Command::Scan(args) => scan(args, &builtin, env, stdout, stderr),
        Command::Rules { json } => {
            let rules = rules::RulesOut::new(&builtin);
            let written = if *json {
                write_json(stdout, &rules)
            } else {
                rules::write_human(stdout, &rules)
            };
            exit_code(written, stderr)
        }
        Command::Explain { target } => explain(target, &builtin, env, stdout, stderr),
        Command::Doctor { json } => {
            let report = doctor::gather(env, &builtin);
            let written = if *json {
                write_json(stdout, &report.to_json())
            } else {
                report.write_to(stdout)
            };
            exit_code(written, stderr)
        }
    }
}

fn scan(
    args: &ScanArgs,
    builtin: &Builtin,
    env: &Environment,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    let engine = match builtin.engine() {
        Ok(engine) => engine,
        Err(message) => return fail(stderr, &message),
    };
    let roots = match absolute(&args.roots) {
        Ok(roots) => roots,
        Err(error) => return fail(stderr, &format!("--root: {error}")),
    };
    let lenses = builtin.lenses();
    let lens = args.lens.map(Lens::from);
    let mut last_seq = None;
    let (report, streamed) = scan::run(&engine, env, &roots, lens, &mut |event| {
        last_seq = Some(event.seq);
        match args.events {
            Some(EventsFormat::Jsonl) => scan::write_event(stdout, &event, &lenses),
            None => Ok(()),
        }
    });
    let out = ScanOut::new(&report, &lenses);
    let written = streamed.and_then(|()| match args.events {
        Some(EventsFormat::Jsonl) => {
            let seq = last_seq.map_or(0, |seq| seq + 1);
            let completed = EventBody::Completed {
                report: Box::new(out),
            };
            scan::write_record(stdout, seq, completed)
        }
        None if args.json => write_json(stdout, &out),
        None => scan::write_human(stdout, &out),
    });
    match exit_code(written, stderr) {
        code if code == ExitCode::SUCCESS && report.outcome == ScanOutcome::Cancelled => {
            ExitCode::from(CANCELLED)
        }
        code => code,
    }
}

fn explain(
    target: &str,
    builtin: &Builtin,
    env: &Environment,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    if let Some((pack, rule)) = explain::find_rule(builtin, target) {
        let written =
            explain::write_rule(stdout, builtin, pack, rule).and_then(|()| stdout.flush());
        return exit_code(written, stderr);
    }
    if target.starts_with("builtin.") {
        let message = format!("kipple: there is no rule {target}; `kipple rules` lists them");
        writeln!(stderr, "{message}").ok();
        return ExitCode::from(USAGE);
    }
    let engine = match builtin.engine() {
        Ok(engine) => engine,
        Err(message) => return fail(stderr, &message),
    };
    let (report, _) = scan::run(&engine, env, &[], None, &mut |_| Ok(()));
    let path = PathBuf::from(target);
    let written =
        explain::write_path(stdout, builtin, &path, &report.findings).and_then(|()| stdout.flush());
    exit_code(written, stderr)
}

/// `roots`, each made absolute against the current directory.
fn absolute(roots: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    roots.iter().map(std::path::absolute).collect()
}

/// Writes `value` as one pretty JSON document.
fn write_json(out: &mut dyn Write, value: &impl serde::Serialize) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)?;
    out.flush()
}

/// Reports an internal failure, which is a bug in kipple or its environment.
fn fail(stderr: &mut dyn Write, message: &str) -> ExitCode {
    writeln!(stderr, "kipple: {message}").ok();
    ExitCode::FAILURE
}

/// The exit code for a command whose output was written, or failed to be. A closed stdout
/// is a cancel: exit 130 and say nothing more.
pub fn exit_code(written: io::Result<()>, stderr: &mut dyn Write) -> ExitCode {
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::from(CANCELLED),
        Err(error) => {
            // Nothing else can be reported if stderr fails too.
            writeln!(stderr, "kipple: could not write the output: {error}").ok();
            ExitCode::FAILURE
        }
    }
}
