#![forbid(unsafe_code)]

use std::io;
use std::process::ExitCode;

use clap::Parser as _;
use kipple_platform::Environment;

fn main() -> ExitCode {
    let cli = kipple::Cli::parse();
    // The one read of the environment: everything below works from this snapshot.
    let env = Environment::from_vars(std::env::vars_os());
    kipple::run(
        &cli,
        &env,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}
