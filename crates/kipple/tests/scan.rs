//! `kipple scan` and `kipple explain` end to end on `demo-home`, every root redirected
//! through the hidden test-roots override (`docs/03-functional-spec.md` §3 and §4): the
//! event stream's order and closing record, the broken-pipe contract, and explanations.
//! Built only with the `test-roots` feature, which `cargo xtask verify-demo` turns on.

#[path = "../../kipple-adapters/tests/fixtures/tree.rs"]
mod tree;

#[path = "../../kipple-adapters/tests/fixtures/node/bun_v1_4_2.rs"]
mod bun_v1_4_2;
#[path = "../../kipple-adapters/tests/fixtures/cargo/v1_99_0.rs"]
mod cargo_v1_99_0;
#[path = "../../kipple-adapters/tests/fixtures/claude-code/settings.rs"]
mod claude_settings;
#[path = "../../kipple-adapters/tests/fixtures/demo_home.rs"]
mod demo_home;
#[path = "../../kipple-adapters/tests/fixtures/journald/v262.rs"]
mod journald_v262;
#[path = "../../kipple-adapters/tests/fixtures/node/npm_v11_19_0.rs"]
mod npm_v11_19_0;
#[path = "../../kipple-adapters/tests/fixtures/pacman/v7_1_0.rs"]
mod pacman_v7_1_0;
#[path = "../../kipple-adapters/tests/fixtures/node/pnpm_v12_9_1.rs"]
mod pnpm_v12_9_1;
#[path = "../../kipple-adapters/tests/fixtures/trash/freedesktop_v1_0.rs"]
mod trash_v1_0;
#[path = "../../kipple-adapters/tests/fixtures/python/uv_v0_12_23.rs"]
mod uv_v0_12_23;
#[path = "../../kipple-adapters/tests/fixtures/python/venv_v3_14_8.rs"]
mod venv_v3_14_8;

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser as _;
use kipple::{CANCELLED, Cli, USAGE};
use kipple_platform::Environment;
use serde_json::Value;

/// `demo-home` in a temp dir, and an environment whose roots all point into it.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn demo() -> (tempfile::TempDir, Environment) {
    let temp = tempfile::tempdir().unwrap();
    demo_home::tree().build_in(temp.path()).unwrap();
    let mut file = String::new();
    for (name, relative) in demo_home::ROOTS {
        writeln!(file, "{name} = {}", temp.path().join(relative).display()).unwrap();
    }
    let roots = temp.path().join("roots.txt");
    fs::write(&roots, file).unwrap();
    let env =
        Environment::from_vars([(OsString::from("KIPPLE_TEST_ROOTS"), roots.into_os_string())]);
    (temp, env)
}

/// Runs `kipple <args>` and returns its exit code, stdout and stderr.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn kipple(env: &Environment, args: &[&str]) -> (ExitCode, String, String) {
    let cli = Cli::parse_from(std::iter::once("kipple").chain(args.iter().copied()));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = kipple::run(&cli, env, &mut out, &mut err);
    let text = |bytes| String::from_utf8(bytes).unwrap();
    (code, text(out), text(err))
}

/// A stdout whose reader has gone, as with `kipple scan --events jsonl | head -1`.
struct ClosedPipe;

impl Write for ClosedPipe {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
}

#[test]
fn a_closed_stdout_cancels_the_event_stream_and_exits_130_saying_nothing_more() {
    let (_temp, env) = demo();
    let cli = Cli::parse_from(["kipple", "scan", "--events", "jsonl"]);
    let mut stderr = Vec::new();

    let code = kipple::run(&cli, &env, &mut ClosedPipe, &mut stderr);

    assert_eq!(code, ExitCode::from(CANCELLED));
    assert_eq!(String::from_utf8(stderr).unwrap(), "");
}

#[test]
fn the_event_stream_is_numbered_in_emission_order_and_ends_with_the_sorted_report() {
    let (_temp, env) = demo();

    let (code, out, _) = kipple(&env, &["scan", "--events", "jsonl"]);
    let (_, json, _) = kipple(&env, &["scan", "--json"]);

    assert_eq!(code, ExitCode::SUCCESS);
    let records: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    for (n, record) in records.iter().enumerate() {
        assert_eq!(record["schema"], "kipple.event/v1");
        assert_eq!(record["seq"], n);
    }
    let types: Vec<&str> = records
        .iter()
        .map(|r| r["type"].as_str().unwrap())
        .collect();
    assert_eq!(types.first(), Some(&"scan-started"));
    assert_eq!(types.get(types.len() - 2), Some(&"scan-completed"));
    let mut added = Vec::new();
    for record in &records {
        match record["type"].as_str() {
            Some("finding-added") => added.push(record["id"].clone()),
            Some("finding-updated") => assert!(added.contains(&record["id"]), "sized before added"),
            _ => {}
        }
    }
    let last = records.last().unwrap();
    assert_eq!(last["type"], "completed");
    let report: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(last["report"]["findings"], report["findings"]);
    assert_eq!(added.len(), report["findings"].as_array().unwrap().len());
}

#[test]
fn a_lens_scans_only_its_own_integrations() {
    let (_temp, env) = demo();

    let (_, json, _) = kipple(&env, &["scan", "--json", "--lens", "agents"]);

    let report: Value = serde_json::from_str(&json).unwrap();
    let ran: Vec<&str> = report["integrations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ran, ["claude-code"]);
    assert_eq!(report["findings"], Value::Array(Vec::new()));
}

#[test]
fn explaining_a_found_path_shows_its_evidence_and_an_unknown_rule_is_a_usage_error() {
    let (temp, env) = demo();
    let target = temp.path().join("home/projects/rust-app/target");

    let (code, out, _) = kipple(&env, &["explain", target.to_str().unwrap()]);
    let (unknown, _, err) = kipple(&env, &["explain", "builtin.cargo.nope"]);

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(
        out.contains("CACHEDIR.TAG inside it, which its tool writes"),
        "{out}"
    );
    assert!(
        out.contains("builtin.cargo.target: Cargo build output"),
        "{out}"
    );
    assert_eq!(unknown, ExitCode::from(USAGE));
    assert!(err.contains("builtin.cargo.nope"), "{err}");
}
