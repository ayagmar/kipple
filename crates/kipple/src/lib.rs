//! The `kipple` command-line interface.
//!
//! The binary is the composition root: it captures the environment once and wires the
//! platform and the integrations into each command. This library target also lets
//! `cargo xtask gen-docs` render the CLI reference and the JSON schemas from the same
//! definitions.
#![forbid(unsafe_code)]

pub mod config;
pub mod doctor;
mod explain;
mod human;
pub mod json;
pub mod rules;
mod scan;
pub mod setup;
pub mod support;
pub mod tui;

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use kipple_core::{Engine, Lens, ScanOutcome};
use kipple_platform::Environment;

use crate::config::{ConfigError, Settings};
use crate::json::{EventBody, ScanOut};
use crate::setup::Builtin;

/// Command-line arguments for `kipple`.
#[derive(Debug, Parser)]
#[command(
    name = "kipple",
    version,
    about,
    after_help = "Without a command, kipple opens the home screen in a terminal, or prints a \
                  summary when its output isn't a terminal. Read-only: nothing is changed."
)]
pub struct Cli {
    /// Read the config from this file instead of `KIPPLE_CONFIG` or the default location.
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// The home screen's options.
    #[command(flatten)]
    pub home: HomeArgs,
    /// What to do. Without one, the home screen.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Options of the home screen.
#[derive(Debug, Args)]
pub struct HomeArgs {
    /// Draw the home screen with ASCII characters only.
    #[arg(long)]
    pub ascii: bool,
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
    /// Show where the config file is, or what it says.
    Config {
        /// What to show.
        #[command(subcommand)]
        action: ConfigAction,
    },
}

/// `kipple config` actions.
#[derive(Debug, Clone, Copy, Subcommand)]
pub enum ConfigAction {
    /// Print the config file's path.
    Path,
    /// Print the settings the config gives, or why it can't be used.
    Show,
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
    // Clap can't exempt the global `--config` from a conflict with the commands, so the
    // home screen's own option is refused here.
    if cli.command.is_some() && cli.home.ascii {
        let message = "kipple: --ascii is for the home screen, not a command";
        writeln!(stderr, "{message}").ok();
        return ExitCode::from(USAGE);
    }
    let builtin = match Builtin::load() {
        Ok(builtin) => builtin,
        Err(message) => return fail(stderr, &message),
    };
    let host = Host {
        builtin: &builtin,
        env,
        config: cli.config.as_deref(),
    };
    match &cli.command {
        None => summary(&host, stdout, stderr),
        Some(Command::Scan(args)) => scan(&host, args, stdout, stderr),
        Some(Command::Rules { json }) => {
            let rules = rules::RulesOut::new(&builtin);
            let written = if *json {
                write_json(stdout, &rules)
            } else {
                rules::write_human(stdout, &rules)
            };
            exit_code(written, stderr)
        }
        Some(Command::Explain { target }) => explain(&host, target, stdout, stderr),
        Some(Command::Config { action }) => config_command(&host, *action, stdout, stderr),
        Some(Command::Doctor { json }) => {
            let report = doctor::gather(env, host.config, &builtin);
            let written = if *json {
                write_json(stdout, &report.to_json())
            } else {
                report.write_to(stdout)
            };
            exit_code(written, stderr)
        }
    }
}

/// Opens the home screen in this terminal (`docs/03-functional-spec.md` §2 J1). Colour
/// follows `NO_COLOR`. It needs a config that can be used.
pub fn home(cli: &Cli, env: &Environment, stderr: &mut dyn Write) -> ExitCode {
    let builtin = match Builtin::load() {
        Ok(builtin) => builtin,
        Err(message) => return fail(stderr, &message),
    };
    let host = Host {
        builtin: &builtin,
        env,
        config: cli.config.as_deref(),
    };
    let settings = match host.settings(&[], stderr) {
        Ok(settings) => settings,
        Err(code) => return code,
    };
    let look = tui::Look {
        color: env.get("NO_COLOR").is_none(),
        ascii: cli.home.ascii,
    };
    match tui::open(env, &settings, look) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(stderr, &format!("the home screen failed: {error}")),
    }
}

/// What every command runs with: the built-in rules, the environment snapshot and the
/// config file `--config` names, if any.
#[derive(Debug, Clone, Copy)]
struct Host<'a> {
    builtin: &'a Builtin,
    env: &'a Environment,
    config: Option<&'a Path>,
}

impl Host<'_> {
    /// The config and roots a scan runs with, the project roots `given` on the command
    /// line, if any. A config that can't be used is reported on `stderr` with exit code
    /// 2; each kept or excluded path that is missing is warned about.
    fn settings(&self, given: &[PathBuf], stderr: &mut dyn Write) -> Result<Settings, ExitCode> {
        let catalog = config::catalog(self.builtin);
        match Settings::load(self.env, self.config, given, &catalog) {
            Ok(settings) => {
                for warning in &settings.warnings {
                    writeln!(stderr, "kipple: warning: {warning}").ok();
                }
                Ok(settings)
            }
            Err(error) => Err(config_failed(stderr, &error)),
        }
    }

    fn engine(&self) -> Result<Engine, String> {
        self.builtin.engine(self.env)
    }
}

/// Reports a config that can't be used: exit code 2.
fn config_failed(stderr: &mut dyn Write, error: &ConfigError) -> ExitCode {
    writeln!(stderr, "kipple: {error}").ok();
    ExitCode::from(USAGE)
}

/// `kipple` without a terminal: the buckets' totals, never the home screen.
fn summary(host: &Host<'_>, stdout: &mut dyn Write, stderr: &mut dyn Write) -> ExitCode {
    let settings = match host.settings(&[], stderr) {
        Ok(settings) => settings,
        Err(code) => return code,
    };
    let engine = match host.engine() {
        Ok(engine) => engine,
        Err(message) => return fail(stderr, &message),
    };
    let (report, _) = scan::run(&engine, &settings, None, &mut |_| Ok(()));
    let out = ScanOut::new(&report, &host.builtin.lenses(), &settings.policy);
    exit_code(scan::write_summary(stdout, &out), stderr)
}

fn scan(
    host: &Host<'_>,
    args: &ScanArgs,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    let roots = match absolute(&args.roots) {
        Ok(roots) => roots,
        Err(error) => return fail(stderr, &format!("--root: {error}")),
    };
    let settings = match host.settings(&roots, stderr) {
        Ok(settings) => settings,
        Err(code) => return code,
    };
    let engine = match host.engine() {
        Ok(engine) => engine,
        Err(message) => return fail(stderr, &message),
    };
    let lenses = host.builtin.lenses();
    let lens = args.lens.map(Lens::from);
    let mut last_seq = None;
    let mut stream = scan::Stream {
        lenses: &lenses,
        policy: &settings.policy,
        found: HashMap::new(),
    };
    let (report, streamed) = scan::run(&engine, &settings, lens, &mut |event| {
        last_seq = Some(event.seq);
        match args.events {
            Some(EventsFormat::Jsonl) => scan::write_event(stdout, &event, &mut stream),
            None => Ok(()),
        }
    });
    let out = ScanOut::new(&report, &lenses, &settings.policy);
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
    host: &Host<'_>,
    target: &str,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    let builtin = host.builtin;
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
    let settings = match host.settings(&[], stderr) {
        Ok(settings) => settings,
        Err(code) => return code,
    };
    let engine = match host.engine() {
        Ok(engine) => engine,
        Err(message) => return fail(stderr, &message),
    };
    let (report, _) = scan::run(&engine, &settings, None, &mut |_| Ok(()));
    let path = PathBuf::from(target);
    let written = explain::write_path(stdout, builtin, &path, &report, &settings.policy)
        .and_then(|()| stdout.flush());
    exit_code(written, stderr)
}

/// `kipple config path|show`: they explain a config that can't be used rather than fail
/// before they can, then exit with code 2.
fn config_command(
    host: &Host<'_>,
    action: ConfigAction,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    let roots = kipple_platform::resolve_roots(host.env, &[]);
    let location = config::locate(host.env, host.config, &roots);
    let loaded = config::load(&location, &config::catalog(host.builtin));
    let written = match action {
        ConfigAction::Path => config::write_path(stdout, &location),
        ConfigAction::Show => config::write_show(stdout, &location, loaded.as_ref().ok()),
    };
    match (exit_code(written, stderr), loaded) {
        (code, Ok(_)) => code,
        (code, Err(_)) if code != ExitCode::SUCCESS => code,
        (_, Err(error)) => config_failed(stderr, &error),
    }
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
