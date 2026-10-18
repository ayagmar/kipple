#![forbid(unsafe_code)]

use std::io::{self, IsTerminal as _};
use std::process::ExitCode;

use clap::Parser as _;
use kipple_platform::Environment;

fn main() -> ExitCode {
    let cli = kipple::Cli::parse();
    // The one read of the environment: everything below works from this snapshot.
    let env = Environment::from_vars(std::env::vars_os());
    // Only a terminal gets the home screen; anything else gets the printed summary.
    let terminal = io::stdout().is_terminal();
    if cli.command.is_none() && terminal {
        return kipple::home(&cli, &env, &mut io::stderr().lock());
    }
    kipple::run(
        &cli,
        &env,
        terminal,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}
