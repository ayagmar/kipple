//! Developer task runner for the kipple workspace. Run it as `cargo xtask <task>`.
#![forbid(unsafe_code)]

mod arch;
mod bench_tree;
mod check;
mod cmd;
mod demo;
mod gen_docs;

// The demo tree, built from the same fixtures as the integration tests.
#[path = "../../crates/kipple-adapters/tests/fixtures/node/bun_v1_4_2.rs"]
mod bun_v1_4_2;
#[path = "../../crates/kipple-adapters/tests/fixtures/cargo/v1_99_0.rs"]
mod cargo_v1_99_0;
#[path = "../../crates/kipple-adapters/tests/fixtures/claude-code/settings.rs"]
mod claude_settings;
#[path = "../../crates/kipple-adapters/tests/fixtures/demo_home.rs"]
mod demo_home;
#[path = "../../crates/kipple-adapters/tests/fixtures/journald/v262.rs"]
mod journald_v262;
#[path = "../../crates/kipple-adapters/tests/fixtures/node/npm_v11_19_0.rs"]
mod npm_v11_19_0;
#[path = "../../crates/kipple-adapters/tests/fixtures/pacman/v7_1_0.rs"]
mod pacman_v7_1_0;
#[path = "../../crates/kipple-adapters/tests/fixtures/node/pnpm_v12_9_1.rs"]
mod pnpm_v12_9_1;
#[path = "../../crates/kipple-adapters/tests/fixtures/trash/freedesktop_v1_0.rs"]
mod trash_v1_0;
#[path = "../../crates/kipple-adapters/tests/fixtures/tree.rs"]
mod tree;
#[path = "../../crates/kipple-adapters/tests/fixtures/python/uv_v0_12_23.rs"]
mod uv_v0_12_23;
#[path = "../../crates/kipple-adapters/tests/fixtures/python/venv_v3_14_8.rs"]
mod venv_v3_14_8;

use std::path::{Path, PathBuf};

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
    /// Build a benchmark reference tree. The references are 100000 and 1000000 entries.
    BenchTree {
        /// Where to build it. Must be missing or empty.
        dir: PathBuf,
        /// Files and directories to create.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        entries: u64,
    },
    /// Run the native-backend tests (the `native` nextest profile). They read real OS
    /// state, so they refuse to run unless `KIPPLE_TEST_DISPOSABLE_HOST=1`.
    TestNative,
    /// Build the shared demo tree, plus the `roots.txt` that points every root into it.
    DemoHome {
        /// Where to build it. Must be missing or empty.
        dir: PathBuf,
    },
    /// Scan demo-home with the `test-roots` build and compare with the committed snapshot.
    VerifyDemo,
    /// Regenerate the CLI reference, the JSON schemas and the support matrix.
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
        Task::BenchTree { dir, entries } => bench_tree::run(&dir, entries),
        Task::TestNative => {
            let disposable =
                std::env::var_os("KIPPLE_TEST_DISPOSABLE_HOST").is_some_and(|v| v == "1");
            check::test_native(root, disposable)
        }
        Task::DemoHome { dir } => demo::build(&dir),
        Task::VerifyDemo => demo::verify(root),
        Task::GenDocs { check } => gen_docs::run(root, check),
    }
}
