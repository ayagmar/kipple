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

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::SystemTime;

use clap::Parser as _;
use kipple::{CANCELLED, Cli, Io, USAGE};
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
    let code = kipple::run(&cli, env, Io::redirected(&mut out, &mut err));
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

    let code = kipple::run(&cli, &env, Io::redirected(&mut ClosedPipe, &mut stderr));

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
        // Windows doesn't show the process paths an interpreted layout needs, so there
        // it is always Protected (04 §4) and the target covers only the rest.
        let judged = kipple_platform::PLATFORM != kipple_core::Platform::Windows
            || finding.pointer("/activity/reason") != Some(&Value::from("unsupported"));
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
    assert_eq!(ran, ["claude-code", "codex", "cursor", "git", "pi"]);
    let lenses: BTreeSet<&str> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["lens"].as_str().unwrap())
        .collect();
    assert_eq!(lenses, BTreeSet::from(["agents"]), "{json}");
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

// Catches an unestimated restore cost reading as the bare word "unknown" in explain's
// text, as if kipple had failed to check something, and the readable wording leaking
// into the JSON contract.
#[test]
fn explain_says_an_unknown_restore_cost_is_not_estimated_and_the_json_keeps_unknown() {
    let (temp, env) = demo();
    let target = temp.path().join("home/projects/rust-app/target");

    let (code, out, _) = kipple(&env, &["explain", target.to_str().unwrap()]);
    let (_, json, _) = kipple(&env, &["scan", "--json"]);

    assert_eq!(code, ExitCode::SUCCESS);
    let line = out
        .lines()
        .find(|line| line.trim_start().starts_with("Restore cost"))
        .unwrap_or_else(|| panic!("no restore cost in {out}"));
    assert!(line.ends_with(" not estimated"), "{line}");
    let report: Value = serde_json::from_str(&json).unwrap();
    let finding = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| {
            Path::new(
                finding
                    .pointer("/path/display")
                    .and_then(Value::as_str)
                    .unwrap(),
            )
            .ends_with("rust-app/target")
        })
        .unwrap();
    assert_eq!(finding["consequence"]["cost"], "unknown");
}

// Catches a hold shown to people as its JSON name (`unknown-layout`) or as a terse code
// that reads like an error (F4), and the wording leaking into the JSON contract.
#[test]
fn holds_read_as_words_in_the_report_and_as_sentences_in_explain_and_json_keeps_names() {
    let (temp, env) = demo();
    let bunapp = temp.path().join("home/projects/bunapp/node_modules");

    let (_, human, _) = kipple(&env, &["scan"]);
    let (_, out, _) = kipple(&env, &["explain", bunapp.to_str().unwrap()]);
    let (_, json, _) = kipple(&env, &["scan", "--json"]);

    let line = human
        .lines()
        .find(|line| line.contains("bunapp"))
        .unwrap_or_else(|| panic!("no bunapp in {human}"));
    // Windows also can't tell whether it is in use (D-050).
    assert!(
        line.contains("(layout not recognised, unknown loss"),
        "{line}"
    );
    assert!(
        out.contains(
            "kipple doesn't recognise what is inside, so it can't tell what is safe to remove."
        ),
        "{out}"
    );
    let report: Value = serde_json::from_str(&json).unwrap();
    let finding = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| {
            Path::new(
                finding
                    .pointer("/path/display")
                    .and_then(Value::as_str)
                    .unwrap(),
            )
            .ends_with("bunapp/node_modules")
        })
        .unwrap();
    let reasons = finding["verdict"]["reasons"].as_array().unwrap();
    assert_eq!(
        reasons.get(..2).unwrap(),
        ["unknown-layout", "unknown-consequence"]
    );
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
    assert_eq!(finding_paths(&alone).len(), 9, "{alone}");
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
    // Every OS has at least the two Cargo targets Safe. The recent install is Review,
    // except on Windows, where an interpreted layout is always Protected (04 §4) and no
    // agent's rule removes anything.
    assert!(!line("Safe to clean").ends_with(" 0 findings"), "{out}");
    let windows = kipple_platform::PLATFORM == kipple_core::Platform::Windows;
    assert_eq!(line("Review").ends_with(" 0 findings"), windows, "{out}");
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

/// A generic `node_modules` in each of seven agent stores, and in a person's project, as
/// the PR #19 review placed them, each beside the npm install record that verifies it.
const IN_STORES: &[&str] = &[
    "home/.pi/agent/npm",
    "home/.claude/plugins/marketplaces/m",
    "home/.claude/debug/proj",
    "home/.codex/plugins/q",
    "home/.codex/cache/remote_plugin_catalog/p",
    "home/.cursor/extensions/e",
    "home/.cursor-server/extensions/e",
];
const IN_A_PROJECT: &str = "home/projects/app";

/// A home with an npm install at each of [`IN_STORES`] and [`IN_A_PROJECT`], an
/// environment whose roots, the agents' stores among them, point into it, and the home
/// to give as `--root`, as `kipple scan --root ~` does.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn stores() -> (tempfile::TempDir, Environment, String) {
    let temp = tempfile::tempdir().unwrap();
    let mut tree = tree::FixtureTree::new();
    for at in IN_STORES.iter().chain([&IN_A_PROJECT]) {
        npm_v11_19_0::project(&mut tree, at);
    }
    tree.build_in(temp.path()).unwrap();
    let now = demo_home::looked_at();
    let mut file = String::new();
    let roots = demo_home::ROOTS
        .iter()
        .chain(&[("cursor.server", "home/.cursor-server")]);
    for (name, relative) in roots {
        writeln!(file, "{name} = {}", temp.path().join(relative).display()).unwrap();
    }
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    writeln!(file, "now = {seconds}").unwrap();
    let path = temp.path().join("roots.txt");
    fs::write(&path, file).unwrap();
    let env =
        Environment::from_vars([(OsString::from("KIPPLE_TEST_ROOTS"), path.into_os_string())]);
    let home = temp.path().join("home").display().to_string();
    (temp, env, home)
}

/// Where the `node_modules` of `at` is, by its trailing components, which hold on every
/// OS whatever form the temp dir's path takes in the report.
fn node_modules_of(at: &str) -> std::path::PathBuf {
    Path::new(at)
        .strip_prefix("home")
        .unwrap_or_else(|_| Path::new(at))
        .join("node_modules")
}

/// Catches a generic artifact inside an agent's closed store shown as Safe by `scan`,
/// `scan --json`, `explain` and the home screen, and then refused at plan as protected
/// (PR #19 review): the same host answer holds it at scan, with the reason that names it.
#[test]
fn generic_artifact_inside_a_closed_store_is_protected_at_scan() {
    let (_temp, env, home) = stores();

    let (code, json, _) = kipple(&env, &["scan", "--root", &home, "--json"]);

    assert_eq!(code, ExitCode::SUCCESS);
    let report: Value = serde_json::from_str(&json).unwrap();
    let findings = report["findings"].as_array().unwrap();
    let verdict_of = |at: &str| {
        let wanted = node_modules_of(at);
        let finding = findings
            .iter()
            .filter(|f| f["rule"] == "builtin.node.node-modules")
            .find(|f| {
                let display = f.pointer("/path/display").and_then(Value::as_str).unwrap();
                Path::new(display).ends_with(&wanted)
            })
            .unwrap_or_else(|| panic!("no node_modules at {at} in {json}"));
        finding["verdict"].clone()
    };
    for at in IN_STORES {
        let verdict = verdict_of(at);
        assert_eq!(verdict["bucket"], "protected", "{at}: {verdict}");
        assert_eq!(verdict["reasons"][0], "subresource", "{at}: {verdict}");
    }
    let control = verdict_of(IN_A_PROJECT);
    let reasons = control["reasons"].as_array().unwrap();
    assert!(
        !reasons.iter().any(|reason| reason == "subresource"),
        "a person's project is no tool's store: {control}"
    );
    // Windows can't tell whether a `node_modules` is in use (D-050).
    if !cfg!(windows) {
        assert_eq!(control["bucket"], "safe", "{control}");
    }
}

/// Catches the scan's verdict and the planner's refusal drifting apart again: for every
/// finding of the same scan, selected whole, none the scan shows Safe or Review is
/// refused because the host protects it, and each the planner refuses for that is
/// Protected at scan with the same reason.
#[test]
fn scan_and_plan_agree_on_every_closed_finding() {
    use kipple::config::{self, Settings};
    use kipple::machine::Machine;
    use kipple::setup::Builtin;
    use kipple_core::{
        CancelToken, EventChannel, Hold, PlanMode, PlanRequest, Selection, Skip, Verdict,
        event_channel, plan,
    };

    let (_temp, env, home) = stores();
    let builtin = Builtin::load().unwrap();
    let catalog = config::catalog(&builtin);
    let settings = Settings::load(&env, None, &[home.into()], &catalog).unwrap();
    let policy = settings.policy.clone();
    let engine = builtin.engine(&env).unwrap();
    let cancel = CancelToken::new();
    let EventChannel { tx, rx } = event_channel(4096, &cancel);
    let scan = engine.scan(&settings.scan_request(None), &tx, &cancel);
    drop(rx);
    let machine = Machine::from_settings(&builtin, &env, settings, None).unwrap();
    let selection = Selection(scan.findings.iter().map(|f| f.id).collect());
    let request = PlanRequest {
        scan: &scan,
        selection: &selection,
        mode: PlanMode::Review,
    };

    let planned = plan(&request, &machine.judge(), &machine.mutation(), &cancel);

    let mut closed = 0;
    for refused in &planned.refused {
        let Skip::HostProtected(why) = refused.skip else {
            continue;
        };
        let finding = scan
            .findings
            .iter()
            .find(|f| f.id == refused.finding)
            .unwrap();
        let verdict = finding.verdict(&policy).unwrap();
        let held =
            matches!(&verdict, Verdict::Protected(holds) if holds.contains(&Hold::from(why)));
        let reports = matches!(&verdict, Verdict::Protected(holds) if holds == &[Hold::ReportOnly] || holds == &[Hold::Advice]);
        assert!(
            held || reports,
            "{} refused as {why} but {verdict:?} at scan",
            finding.path.display()
        );
        closed += 1;
    }
    assert!(
        closed >= IN_STORES.len(),
        "every store's node_modules is refused: {:?}",
        planned.refused
    );
}

/// Catches the fix closing what D-076 opens: an agent's own rule in its own open area, on
/// the demo's verified layouts, keeps the verdict it had.
#[test]
fn open_areas_still_open_at_scan() {
    let (_temp, env) = demo();

    let (_, json, _) = kipple(&env, &["scan", "--json"]);

    let report: Value = serde_json::from_str(&json).unwrap();
    let findings = report["findings"].as_array().unwrap();
    let opened = [
        ("builtin.codex.plugin-catalog-cache", "safe"),
        ("builtin.codex.legacy-tui-log", "review"),
        ("builtin.claude-code.closed-debug-logs", "review"),
    ];
    for (rule, bucket) in opened {
        let found: Vec<&Value> = findings.iter().filter(|f| f["rule"] == rule).collect();
        assert!(!found.is_empty(), "the demo has {rule}");
        for finding in &found {
            let reasons = finding["verdict"]["reasons"].as_array().unwrap();
            assert!(
                !reasons.iter().any(|reason| reason == "subresource"),
                "{rule} is in an area its store opens to it: {finding}"
            );
        }
        if std::env::consts::OS == "linux" {
            assert!(
                found.iter().any(|f| f["verdict"]["bucket"] == bucket),
                "{rule} keeps a {bucket} finding on Linux: {found:?}"
            );
        }
    }
}

/// The `Looks in` row of `kipple explain <rule>`.
#[expect(
    clippy::panic,
    reason = "a rule explained without the row fails the test"
)]
fn looks_in(env: &Environment, rule: &str) -> String {
    let (code, out, _) = kipple(env, &["explain", rule]);
    assert_eq!(code, ExitCode::SUCCESS, "{out}");
    let line = out
        .lines()
        .find(|line| line.trim_start().starts_with("Looks in"));
    let line = line.unwrap_or_else(|| panic!("no Looks in row in {out}"));
    line.trim_start()
        .trim_start_matches("Looks in")
        .trim()
        .to_owned()
}

/// Catches `explain` naming a git rule's own root alone, so a person reading it concludes
/// kipple doesn't read the agents' worktree locations (PR #19 review), and a rule whose
/// integration adds no root naming its own twice.
#[test]
fn explain_names_every_root_a_git_rule_reads() {
    let (_temp, env) = demo();

    assert_eq!(
        looks_in(&env, "builtin.git.worktree-remove"),
        "config.project-roots, git.agent-locations"
    );
    assert_eq!(
        looks_in(&env, "builtin.cargo.target"),
        "config.project-roots"
    );
}
