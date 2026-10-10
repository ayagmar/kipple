//! Launching cargo and the gate tools.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// A command for `program` that runs in the workspace root.
pub(crate) fn command(program: &str, root: &Path) -> Command {
    #[expect(
        clippy::disallowed_methods,
        reason = "xtask is the developer task runner; launching cargo and the gate tools is its job"
    )]
    let mut cmd = Command::new(program);
    cmd.current_dir(root);
    cmd
}

/// Runs `cmd` with inherited stdio and fails unless it exits successfully.
pub(crate) fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd
        .status()
        .with_context(|| format!("could not start `{}`", describe(cmd)))?;
    if !status.success() {
        bail!("`{}` failed ({status})", describe(cmd));
    }
    Ok(())
}

/// Runs `cmd`, passing stderr through, and returns its stdout.
pub(crate) fn stdout(cmd: &mut Command) -> Result<Vec<u8>> {
    let output = cmd
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("could not start `{}`", describe(cmd)))?;
    if !output.status.success() {
        bail!("`{}` failed ({})", describe(cmd), output.status);
    }
    Ok(output.stdout)
}

fn describe(cmd: &Command) -> String {
    std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}
