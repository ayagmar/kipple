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
pub mod theme;
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
use crate::setup::{Builtin, BuiltinRule};
use crate::theme::{ColorChoice, Colours, Glyphs, Ink, Stdout, Theme, Themes};

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
    /// Draw with this theme instead of the config's `ui.theme`: a built-in or one of
    /// yours (`kipple theme list`).
    #[arg(long, global = true, value_name = "NAME", value_parser = theme_name)]
    pub theme: Option<String>,
    /// When to use colour. `auto`: when the output is a terminal and `NO_COLOR` isn't set.
    #[arg(long, global = true, value_enum, value_name = "WHEN")]
    pub color: Option<ColorArg>,
    /// The same as `--color never`.
    #[arg(long, global = true, conflicts_with = "color")]
    pub no_color: bool,
    /// The home screen's options.
    #[command(flatten)]
    pub home: HomeArgs,
    /// What to do. Without one, the home screen.
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// What `--color` and `--no-color` say.
    #[must_use]
    pub const fn colour_choice(&self) -> ColorChoice {
        match (self.no_color, self.color) {
            (true, _) | (false, Some(ColorArg::Never)) => ColorChoice::Never,
            (false, Some(ColorArg::Always)) => ColorChoice::Always,
            (false, Some(ColorArg::Auto) | None) => ColorChoice::Auto,
        }
    }
}

/// `--color`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ColorArg {
    /// When the output is a terminal and `NO_COLOR` isn't set.
    Auto,
    /// Always, even when piped or under `NO_COLOR`.
    Always,
    /// Never.
    Never,
}

/// `--theme NAME`, checked as a name: lowercase letters, digits and `-`.
fn theme_name(name: &str) -> Result<String, String> {
    if theme::valid_name(name) {
        Ok(name.to_owned())
    } else {
        Err("a theme name is lowercase letters, digits and -".to_owned())
    }
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
    /// List the themes, print one as a theme file, or show where your own themes go.
    Theme {
        /// What to do.
        #[command(subcommand)]
        action: ThemeAction,
    },
}

/// `kipple theme` actions.
#[derive(Debug, Clone, Subcommand)]
pub enum ThemeAction {
    /// List the built-in themes and yours, with where each comes from.
    List,
    /// Print a theme as a complete theme file, with what it extends written out, to start
    /// your own from.
    Show {
        /// The theme's name.
        #[arg(value_parser = theme_name)]
        name: String,
    },
    /// Print the directory your own themes go in, one `NAME.toml` each.
    Path,
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

/// Runs `cli`, writing data to `stdout` and diagnostics to `stderr`. Human output is in
/// colour when `--color` says so, or by default when `goes_to` is a terminal.
pub fn run(
    cli: &Cli,
    env: &Environment,
    goes_to: Stdout,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    // Clap can't exempt the global `--config` from a conflict with the commands, so the
    // home screen's own option is refused here.
    if cli.command.is_some() && cli.home.ascii {
        say(stderr, "--ascii is for the home screen, not a command");
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
        theme: cli.theme.as_deref(),
        colours: Colours::detect(cli.colour_choice(), env, goes_to),
    };
    match &cli.command {
        None => summary(&host, stdout, stderr),
        Some(Command::Scan(args)) => scan(&host, args, stdout, stderr),
        Some(Command::Rules { json }) => {
            let rules = rules::RulesOut::new(&builtin);
            let written = if *json {
                write_json(stdout, &rules)
            } else {
                rules::write_human(stdout, &rules, &host.ink(stderr))
            };
            exit_code(written, stderr)
        }
        Some(Command::Explain { target }) => explain(&host, target, stdout, stderr),
        Some(Command::Config { action }) => config_command(&host, *action, stdout, stderr),
        Some(Command::Theme { action }) => theme_command(&host, action, stdout, stderr),
        Some(Command::Doctor { json }) => {
            let report = doctor::gather(env, host.config, &builtin);
            let written = if *json {
                write_json(stdout, &report.to_json())
            } else {
                let ink = host.ink(stderr);
                report
                    .write_to(stdout, &ink)
                    .and_then(|()| doctor::write_themes(stdout, &ink, &host.theme_check()))
            };
            exit_code(written, stderr)
        }
    }
}

/// Opens the home screen in this terminal (`docs/03-functional-spec.md` §2 J1).
///
/// It needs a config that can be used, and a theme that can: `--theme`, else the
/// config's `ui.theme`, else `kipple`. Colour follows `--color`, then `NO_COLOR`.
pub fn home(cli: &Cli, env: &Environment, stderr: &mut dyn Write) -> ExitCode {
    let builtin = match Builtin::load() {
        Ok(builtin) => builtin,
        Err(message) => return fail(stderr, &message),
    };
    let host = Host {
        builtin: &builtin,
        env,
        config: cli.config.as_deref(),
        theme: cli.theme.as_deref(),
        colours: Colours::detect(cli.colour_choice(), env, Stdout::Terminal),
    };
    let settings = match host.settings(&[], stderr) {
        Ok(settings) => settings,
        Err(code) => return code,
    };
    let glyphs = if cli.home.ascii {
        Glyphs::Ascii
    } else {
        settings.loaded.config.ui.glyphs
    };
    let active = host.theme_name(Some(&settings.loaded.config));
    let catalog = Themes::new(env).catalog();
    let appearance = match tui::Appearance::new(catalog, &active, host.colours, glyphs) {
        Ok(appearance) => appearance,
        Err(error) => {
            say(stderr, &format!("theme {active}: {error}"));
            return ExitCode::from(USAGE);
        }
    };
    match tui::open(env, &settings, appearance) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(stderr, &format!("the home screen failed: {error}")),
    }
}

/// What every command runs with: the built-in rules, the environment snapshot, the
/// config file `--config` names, if any, and how human output is drawn.
#[derive(Debug, Clone, Copy)]
struct Host<'cli> {
    builtin: &'cli Builtin,
    env: &'cli Environment,
    config: Option<&'cli Path>,
    /// `--theme`.
    theme: Option<&'cli str>,
    /// Whether human output is in colour, and in how many.
    colours: Colours,
}

impl Host<'_> {
    /// The theme to draw: `--theme`, else `ui.theme` of `config`, else `kipple`.
    fn theme_name(&self, config: Option<&config::Config>) -> String {
        self.theme
            .map(str::to_owned)
            .or_else(|| config.and_then(|config| config.ui.theme.clone()))
            .unwrap_or_else(|| theme::DEFAULT.to_owned())
    }

    /// The config as far as it can be read, for its theme only: a config that can't be
    /// used is the business of the commands that need one.
    fn config_for_theme(&self) -> Option<config::Config> {
        let roots = kipple_platform::resolve_roots(self.env, &[]);
        let location = config::locate(self.env, self.config, &roots);
        config::load(&location, &config::catalog(self.builtin))
            .ok()
            .map(|loaded| loaded.config)
    }

    /// How human output paints its roles. Without colour it reads no theme at all. A
    /// theme that can't be used is warned about once, and `kipple` is used instead.
    fn ink(&self, stderr: &mut dyn Write) -> Ink {
        if self.colours == Colours::Off {
            return Ink::plain();
        }
        let name = self.theme_name(self.config_for_theme().as_ref());
        let theme = Themes::new(self.env)
            .resolve(&name)
            .unwrap_or_else(|error| {
                let using = theme::DEFAULT;
                say(
                    stderr,
                    &format!("warning: theme {name}: {error}; using {using}"),
                );
                Theme::builtin(using).unwrap_or_default()
            });
        Ink::new(theme.drawn(self.colours))
    }

    /// What `doctor` says about themes: the one drawn and every one that can't be used.
    fn theme_check(&self) -> doctor::ThemeCheck {
        let themes = Themes::new(self.env);
        doctor::ThemeCheck {
            active: self.theme_name(self.config_for_theme().as_ref()),
            dir: themes.dir().map(Path::to_owned).map_err(str::to_owned),
            catalog: themes.catalog(),
        }
    }

    /// The config and roots a scan runs with, the project roots `given` on the command
    /// line, if any. A config that can't be used is reported on `stderr` with exit code
    /// 2; each kept or excluded path that is missing is warned about.
    fn settings(&self, given: &[PathBuf], stderr: &mut dyn Write) -> Result<Settings, ExitCode> {
        let catalog = config::catalog(self.builtin);
        match Settings::load(self.env, self.config, given, &catalog) {
            Ok(settings) => {
                for warning in &settings.warnings {
                    say(stderr, &format!("warning: {warning}"));
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
    say(stderr, &error.to_string());
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
    let report = scan::run(&engine, &settings, None, &mut |_| Ok(())).report;
    let out = ScanOut::new(&report, &host.builtin.lenses(), &settings.policy);
    exit_code(scan::write_summary(stdout, &out, &host.ink(stderr)), stderr)
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
    let scan::Scanned { report, streamed } = scan::run(&engine, &settings, lens, &mut |event| {
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
        None => scan::write_human(stdout, &out, &host.ink(stderr)),
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
    if let Some(BuiltinRule { pack, rule }) = explain::find_rule(builtin, target) {
        let ink = host.ink(stderr);
        let written =
            explain::write_rule(stdout, &ink, builtin, pack, rule).and_then(|()| stdout.flush());
        return exit_code(written, stderr);
    }
    if target.starts_with("builtin.") {
        say(
            stderr,
            &format!("there is no rule {target}; `kipple rules` lists them"),
        );
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
    let report = scan::run(&engine, &settings, None, &mut |_| Ok(())).report;
    let path = PathBuf::from(target);
    let ink = host.ink(stderr);
    let judged = explain::Judged {
        report: &report,
        policy: &settings.policy,
    };
    let written =
        explain::write_path(stdout, &ink, builtin, &path, judged).and_then(|()| stdout.flush());
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

/// `kipple theme list|show|path`. `path` reads no theme; `show` exits 2 when the theme
/// can't be used.
fn theme_command(
    host: &Host<'_>,
    action: &ThemeAction,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    let themes = Themes::new(host.env);
    let written = match action {
        ThemeAction::Path => match themes.dir() {
            Ok(dir) => writeln!(stdout, "{}", human::escaped(&dir.display().to_string()))
                .and_then(|()| stdout.flush()),
            Err(why) => {
                say(stderr, why);
                return ExitCode::from(USAGE);
            }
        },
        ThemeAction::List => {
            let catalog = themes.catalog();
            if let (Some(problem), Ok(dir)) = (&catalog.problem, themes.dir()) {
                say(stderr, &format!("warning: {} {problem}", dir.display()));
            }
            theme::write_list(stdout, &catalog)
        }
        ThemeAction::Show { name } => match themes.resolve(name) {
            Ok(theme) => {
                let comment = format!(
                    "The theme {name}, with what it extends written out. Save it as\n\
                     NAME.toml in `kipple theme path` and change what you like."
                );
                stdout
                    .write_all(theme.to_toml(&comment).as_bytes())
                    .and_then(|()| stdout.flush())
            }
            Err(error) => {
                say(stderr, &format!("theme {name}: {error}"));
                return ExitCode::from(USAGE);
            }
        },
    };
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
    say(stderr, message);
    ExitCode::FAILURE
}

/// Writes one diagnostic line on `stderr`, escaped: config and theme errors, warnings and
/// OS messages carry names and paths from files, which may hold control characters.
fn say(stderr: &mut dyn Write, message: &str) {
    writeln!(stderr, "kipple: {}", human::escaped(message)).ok();
}

/// The exit code for a command whose output was written, or failed to be. A closed stdout
/// is a cancel: exit 130 and say nothing more.
pub fn exit_code(written: io::Result<()>, stderr: &mut dyn Write) -> ExitCode {
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::from(CANCELLED),
        Err(error) => {
            // Nothing else can be reported if stderr fails too.
            say(stderr, &format!("could not write the output: {error}"));
            ExitCode::FAILURE
        }
    }
}
