//! The `kipple` command-line interface.
//!
//! The binary is the composition root. This library target exists so that
//! `cargo xtask gen-docs` can render the CLI reference from the same clap definition.
#![forbid(unsafe_code)]

use clap::Parser;

/// Command-line arguments for `kipple`.
#[derive(Debug, Parser)]
#[command(name = "kipple", version, about)]
pub struct Cli {}
