//! The gate (`docs/08-engineering-standards.md` §1) and `fix`.

use std::io::{self, Write as _};
use std::path::Path;

use anyhow::{Result, bail};
use clap::ValueEnum;

use crate::{arch, cmd, gen_docs};

/// Gate steps that may be left out because CI runs them as their own job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Skip {
    /// `cargo deny check`. Its advisory database changes over time, so CI runs it separately.
    Deny,
}

/// One gate step that runs an external tool.
struct Step {
    name: &'static str,
    program: &'static str,
    args: &'static [&'static str],
    env: &'static [(&'static str, &'static str)],
    skip: Option<Skip>,
}

const STEPS: &[Step] = &[
    Step {
        name: "format",
        program: "cargo",
        args: &["fmt", "--all", "--check"],
        env: &[],
        skip: None,
    },
    Step {
        name: "lint",
        program: "cargo",
        args: &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
        env: &[],
        skip: None,
    },
    Step {
        name: "rustdoc",
        program: "cargo",
        args: &["doc", "--workspace", "--no-deps", "--locked"],
        env: &[("RUSTDOCFLAGS", "-D warnings")],
        skip: None,
    },
    Step {
        name: "tests",
        program: "cargo",
        args: &["nextest", "run", "--workspace", "--locked"],
        env: &[],
        skip: None,
    },
    Step {
        name: "doctests",
        program: "cargo",
        args: &["test", "--doc", "--workspace", "--locked"],
        env: &[],
        skip: None,
    },
    Step {
        name: "supply chain",
        program: "cargo",
        args: &["deny", "check"],
        env: &[],
        skip: Some(Skip::Deny),
    },
    Step {
        name: "unused deps",
        // Run directly: launched as `cargo machete` from inside `cargo run`, it takes its
        // own subcommand name for a directory to scan.
        program: "cargo-machete",
        args: &[],
        env: &[],
        skip: None,
    },
    Step {
        name: "spelling",
        program: "typos",
        args: &[],
        env: &[],
        skip: None,
    },
];

/// Runs every gate step in order and stops at the first failure.
pub(crate) fn run(root: &Path, skip: &[Skip]) -> Result<()> {
    for step in STEPS {
        if step.skip.is_some_and(|s| skip.contains(&s)) {
            announce(&format!("{} (skipped)", step.name))?;
            continue;
        }
        announce(step.name)?;
        let mut command = cmd::command(step.program, root);
        command.args(step.args).envs(step.env.iter().copied());
        cmd::run(&mut command)?;
    }
    announce("architecture")?;
    arch::run(root)?;
    announce("generated docs")?;
    gen_docs::run(root, true)?;
    announce("all checks passed")
}

/// Runs the `native` nextest profile, which reads real OS state (the process table,
/// Known Folders). Only on a disposable host: a CI runner or a throwaway VM.
pub(crate) fn test_native(root: &Path, disposable_host: bool) -> Result<()> {
    if !disposable_host {
        bail!(
            "the native tests read real OS state; run them only on a disposable host \
             (a CI runner or a throwaway VM) with KIPPLE_TEST_DISPOSABLE_HOST=1"
        );
    }
    cmd::run(cmd::command("cargo", root).args([
        "nextest",
        "run",
        "--workspace",
        "--locked",
        "--profile",
        "native",
    ]))
}

/// Applies clippy's machine-applicable fixes, then formats.
pub(crate) fn fix(root: &Path) -> Result<()> {
    cmd::run(cmd::command("cargo", root).args([
        "clippy",
        "--fix",
        "--allow-dirty",
        "--allow-staged",
        "--workspace",
        "--all-targets",
        "--locked",
    ]))?;
    cmd::run(cmd::command("cargo", root).args(["fmt", "--all"]))
}

fn announce(message: &str) -> Result<()> {
    writeln!(io::stderr(), "xtask: {message}")?;
    Ok(())
}
