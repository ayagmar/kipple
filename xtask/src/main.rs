//! Developer task runner for the kipple workspace. Run it as `cargo xtask <task>`.
#![forbid(unsafe_code)]

mod arch;
mod check;
mod cmd;
mod gen_docs;

use std::path::Path;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// The workspace root, fixed when xtask is built.
const WORKSPACE_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

#[derive(Debug, Parser)]
#[command(about = "Developer tasks for the kipple workspace")]
struct Args {
    #[command(subcommand)]
    task: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Run the gate. Green here is the only definition of green.
    Check {
        /// Leave out a step that CI runs as its own job.
        #[arg(long, value_enum)]
        skip: Vec<check::Skip>,
    },
    /// Apply clippy's machine-applicable fixes, then rustfmt.
    Fix,
    /// Fail on crate dependencies or OS-specific code that break the architecture.
    CheckArch,
    /// Regenerate the CLI reference.
    GenDocs {
        /// Fail if the committed reference differs from a fresh generation.
        #[arg(long)]
        check: bool,
    },
}

fn main() -> Result<()> {
    let root = Path::new(WORKSPACE_ROOT);
    match Args::parse().task {
        Task::Check { skip } => check::run(root, &skip),
        Task::Fix => check::fix(root),
        Task::CheckArch => arch::run(root),
        Task::GenDocs { check } => gen_docs::run(root, check),
    }
}
