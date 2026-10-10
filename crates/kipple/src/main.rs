#![forbid(unsafe_code)]

use clap::Parser as _;

fn main() {
    kipple::Cli::parse();
}
