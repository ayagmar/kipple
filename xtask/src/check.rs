//! The gate (`docs/08-engineering-standards.md` §1) and `fix`.

use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use clap::ValueEnum;
use serde::Deserialize;
use serde_json::Value;

use crate::{arch, cmd, compat, docs_cmds, gen_docs, style};

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
        name: "lint (all features)",
        program: "cargo",
        args: &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
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

/// Test targets the run must hold. `kipple-core`'s apply tests need a feature only another
/// crate's dev-dependency turns on, and a target whose `required-features` are off drops out
/// of the run without a word.
const REQUIRED_TEST_TARGETS: &[&str] = &["kipple-core::apply"];

/// What `cargo nextest list --message-format json` says the run holds.
#[derive(Debug, Deserialize)]
struct TestList {
    #[serde(rename = "rust-suites")]
    suites: BTreeMap<String, Suite>,
}

/// A test target. One the run's filter leaves out is listed with no test cases.
#[derive(Debug, Deserialize)]
struct Suite {
    testcases: BTreeMap<String, Value>,
}

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
    announce("required test targets")?;
    required_test_targets(root)?;
    announce("architecture")?;
    arch::run(root)?;
    announce("generated docs")?;
    gen_docs::run(root, gen_docs::Committed::Check)?;
    announce("compatibility")?;
    compat::run(root, compat::Baselines::Keep)?;
    announce("documented commands")?;
    docs_cmds::run(root)?;
    announce("style")?;
    style::run(root)?;
    announce("all checks passed")
}

/// Fails unless the workspace test run holds every one of [`REQUIRED_TEST_TARGETS`]. The
/// test step has already built what this lists.
fn required_test_targets(root: &Path) -> Result<()> {
    let mut command = cmd::command("cargo", root);
    command.args(["nextest", "list", "--workspace", "--locked"]);
    command.args(["--message-format", "json"]);
    let json = cmd::stdout(&mut command)?;
    let list = serde_json::from_slice(&json).context("could not parse `cargo nextest list`")?;
    let missing = missing_targets(&list);
    if !missing.is_empty() {
        bail!(
            "the workspace test run doesn't run {}; see `required-features` in its crate's \
             Cargo.toml",
            missing.join(", ")
        );
    }
    Ok(())
}

/// Those of [`REQUIRED_TEST_TARGETS`] with no test in `list`.
fn missing_targets(list: &TestList) -> Vec<&'static str> {
    REQUIRED_TEST_TARGETS
        .iter()
        .copied()
        .filter(|target| {
            list.suites
                .get(*target)
                .is_none_or(|suite| suite.testcases.is_empty())
        })
        .collect()
}

/// The machine xtask runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Host {
    /// A CI runner or a throwaway VM: `KIPPLE_TEST_DISPOSABLE_HOST=1`.
    Disposable,
    /// Anything else, such as someone's own machine.
    Precious,
}

/// Runs the `native` nextest profile, which reads real OS state (the process table,
/// Known Folders). Only on a disposable host: a CI runner or a throwaway VM.
pub(crate) fn test_native(root: &Path, host: Host) -> Result<()> {
    if host != Host::Disposable {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn missing(json: &str) -> Vec<&'static str> {
        missing_targets(&serde_json::from_str(json).unwrap())
    }

    /// Catches the check reading a target as run when `required-features` took it out of the
    /// list, or the run's filter left it there empty, which is how nextest shows that.
    #[test]
    fn a_required_target_counts_only_with_a_test_in_the_run() {
        let running = r#"{"rust-suites": {"kipple-core::apply": {"testcases": {"t": {}}}}}"#;
        let filtered = r#"{"rust-suites": {"kipple-core::apply": {"testcases": {}}}}"#;
        let absent = r#"{"rust-suites": {"kipple-core": {"testcases": {"t": {}}}}}"#;

        assert_eq!(missing(running), [] as [&str; 0]);
        assert_eq!(missing(filtered), ["kipple-core::apply"]);
        assert_eq!(missing(absent), ["kipple-core::apply"]);
    }
}
