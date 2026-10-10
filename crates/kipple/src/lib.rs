//! The `kipple` command-line interface.
//!
//! The binary is the composition root: it captures the environment once and wires the
//! platform into each command. This library target also lets `cargo xtask gen-docs`
//! render the CLI reference from the same clap definition.
#![forbid(unsafe_code)]

pub mod doctor;

use std::io::{self, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use kipple_platform::Environment;

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
    /// Show where kipple looks, what it can read and which tools it found. Read-only.
    Doctor,
}

/// The exit code when stdout closes before the output is written (`docs/03-functional-
/// spec.md` §4): the same as a cancel.
pub const CANCELLED: u8 = 130;

/// Runs `cli`, writing data to `stdout` and diagnostics to `stderr`.
pub fn run(
    cli: &Cli,
    env: &Environment,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> ExitCode {
    let written = match cli.command {
        Command::Doctor => doctor::gather(env).write_to(stdout),
    };
    exit_code(written, stderr)
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
