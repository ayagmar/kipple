//! `kipple clean`, `plan`, `apply`, `history` and `restore` end to end on `demo-home`,
//! every root redirected into a temp dir through the hidden test-roots override (spec
//! M3b pinned 1, 2, 12, 17; `docs/04-safety-model.md` §10). Nothing here reaches the real
//! home, trash or data directory.
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]

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
use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::SystemTime;

use clap::Parser as _;
use kipple::{CANCELLED, Cli, Io, Person};
use kipple_core::{CancelToken, Platform};
use kipple_platform::{Environment, PLATFORM};
use serde_json::Value;
use tempfile::TempDir;

const TARGET: &str = "builtin.cargo.target";
const ACME: &str = "home/projects/clients/acme/api/target";
const RUST_APP: &str = "home/projects/rust-app/target";

/// `demo-home` in a temp dir, and the roots file that points every root into it.
struct Demo {
    temp: TempDir,
    roots: PathBuf,
}

impl Demo {
    fn new() -> Self {
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
        Self { temp, roots }
    }

    fn at(&self, relative: &str) -> PathBuf {
        self.temp.path().join(relative)
    }

    /// The environment a run sees: the roots file, and `PATH`, so git can be asked.
    fn env(&self) -> Environment {
        let mut vars = vec![(
            OsString::from("KIPPLE_TEST_ROOTS"),
            self.roots.clone().into_os_string(),
        )];
        vars.extend(std::env::var_os("PATH").map(|path| (OsString::from("PATH"), path)));
        Environment::from_vars(vars)
    }

    /// Runs `kipple <args>` with nobody to answer.
    fn run(&self, args: &[&str]) -> Ran {
        self.run_with(args, Asked::Nobody, &CancelToken::new())
    }

    /// Runs `kipple <args>` as a person at a terminal who answers `answer`.
    fn answer(&self, args: &[&str], answer: &str) -> Ran {
        self.run_with(args, Asked::Person(answer), &CancelToken::new())
    }

    fn run_with(&self, args: &[&str], asked: Asked<'_>, cancel: &CancelToken) -> Ran {
        let cli = Cli::parse_from(std::iter::once("kipple").chain(args.iter().copied()));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut io = Io::redirected(&mut out, &mut err);
        io.cancel = cancel.clone();
        if let Asked::Person(answer) = asked {
            io.person = Person::AtTerminal;
            io.stdin = Box::new(Cursor::new(answer.as_bytes().to_vec()));
        }
        let code = kipple::run(&cli, &self.env(), io);
        Ran {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    /// Runs the built binary as a separate process, as a crash or a terminal needs:
    /// `vars` added to the roots file, `input` on its stdin.
    #[expect(
        clippy::disallowed_methods,
        reason = "the end-to-end test runs the built kipple, not a native tool"
    )]
    fn spawn(&self, args: &[&str], vars: &[(&str, &str)], input: &str) -> Spawned {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kipple"))
            .args(args)
            .env_clear()
            .env("KIPPLE_TEST_ROOTS", &self.roots)
            .envs(system_root())
            .envs(vars.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        Spawned {
            code: output.status.code(),
            out: String::from_utf8(output.stdout).unwrap(),
            err: String::from_utf8(output.stderr).unwrap(),
        }
    }

    /// The runs `history --json` lists.
    fn history(&self) -> Vec<Value> {
        let ran = self.run(&["history", "--json"]);
        assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
        items(&json(&ran.out), "runs").to_vec()
    }
}

/// Who answers a prompt.
#[derive(Debug, Clone, Copy)]
enum Asked<'answer> {
    Nobody,
    Person(&'answer str),
}

/// What a run in this process did.
#[derive(Debug)]
struct Ran {
    code: ExitCode,
    out: String,
    err: String,
}

/// What a run of the binary did.
#[derive(Debug)]
struct Spawned {
    code: Option<i32>,
    out: String,
    err: String,
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

/// The array `key` of `value`.
fn items<'value>(value: &'value Value, key: &str) -> &'value [Value] {
    value.get(key).and_then(Value::as_array).unwrap()
}

/// The text at `pointer` in `value`.
fn text<'value>(value: &'value Value, pointer: &str) -> &'value str {
    value.pointer(pointer).and_then(Value::as_str).unwrap()
}

/// The path display of each item of a receipt or run, in order.
fn item_paths(receipt: &Value) -> Vec<PathBuf> {
    items(receipt, "items")
        .iter()
        .map(|item| PathBuf::from(text(item, "/path")))
        .collect()
}

/// The item of `receipt` whose path ends with `relative`.
fn item_at<'receipt>(receipt: &'receipt Value, relative: &str) -> &'receipt Value {
    items(receipt, "items")
        .iter()
        .find(|item| Path::new(text(item, "/path")).ends_with(relative))
        .unwrap()
}

fn statuses(receipt: &Value) -> Vec<&str> {
    items(receipt, "items")
        .iter()
        .map(|item| text(item, "/outcome/status"))
        .collect()
}

/// `SYSTEMROOT`, which Windows programs started with a cleared environment still need;
/// nothing elsewhere.
fn system_root() -> Option<(&'static str, OsString)> {
    std::env::var_os("SYSTEMROOT").map(|root| ("SYSTEMROOT", root))
}

fn present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Catches `--yes` cleaning more or less than the Safe findings of the rules `--only`
/// names, a run left unrecorded, or a second clean finding what the first removed (J6,
/// pinned 1, 17): both Safe Cargo targets go, nothing else does, `history` lists the run,
/// and the next clean has nothing to clean.
#[test]
fn clean_yes_removes_exactly_the_safe_findings_of_the_rules_named_and_records_the_run() {
    let demo = Demo::new();

    let ran = demo.run(&["clean", "--only", TARGET, "--yes", "--json"]);

    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    let receipt = json(&ran.out);
    assert_eq!(receipt["schema"], "kipple.receipt/v1");
    assert_eq!(receipt["ending"], "done");
    assert_eq!(statuses(&receipt), ["done", "done"]);
    let paths = item_paths(&receipt);
    assert!(
        paths.iter().any(|p| p.ends_with(ACME)) && paths.iter().any(|p| p.ends_with(RUST_APP)),
        "the two Safe targets are the items: {paths:?}"
    );
    only_the_targets_are_gone(&demo);

    let runs = demo.history();
    assert_eq!(runs.len(), 1, "one run is kept: {runs:?}");
    assert_eq!(runs[0]["id"], receipt["run"]);
    assert_eq!(runs[0]["state"], "complete");
    assert_eq!(item_paths(&runs[0]), paths);

    let again = demo.run(&["clean", "--only", TARGET, "--yes", "--json"]);
    assert_eq!(again.code, ExitCode::SUCCESS, "{}", again.err);
    assert_eq!(json(&again.out)["ending"], "nothing-to-clean");
    assert_eq!(demo.history().len(), 1, "an empty plan stores no run");
}

/// Both Safe Cargo targets are gone, and nothing else is.
fn only_the_targets_are_gone(demo: &Demo) {
    assert!(!present(&demo.at(ACME)), "acme's target is gone");
    assert!(!present(&demo.at(RUST_APP)), "rust-app's target is gone");
    for kept in [
        "home/projects/rust-app/Cargo.toml",
        "home/projects/dash/node_modules",
        "home/projects/web/node_modules",
        "home/projects/notes/target/plan.md",
    ] {
        assert!(present(&demo.at(kept)), "{kept} stays");
    }
}

/// Catches `--yes` without `--only` cleaning everything Safe (J6): it exits 3, says how
/// to name what to clean, and changes nothing.
#[test]
fn yes_without_only_changes_nothing_and_says_what_to_name() {
    let demo = Demo::new();

    let ran = demo.run(&["clean", "--yes"]);

    assert_eq!(ran.code, ExitCode::from(3));
    assert!(ran.err.contains("--only"), "{}", ran.err);
    assert!(present(&demo.at(ACME)), "acme's target stays");
    assert!(demo.history().is_empty(), "no run is stored");
}

/// Catches a clean with nobody to confirm it, and no `--yes`, running anyway (pinned 1):
/// a script or a pipe gets exit 3 and nothing changes.
#[test]
fn without_a_terminal_or_yes_nothing_runs() {
    let demo = Demo::new();

    let ran = demo.run(&["clean", "--only", TARGET]);

    assert_eq!(ran.code, ExitCode::from(3));
    assert!(ran.err.contains("terminal"), "{}", ran.err);
    assert!(present(&demo.at(ACME)), "acme's target stays");
    assert!(demo.history().is_empty(), "no run is stored");
}

/// Catches a "no", or an answer that isn't yes, running or storing anything, and a
/// prompt that doesn't show what it would remove (J3, pinned 1).
#[test]
fn a_person_who_doesnt_say_yes_changes_nothing() {
    let demo = Demo::new();

    for answer in ["n\n", "\n", "yes please\n", ""] {
        let ran = demo.answer(&["clean", "--only", TARGET], answer);

        assert_eq!(ran.code, ExitCode::from(3), "{answer:?}: {}", ran.err);
        assert!(ran.err.contains("Clean 2 items? [y/N]"), "{}", ran.err);
        assert!(ran.err.contains("permanent"), "{}", ran.err);
        assert!(present(&demo.at(ACME)), "{answer:?}: acme's target stays");
    }
    assert!(demo.history().is_empty(), "no run is stored");
}

/// Catches a "yes" not running the plan shown, or the prompt's Review findings being
/// cleaned on the command line (pinned 1): the Safe items go, the recent `node_modules`
/// is only listed as needing the TUI.
#[test]
fn a_person_who_says_yes_cleans_the_safe_items_shown_and_lists_the_review_ones() {
    let demo = Demo::new();

    let ran = demo.answer(&["clean"], "y\n");

    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    assert!(ran.err.contains("Needs review in the TUI"), "{}", ran.err);
    assert!(ran.err.contains("web"), "{}", ran.err);
    assert!(
        ran.out.contains("Removed an estimated") && !ran.out.contains("to the trash"),
        "{}",
        ran.out
    );
    assert!(!present(&demo.at(ACME)), "acme's target is gone");
    assert!(
        present(&demo.at("home/projects/web/node_modules")),
        "the Review node_modules stays"
    );
    assert_eq!(demo.history().len(), 1);
}

/// Catches `--yes` moving a folder to the trash whole, which needs a review (pinned 1):
/// nothing is planned, it exits 3, and the receipt says why for each.
#[test]
fn yes_never_moves_a_folder_to_the_trash_whole() {
    let demo = Demo::new();

    let ran = demo.run(&[
        "clean", "--only", TARGET, "--method", "trash", "--yes", "--json",
    ]);

    assert_eq!(ran.code, ExitCode::from(3), "{}", ran.err);
    let receipt = json(&ran.out);
    assert_eq!(receipt["ending"], "none-ran");
    let skips: Vec<&Value> = receipt["refused"]
        .as_array()
        .unwrap()
        .iter()
        .map(|refused| &refused["skip"]["skip"])
        .collect();
    assert_eq!(skips, ["whole-not-reviewed", "whole-not-reviewed"]);
    assert!(present(&demo.at(ACME)), "acme's target stays");
    assert!(demo.history().is_empty(), "no run is stored");
}

/// Catches `plan -o` replacing a file, or `apply` running a plan file without planning
/// it again (pinned 2): the second `plan -o` to the same file fails and leaves it, and
/// `apply --yes` removes the items still Safe.
#[test]
fn a_plan_file_is_written_once_and_applied_after_planning_it_again() {
    let demo = Demo::new();
    let file = demo.at("plan.json");
    let file_arg = file.to_str().unwrap();

    let planned = demo.run(&["plan", "--only", TARGET, "-o", file_arg]);
    assert_eq!(planned.code, ExitCode::SUCCESS, "{}", planned.err);
    assert!(present(&demo.at(ACME)), "planning removes nothing");
    let written = fs::read(&file).unwrap();
    assert!(
        written.starts_with(b"{\n  \"schema\": \"kipple.plan/v1\""),
        "a plan file"
    );

    let again = demo.run(&["plan", "--only", TARGET, "-o", file_arg]);
    assert_eq!(again.code, ExitCode::FAILURE, "{}", again.err);
    assert_eq!(fs::read(&file).unwrap(), written, "the file is untouched");

    let applied = demo.run(&["apply", file_arg, "--yes", "--json"]);
    assert_eq!(applied.code, ExitCode::SUCCESS, "{}", applied.err);
    assert_eq!(statuses(&json(&applied.out)), ["done", "done"]);
    assert!(!present(&demo.at(ACME)), "acme's target is gone");
}

/// Catches a plan file applied to a project that is gone or no longer mounted (04 §10
/// "a project that is missing or unmounted"): every item drops, the plan is stale, it
/// exits 3 and the receipt names each item dropped.
#[test]
fn a_plan_file_whose_project_is_gone_is_stale() {
    let demo = Demo::new();
    let elsewhere = tempfile::tempdir().unwrap();
    let mut tree = tree::FixtureTree::new();
    cargo_v1_99_0::project(&mut tree, "app");
    tree.build_in(elsewhere.path()).unwrap();
    let root = elsewhere.path().to_str().unwrap().to_owned();
    let file = demo.at("plan.json");
    let file_arg = file.to_str().unwrap();
    let planned = demo.run(&["plan", "--only", TARGET, "--root", &root, "-o", file_arg]);
    assert_eq!(planned.code, ExitCode::SUCCESS, "{}", planned.err);

    drop(elsewhere);
    let applied = demo.run(&["apply", file_arg, "--root", &root, "--yes", "--json"]);

    assert_eq!(applied.code, ExitCode::from(3), "{}", applied.err);
    let receipt = json(&applied.out);
    assert_eq!(receipt["ending"], "stale");
    let dropped = receipt["dropped"].as_array().unwrap();
    assert_eq!(dropped.len(), 1, "{receipt}");
    assert_eq!(dropped[0]["why"]["reason"], "not-found");
    assert!(
        Path::new(dropped[0]["path"].as_str().unwrap()).ends_with("app/target"),
        "{receipt}"
    );
    assert!(demo.history().is_empty(), "no run is stored");
}

/// Catches a plan file that isn't one being applied, or reported as a failure rather
/// than a usage error (exit 2).
#[test]
fn a_file_that_isnt_a_plan_is_refused() {
    let demo = Demo::new();
    let file = demo.at("notes.json");
    fs::write(&file, br#"{"schema":"kipple.scan/v1"}"#).unwrap();

    let ran = demo.run(&["apply", file.to_str().unwrap(), "--yes"]);

    assert_eq!(ran.code, ExitCode::from(2), "{}", ran.err);
    assert!(ran.err.contains("can't be applied"), "{}", ran.err);
}

/// Catches the event stream mixing formats or numbering, or ending without the receipt
/// (pinned 18): scan lines, then the run's lines, each naming its format, `seq` counting
/// on across both, then the receipt.
#[test]
fn clean_events_are_the_scans_then_the_runs_then_the_receipt() {
    let demo = Demo::new();

    let ran = demo.run(&["clean", "--only", TARGET, "--yes", "--events", "jsonl"]);

    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    let lines: Vec<Value> = ran.out.lines().map(json).collect();
    let schemas: Vec<&str> = lines
        .iter()
        .map(|line| line["schema"].as_str().unwrap())
        .collect();
    let scan = schemas
        .iter()
        .take_while(|schema| **schema == "kipple.event/v1")
        .count();
    let apply = schemas[scan..]
        .iter()
        .take_while(|schema| **schema == "kipple.apply-event/v1")
        .count();
    assert!(scan > 0 && apply > 0, "{schemas:?}");
    assert_eq!(scan + apply + 1, lines.len(), "{schemas:?}");
    assert_eq!(schemas.last(), Some(&"kipple.receipt/v1"));
    let seqs: Vec<u64> = lines[..scan + apply]
        .iter()
        .map(|line| line["seq"].as_u64().unwrap())
        .collect();
    let counted: Vec<u64> = (0..seqs.len() as u64).collect();
    assert_eq!(seqs, counted, "seq counts on across both formats");
    let types: Vec<&str> = lines[scan..scan + apply]
        .iter()
        .map(|line| line["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "run-started",
            "item-started",
            "item-ended",
            "item-started",
            "item-ended"
        ]
    );
}

/// Catches Ctrl+C being ignored (pinned 17): a mutating command whose token is cancelled
/// exits 130 and removes nothing it hadn't removed already.
#[test]
fn a_cancelled_clean_exits_130_and_removes_nothing() {
    let demo = Demo::new();
    let cancel = CancelToken::new();
    cancel.cancel();

    let ran = demo.run_with(
        &["clean", "--only", TARGET, "--yes"],
        Asked::Nobody,
        &cancel,
    );

    assert_eq!(ran.code, ExitCode::from(CANCELLED), "{}", ran.err);
    assert!(present(&demo.at(ACME)), "acme's target stays");
    assert!(demo.history().is_empty(), "no run is stored");
}

/// Catches a file git tracks inside a build directory being deleted with it (04 §10 "a
/// tracked or custom file inside an apparent artifact", D-066): the rest of the target
/// goes, the tracked file and the directories holding it stay.
#[test]
fn a_file_git_tracks_inside_a_target_stays() {
    let demo = Demo::new();
    let project = demo.at("home/projects/rust-app");
    git(&demo, &project, &["init", "-q"]);
    fs::write(project.join("target/debug/kept.txt"), b"tracked\n").unwrap();
    git(
        &demo,
        &project,
        &["add", "-f", "--", "target/debug/kept.txt"],
    );

    let ran = demo.run(&["clean", "--only", TARGET, "--yes", "--json"]);

    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    let receipt = json(&ran.out);
    let rust_app = item_at(&receipt, RUST_APP);
    assert_eq!(rust_app["outcome"]["status"], "done", "{rust_app}");
    assert!(
        present(&project.join("target/debug/kept.txt")),
        "the tracked file stays"
    );
    assert!(
        !present(&project.join("target/debug/demo")),
        "the build output beside it is gone"
    );
    assert!(!present(&demo.at(ACME)), "the other target is gone whole");
}

/// Runs the host's git in `dir`, with a temporary home and no config of the user's.
#[expect(
    clippy::disallowed_methods,
    reason = "the fixture repository is built with the host's git"
)]
fn git(demo: &Demo, dir: &Path, args: &[&str]) {
    let home = demo.at("git-home");
    fs::create_dir_all(&home).unwrap();
    let config = home.join("gitconfig");
    fs::write(&config, b"[user]\n\tname = t\n\temail = t@example.com\n").unwrap();
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_clear()
        .envs(std::env::var_os("PATH").map(|path| ("PATH", path)))
        .envs(system_root())
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &config)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} succeeded");
}

/// Catches a crash between an item's intent and its outcome being lost or run again
/// (04 §7, §10 "crash reconciliation", pinned 8, 10): the crashed run shows interrupted,
/// the next clean reconciles it through the store first, recording what it finds and
/// running nothing of it again, then cleans as asked.
#[test]
fn a_run_a_crash_interrupted_is_reconciled_by_the_next_clean() {
    let demo = Demo::new();

    let crashed = demo.spawn(
        &["clean", "--only", TARGET, "--yes"],
        &[("KIPPLE_TEST_CRASH_AT", "0")],
        "",
    );

    assert_eq!(crashed.code, Some(99), "{}{}", crashed.out, crashed.err);
    assert!(present(&demo.at(ACME)), "nothing ran after the intent");
    assert!(present(&demo.at(RUST_APP)), "nothing ran after the intent");
    let before = demo.history();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0]["state"], "interrupted");

    let ran = demo.run(&["clean", "--only", TARGET, "--yes", "--json"]);

    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    assert!(ran.err.contains("was interrupted"), "{}", ran.err);
    assert_eq!(statuses(&json(&ran.out)), ["done", "done"]);
    let runs = demo.history();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["state"], "interrupted", "item 1 never started");
    let crashed_item = &runs[0]["items"][0]["outcome"];
    assert_eq!(crashed_item["status"], "indeterminate", "{crashed_item}");
    assert_eq!(
        crashed_item["found"]["found"], "unchanged",
        "{crashed_item}"
    );
}

/// Has a person at the test build's terminal hook confirm `clean --method trash` of the
/// Cargo targets, and returns the run's ID and record.
fn trash_the_targets(demo: &Demo) -> (String, Value) {
    let trashed = demo.spawn(
        &["clean", "--only", TARGET, "--method", "trash"],
        &[("KIPPLE_TEST_TERMINAL", "1")],
        "y\n",
    );
    assert_eq!(trashed.code, Some(0), "{}{}", trashed.out, trashed.err);
    assert!(
        trashed.err.contains("trash move takes each folder whole"),
        "{}",
        trashed.err
    );
    // 04 §8: what went to the trash is still on disk, never counted as removed.
    assert!(
        trashed
            .out
            .contains("in 2 items to the trash, still on disk.")
            && !trashed.out.contains("Removed an estimated"),
        "{}",
        trashed.out
    );
    let run = demo.history().pop().unwrap();
    (text(&run, "/id").to_owned(), run)
}

/// Catches a trash run that can't be put back, a restore run twice, or a restore that
/// overwrites (J4, pinned 12, 04 §10 "restore conflicts"). A person confirms
/// `clean --method trash` through the test build's terminal hook; `restore` puts an item
/// back once, says "already restored" the second time, and refuses when something is at
/// the original name and when the trash was emptied. Linux only: macOS's trash isn't
/// driven here, and Windows has none (D-062).
#[test]
fn trashed_items_restore_once_and_refuse_a_taken_name_or_an_emptied_trash() {
    if PLATFORM != Platform::Linux {
        return;
    }
    let demo = Demo::new();
    let (run, record) = trash_the_targets(&demo);
    assert!(!present(&demo.at(ACME)), "acme's target moved");
    let number = |relative| item_at(&record, relative)["item"].to_string();
    let (acme, rust_app) = (number(ACME), number(RUST_APP));

    let restored = demo.spawn(&["restore", &run, &acme], &[], "");
    assert_eq!(restored.code, Some(0), "{}{}", restored.out, restored.err);
    assert!(
        present(&demo.at(&format!("{ACME}/debug/demo"))),
        "acme's target is back"
    );
    let twice = demo.spawn(&["restore", &run, &acme], &[], "");
    assert_eq!(twice.code, Some(3), "{}{}", twice.out, twice.err);
    assert!(twice.out.contains("already restored"), "{}", twice.out);

    fs::create_dir(demo.at(RUST_APP)).unwrap();
    let occupied = demo.spawn(&["restore", &run, &rust_app], &[], "");
    assert_eq!(occupied.code, Some(1), "{}{}", occupied.out, occupied.err);
    assert!(
        occupied
            .out
            .contains("something is already where it would go"),
        "{}",
        occupied.out
    );
    assert!(
        !present(&demo.at(&format!("{RUST_APP}/debug"))),
        "nothing was put over it"
    );

    let slot = &item_at(&record, RUST_APP)["outcome"]["effect"]["trashed"];
    remove_dir_all(&Path::new(text(slot, "/trash")).join(text(slot, "/name")));
    remove_dir_all(&demo.at(RUST_APP));
    let emptied = demo.spawn(&["restore", &run, &rust_app], &[], "");
    assert_eq!(emptied.code, Some(1), "{}{}", emptied.out, emptied.err);
    assert!(
        emptied.out.contains("isn't in the trash any more"),
        "{}",
        emptied.out
    );
}

/// Catches a restore into a folder that replaced the one the item came from (J4, 04 §10
/// "restore conflicts"): it is refused, and nothing goes into the new folder. Linux only,
/// as above.
#[test]
fn a_restore_into_a_replaced_folder_is_refused() {
    if PLATFORM != Platform::Linux {
        return;
    }
    let demo = Demo::new();
    let (run, _) = trash_the_targets(&demo);
    let parent = demo.at("home/projects/clients/acme/api");
    rename(&parent, &demo.at("home/projects/clients/acme/api.old"));
    fs::create_dir(&parent).unwrap();

    let replaced = demo.spawn(&["restore", &run], &[], "");

    assert_eq!(replaced.code, Some(1), "{}{}", replaced.out, replaced.err);
    assert!(
        replaced.out.contains("the folder holding it changed"),
        "{}",
        replaced.out
    );
    assert!(
        !present(&parent.join("target")),
        "nothing went into the new folder"
    );
    assert!(
        present(&demo.at(&format!("{RUST_APP}/debug/demo"))),
        "the item whose folder stayed is back"
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "the test empties the trash and clears a name, as a person would"
)]
fn remove_dir_all(path: &Path) {
    fs::remove_dir_all(path).unwrap();
}

#[expect(
    clippy::disallowed_methods,
    reason = "the test replaces a project's folder, as a person would"
)]
fn rename(from: &Path, to: &Path) {
    fs::rename(from, to).unwrap();
}

/// Catches a file another program holds open without sharing delete being reported
/// removed, failing the whole run, or stopping the items after it (04 §10 "locked files
/// on Windows"): through `clean`, its item is partial with the file left busy, the other
/// target goes whole, and the run exits 1.
#[cfg(windows)]
#[test]
fn a_locked_file_leaves_its_item_partial_and_the_rest_goes() {
    use std::os::windows::fs::OpenOptionsExt as _;

    /// `FILE_SHARE_READ`: others may read the file, never delete it.
    const SHARE_READ: u32 = 0x1;

    let demo = Demo::new();
    let locked = demo.at(RUST_APP).join("debug/demo");
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ)
        .open(&locked)
        .unwrap();

    let ran = demo.run(&["clean", "--only", TARGET, "--yes", "--json"]);
    drop(held);

    assert_eq!(ran.code, ExitCode::from(1), "{}", ran.err);
    let receipt = json(&ran.out);
    let rust_app = &item_at(&receipt, RUST_APP)["outcome"];
    assert_eq!(rust_app["status"], "partial", "{rust_app}");
    let busy = items(&rust_app["effect"], "left")
        .iter()
        .find(|left| left["reason"] == "busy");
    assert_eq!(
        busy.map(|left| &left["count"]),
        Some(&Value::from(1)),
        "{rust_app}"
    );
    assert!(present(&locked), "the locked file stays");
    assert_eq!(item_at(&receipt, ACME)["outcome"]["status"], "done");
    assert!(!present(&demo.at(ACME)), "the other target is gone whole");
}
