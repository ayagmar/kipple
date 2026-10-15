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

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::SystemTime;

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
    let now = demo_home::looked_at();
    demo_home::tree(now).build_in(temp.path()).unwrap();
    let mut file = String::new();
    for (name, relative) in demo_home::ROOTS {
        writeln!(file, "{name} = {}", temp.path().join(relative).display()).unwrap();
    }
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    writeln!(file, "now = {seconds}").unwrap();
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
fn balanced_puts_at_least_80_percent_of_regenerable_bytes_of_the_demo_home_in_safe() {
    let (_temp, env) = demo();

    let (code, json, _) = kipple(&env, &["scan", "--json"]);

    assert_eq!(code, ExitCode::SUCCESS);
    let report: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(report["level"], "balanced");
    let (mut regenerable, mut safe) = (0, 0);
    let mut buckets = BTreeMap::<&str, u64>::new();
    for finding in report["findings"].as_array().unwrap() {
        let bytes = finding.pointer("/size/apparent").unwrap().as_u64().unwrap();
        let bucket = finding
            .pointer("/verdict/bucket")
            .unwrap()
            .as_str()
            .unwrap();
        *buckets.entry(bucket).or_default() += bytes;
        let recovery = finding.pointer("/consequence/recovery");
        let comes_back = ["rebuild-local", "reinstall", "redownload"]
            .iter()
            .any(|kind| recovery == Some(&Value::from(*kind)));
        // Where the OS doesn't show the process paths a layout needs (Windows), it is
        // always Protected (04 §4), so the target covers only what can be judged.
        let judged = finding.pointer("/activity/reason") != Some(&Value::from("unsupported"));
        if !comes_back || !judged {
            continue;
        }
        regenerable += bytes;
        if bucket == "safe" {
            safe += bytes;
        }
    }
    assert!(regenerable > 0, "{json}");
    assert!(
        safe * 10 >= regenerable * 8,
        "only {safe} of {regenerable} regenerable bytes are Safe; bytes by bucket: \
         {buckets:?}. If every finding is held as unknown-git, the temp dir \
         (TMPDIR) is probably inside a git repository. {json}"
    );
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

/// The paths of the findings in a `--json` report.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn finding_paths(json: &str) -> Vec<String> {
    let report: Value = serde_json::from_str(json).unwrap();
    report
        .get("findings")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|f| {
            f.pointer("/path/display")
                .and_then(Value::as_str)
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[test]
fn a_project_root_given_twice_or_through_its_parent_finds_each_artifact_once() {
    let (temp, env) = demo();
    let projects = temp.path().join("home/projects");
    let via_parent = projects.join("clients/..");
    let nested = projects.join("clients");

    let (_, alone, _) = kipple(
        &env,
        &[
            "scan",
            "--json",
            "--lens",
            "dev",
            "--root",
            projects.to_str().unwrap(),
        ],
    );
    let given = [&projects, &via_parent, &nested].map(|p| p.to_str().unwrap().to_owned());
    let mut args = vec!["scan", "--json", "--lens", "dev"];
    for root in &given {
        args.extend(["--root", root]);
    }
    let (code, repeated, _) = kipple(&env, &args);
    let (_, parent, _) = kipple(
        &env,
        &["scan", "--json", "--lens", "dev", "--root", &given[1]],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(finding_paths(&alone).len(), 8, "{alone}");
    assert_eq!(finding_paths(&repeated), finding_paths(&alone));
    assert_eq!(finding_paths(&parent), finding_paths(&alone));
}

#[test]
#[cfg(unix)]
fn explaining_a_path_the_scan_couldnt_read_says_so_instead_of_that_nothing_is_there() {
    use std::os::unix::fs::PermissionsExt as _;

    let (temp, env) = demo();
    let project = temp.path().join("home/projects/rust-app");
    fs::set_permissions(&project, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&project).is_ok() {
        // Permissions don't bind root, so there is nothing unreadable to test.
        return;
    }

    let target = project.join("target");
    let (code, out, _) = kipple(&env, &["explain", target.to_str().unwrap()]);
    fs::set_permissions(&project, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("kipple found nothing at"), "{out}");
    assert!(out.contains("couldn't cover everything"), "{out}");
    assert!(out.contains("rust-app"), "{out}");
}

#[test]
fn kipple_without_a_terminal_prints_each_buckets_total_instead_of_the_home_screen() {
    let (_temp, env) = demo();

    let (code, out, err) = kipple(&env, &[]);

    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(err, "");
    assert!(!out.contains('\u{1b}'), "terminal control in {out}");
    let line = |title: &str| {
        out.lines()
            .find(|line| line.trim_start().starts_with(title))
            .unwrap_or_else(|| panic!("no {title} line in {out}"))
            .to_owned()
    };
    // The summary counts what `scan --json` puts in each bucket.
    let (_, json, _) = kipple(&env, &["scan", "--json"]);
    let report: Value = serde_json::from_str(&json).unwrap();
    let count = |bucket: &str| {
        let n = report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f.pointer("/verdict/bucket") == Some(&Value::from(bucket)))
            .count();
        if n == 1 {
            "1 finding".to_owned()
        } else {
            format!("{n} findings")
        }
    };
    assert!(line("Safe to clean").ends_with(&count("safe")), "{out}");
    assert!(line("Review").ends_with(&count("review")), "{out}");
    assert!(line("Protected").contains("finding"), "{out}");
    assert!(out.contains("Nothing was changed."), "{out}");
}

#[test]
fn a_venv_holding_an_editable_checkout_is_protected_as_holds_repository() {
    // `pip install -e git+…` clones the package into the venv: removing the venv would
    // take the checkout and any work in it (04 §10).
    let (temp, env) = demo();
    let mut tree = tree::FixtureTree::new();
    venv_v3_14_8::project(&mut tree, "home/projects/editable");
    tree.file(
        "home/projects/editable/.venv/src/pkg/.git/HEAD",
        b"ref: refs/heads/main\n",
    );
    tree.build_in(temp.path()).unwrap();
    let project = temp.path().join("home/projects/editable");
    // `--root` resolves its parent (D-045), so the reported path may differ from the
    // temp path by more than its spelling (`/var` is `/private/var` on macOS).
    let venv_tail = Path::new("projects").join("editable").join(".venv");

    let (code, json, _) = kipple(
        &env,
        &["scan", "--json", "--root", project.to_str().unwrap()],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    let report: Value = serde_json::from_str(&json).unwrap();
    let venv = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| {
            f["path"]["display"]
                .as_str()
                .is_some_and(|p| Path::new(p).ends_with(&venv_tail))
        })
        .unwrap_or_else(|| panic!("no .venv finding in {json}"));
    assert_eq!(venv["verdict"]["bucket"], "protected", "{venv}");
    let reasons = venv["verdict"]["reasons"].as_array().unwrap();
    assert!(reasons.contains(&Value::from("holds-repository")), "{venv}");
}
