#![forbid(unsafe_code)]

use std::io::{self, IsTerminal as _};
use std::process::ExitCode;

use clap::Parser as _;
use kipple::theme::Stdout;
use kipple::{Io, Person};
use kipple_core::CancelToken;
use kipple_platform::Environment;

fn main() -> ExitCode {
    let cli = kipple::Cli::parse();
    // The one read of the environment: everything below works from this snapshot.
    let env = Environment::from_vars(std::env::vars_os());
    // Only a terminal gets the home screen; anything else gets the printed summary.
    let stdout = if io::stdout().is_terminal() {
        Stdout::Terminal
    } else {
        Stdout::Redirected
    };
    if cli.command.is_none() && stdout == Stdout::Terminal {
        return kipple::home(&cli, &env, &mut io::stderr().lock());
    }
    // A question goes to stderr and its answer comes from stdin: both must be a terminal.
    let seen = if io::stdin().is_terminal() && io::stderr().is_terminal() {
        Person::AtTerminal
    } else {
        Person::Absent
    };
    let cancel = CancelToken::new();
    if cli.mutates()
        && let Err(error) = kipple_platform::cancel_on_interrupt(&cancel)
    {
        kipple::warn_no_interrupt(&mut io::stderr().lock(), &error);
    }
    kipple::run(
        &cli,
        &env,
        Io {
            goes_to: stdout,
            stdin: Box::new(io::stdin().lock()),
            stdout: &mut io::stdout().lock(),
            stderr: &mut io::stderr().lock(),
            person: kipple::person(&env, seen),
            cancel,
        },
    )
}
