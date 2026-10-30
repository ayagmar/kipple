//! The config file end to end on `demo-home` (`docs/03-functional-spec.md` §6, D-051):
//! which file is read, what makes it unusable, what each command does then, how its
//! level, rules and kept or excluded paths reach every frontend, and how edits are
//! written. Built only with the `test-roots` feature: every root, the config directory
//! included, points into a temp dir.

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
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use clap::Parser as _;
use kipple::config::{self, DetailPlace, Edit, Location, Source};
use kipple::setup::Builtin;
use kipple::theme::Glyphs;
use kipple::{Cli, USAGE};
use kipple_core::{Level, Roots, RuleId, SymbolicRoot};
use kipple_platform::Environment;
use serde_json::Value;

const CARGO: &str = "builtin.cargo.target";

/// `demo-home` in a temp dir.
struct Demo {
    temp: tempfile::TempDir,
    roots_file: PathBuf,
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
impl Demo {
    /// Every demo root but those `left_out` names, and `extra` lines.
    fn new(left_out: &[&str], extra: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let now = demo_home::looked_at();
        demo_home::tree(now).build_in(temp.path()).unwrap();
        let mut file = String::new();
        for (name, relative) in demo_home::ROOTS {
            if !left_out.contains(name) {
                writeln!(file, "{name} = {}", temp.path().join(relative).display()).unwrap();
            }
        }
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        writeln!(file, "now = {seconds}\n{extra}").unwrap();
        let roots_file = temp.path().join("roots.txt");
        fs::write(&roots_file, file).unwrap();
        Self { temp, roots_file }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.temp.path().join(relative)
    }

    /// The machine, with these variables besides the roots file.
    fn env(&self, vars: &[(&str, &Path)]) -> Environment {
        let roots = (
            OsString::from("KIPPLE_TEST_ROOTS"),
            self.roots_file.clone().into_os_string(),
        );
        let others = vars
            .iter()
            .map(|(name, value)| (OsString::from(name), value.as_os_str().to_owned()));
        Environment::from_vars(std::iter::once(roots).chain(others))
    }

    /// Writes a config file at `relative` and returns its path.
    fn config(&self, relative: &str, text: &str) -> PathBuf {
        let file = self.path(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, text).unwrap();
        file
    }

    /// The default config file: kipple's config directory under the demo's `xdg.config`
    /// (`known.roaming-app-data` on Windows).
    fn default_config(&self, text: &str) -> PathBuf {
        let roots = kipple_platform::resolve_roots(&self.env(&[]), &[]);
        let location = config::locate(&self.env(&[]), None, &roots);
        let file = location.path.unwrap();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, text).unwrap();
        file
    }
}

/// Runs `kipple <args>` and returns its exit code, stdout and stderr.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn kipple(env: &Environment, args: &[&str]) -> (ExitCode, String, String) {
    let cli = Cli::parse_from(std::iter::once("kipple").chain(args.iter().copied()));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = kipple::run(&cli, env, false, &mut out, &mut err);
    let text = |bytes| String::from_utf8(bytes).unwrap();
    (code, text(out), text(err))
}

/// `kipple scan --json` with the config at `file`.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn scan_with(env: &Environment, file: &Path) -> Value {
    let (code, out, err) = kipple(env, &["--config", file.to_str().unwrap(), "scan", "--json"]);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    serde_json::from_str(&out).unwrap()
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn findings(report: &Value) -> &Vec<Value> {
    report.get("findings").and_then(Value::as_array).unwrap()
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn path_of(finding: &Value) -> PathBuf {
    PathBuf::from(
        finding
            .pointer("/path/display")
            .and_then(Value::as_str)
            .unwrap(),
    )
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn reasons(finding: &Value) -> Vec<&str> {
    finding
        .pointer("/verdict/reasons")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect()
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn bucket(finding: &Value) -> &str {
    finding
        .pointer("/verdict/bucket")
        .and_then(Value::as_str)
        .unwrap()
}

/// The Cargo target the demo has in `rust-app`, as the report names it.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn rust_app_target(report: &Value) -> PathBuf {
    findings(report)
        .iter()
        .map(path_of)
        .find(|path| path.ends_with("rust-app/target"))
        .unwrap()
}

#[test]
fn no_config_at_the_default_location_means_balanced_and_the_default_roots() {
    let demo = Demo::new(&[], "");

    let (code, out, err) = kipple(&demo.env(&[]), &["scan", "--json"]);

    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    let report: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(report["level"], "balanced");
    assert!(findings(&report).iter().all(|f| f["level"] == "balanced"));
    assert!(findings(&report).iter().any(|f| bucket(f) == "safe"));
}

#[test]
fn every_config_kipple_cant_use_exits_2_with_the_reason_and_where_it_is() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let cases = [
        ("[care\nlevel = 1", "line 1, column"),
        (
            "[care]\nspeed = \"fast\"",
            "line 2, column 1: unknown key care.speed",
        ),
        ("colour = true", "line 1, column 1: unknown key colour"),
        (
            "[ui]\ncolour = true",
            "line 2, column 1: unknown key ui.colour",
        ),
        ("ui = 1", "ui: expected a table"),
        (
            "[review]\nconfirm_by_typing_above = \"5 GB\"",
            "review is not supported yet",
        ),
        ("[retention]", "retention is not supported yet"),
        (
            "[rules.\"builtin.cargo.target\"]\nmethod = \"trash\"",
            "line 2, column 1: rules.\"builtin.cargo.target\".method is not supported yet",
        ),
        (
            "[care]\nlevel = \"reckless\"",
            "line 2, column 9: care.level: expected",
        ),
        (
            "[clean]\nmethod = \"shred\"",
            "clean.method: expected delete or trash",
        ),
        (
            "[rules.\"builtin.cargo.target\"]\nrecent_days = 0",
            "recent_days: expected a whole number of days from 1 to 365",
        ),
        (
            "[rules.\"builtin.cargo.target\"]\nrecent_days = 366",
            "from 1 to 365",
        ),
        (
            "[rules.\"builtin.cargo.target\"]\nenabled = \"no\"",
            "expected true or false",
        ),
        (
            "[rules.\"builtin.made.up\"]\nenabled = false",
            "line 1, column 8: there is no built-in rule builtin.made.up",
        ),
        (
            "[exclude]\nrules = [\"made.up\"]",
            "line 2, column 10: there is no built-in rule made.up",
        ),
        (
            "[exclude]\npaths = [\"projects/x\"]",
            "exclude.paths: \"projects/x\" is neither absolute nor starts with ~",
        ),
        (
            "[scan]\nproject_roots = \"~/p\"",
            "scan.project_roots: expected a list of strings",
        ),
        (
            "[keep]\npaths = [{ note = \"x\" }]",
            "keep.paths: each table needs a path",
        ),
    ];
    for (text, expected) in cases {
        let file = demo.config("bad.toml", text);

        let (code, _, err) = kipple(&env, &["--config", file.to_str().unwrap(), "scan"]);

        assert_eq!(code, ExitCode::from(USAGE), "{text}");
        assert!(err.contains(expected), "{text}: {err}");
        assert!(err.contains("from --config"), "{err}");
    }
}

// Catches a `[keys]` table that can't be used being ignored or blamed on the wrong place:
// an unknown action or key, esc, one key taken twice where both are read, and an action
// that needs a key left with none (03 §6 Keys).
#[test]
fn a_keys_table_kipple_cant_use_exits_2_with_the_reason_and_where_it_is() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let cases = [
        (
            "[keys]\nzap = [\"x\"]",
            "line 2, column 1: unknown key keys.zap",
        ),
        (
            "[keys]\nselect = \"x\"",
            "line 2, column 10: keys.select: expected a list of keys",
        ),
        (
            "[keys]\nselect = [\"ctrl-s\"]",
            "line 2, column 11: keys.select: expected one character, space, enter",
        ),
        (
            "[keys]\nquit = [\"esc\"]",
            "line 2, column 9: keys.quit: esc always goes back or closes",
        ),
        (
            "[keys]\nselect = [\"x\"]\nkeep = [\"y\", \"x\"]",
            "line 3, column 14: keys.keep: \"x\" is bound to select as well in the tree",
        ),
        (
            "[keys]\nselect = []",
            "line 2, column 1: keys.select: select needs a key",
        ),
        (
            "[keys]\nkeep = [\"j\", \"down\"]",
            "line 2, column 1: keys.keep: this leaves move-down with no key in the tree",
        ),
    ];
    for (text, expected) in cases {
        let file = demo.config("bad.toml", text);

        let (code, _, err) = kipple(&env, &["--config", file.to_str().unwrap(), "scan"]);

        assert_eq!(code, ExitCode::from(USAGE), "{text}");
        assert!(err.contains(expected), "{text}: {err}");
        assert!(err.contains("from --config"), "{err}");
    }
}

#[test]
fn a_named_config_that_isnt_a_readable_file_exits_2_but_the_default_one_missing_does_not() {
    let demo = Demo::new(&[], "");
    let missing = demo.path("nowhere/config.toml");
    let dir = demo.path("home");

    for (env, args) in [
        (
            demo.env(&[]),
            vec!["--config", missing.to_str().unwrap(), "scan"],
        ),
        (demo.env(&[("KIPPLE_CONFIG", &missing)]), vec!["scan"]),
        (
            demo.env(&[]),
            vec!["--config", dir.to_str().unwrap(), "scan"],
        ),
    ] {
        let (code, _, err) = kipple(&env, &args);
        assert_eq!(code, ExitCode::from(USAGE), "{args:?}");
        assert!(
            err.contains("no such file") || err.contains("directory"),
            "{err}"
        );
    }
    let (code, _, err) = kipple(&demo.env(&[("KIPPLE_CONFIG", &missing)]), &["scan"]);
    assert_eq!(code, ExitCode::from(USAGE));
    assert!(err.contains("from KIPPLE_CONFIG"), "{err}");
}

#[test]
#[cfg(unix)]
fn a_named_config_that_is_a_link_to_nothing_exits_2() {
    let demo = Demo::new(&[], "");
    let link = demo.path("config.toml");
    std::os::unix::fs::symlink(demo.path("gone.toml"), &link).unwrap();

    let (code, _, err) = kipple(
        &demo.env(&[]),
        &["--config", link.to_str().unwrap(), "scan"],
    );

    assert_eq!(code, ExitCode::from(USAGE));
    assert!(err.contains("a link to nothing"), "{err}");
}

#[test]
fn an_unknown_config_directory_exits_2_unless_a_file_is_named() {
    let demo = Demo::new(&["xdg.config", "known.roaming-app-data"], "");
    let env = demo.env(&[]);
    let file = demo.config("kipple.toml", "[care]\nlevel = \"careful\"\n");

    let (code, _, err) = kipple(&env, &["scan"]);
    let (named, out, _) = kipple(
        &env,
        &["--config", file.to_str().unwrap(), "scan", "--json"],
    );

    assert_eq!(code, ExitCode::from(USAGE));
    assert!(err.contains("config directory is unknown"), "{err}");
    assert_eq!(named, ExitCode::SUCCESS);
    assert!(out.contains("\"level\": \"careful\""));
}

#[test]
fn commands_that_scan_need_a_usable_config_and_the_others_ignore_it() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let file = demo.config("bad.toml", "[care]\nlevel = \"reckless\"\n");
    let flag = file.to_str().unwrap();
    let target = demo.path("home/projects/rust-app/target");

    for args in [
        vec!["--config", flag],
        vec!["--config", flag, "scan", "--json"],
        vec!["--config", flag, "explain", target.to_str().unwrap()],
    ] {
        let (code, out, err) = kipple(&env, &args);
        assert_eq!(code, ExitCode::from(USAGE), "{args:?}");
        assert_eq!(out, "", "{args:?}");
        assert!(err.contains("line 2, column 9"), "{err}");
    }
    let cli = Cli::parse_from(["kipple", "--config", flag]);
    let mut stderr = Vec::new();
    assert_eq!(kipple::home(&cli, &env, &mut stderr), ExitCode::from(USAGE));

    // `rules` and `explain RULE` don't read it.
    for args in [
        vec!["--config", flag, "rules"],
        vec!["--config", flag, "explain", CARGO],
    ] {
        let (code, _, err) = kipple(&env, &args);
        assert_eq!(code, ExitCode::SUCCESS, "{args:?}: {err}");
    }
}

// Catches the global `--config` taking away the refusal of a home screen option given
// with a command, which clap's conflict did before.
#[test]
fn the_home_screens_options_are_refused_with_a_command() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    for args in [vec!["--ascii", "scan"], vec!["--ascii", "rules"]] {
        let (code, out, err) = kipple(&env, &args);
        assert_eq!(code, ExitCode::from(USAGE), "{args:?}");
        assert_eq!(out, "");
        assert!(err.contains("home screen"), "{err}");
    }
}

#[test]
fn doctor_and_config_say_what_is_wrong_with_the_config_and_where_it_came_from() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let file = demo.config("bad.toml", "[care]\nlevel = \"reckless\"\n");
    let flag = file.to_str().unwrap();

    let (code, out, _) = kipple(&env, &["--config", flag, "doctor"]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("--config"), "{out}");
    assert!(out.contains("line 2, column 9: care.level"), "{out}");
    let (json_code, json, _) = kipple(&env, &["--config", flag, "doctor", "--json"]);
    assert_eq!(json_code, ExitCode::SUCCESS);
    let doctor: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(doctor["config"]["source"], "--config");
    assert!(
        doctor["config"]["problem"]
            .as_str()
            .unwrap()
            .contains("line 2, column 9")
    );
    let (show_code, shown, err) = kipple(&env, &["--config", flag, "config", "show"]);
    assert_eq!(show_code, ExitCode::from(USAGE));
    assert!(shown.contains("bad.toml (from --config)"), "{shown}");
    assert!(err.contains("line 2, column 9"), "{err}");
    let (path_code, path, _) = kipple(&env, &["--config", flag, "config", "path"]);
    assert_eq!(path_code, ExitCode::from(USAGE));
    assert!(Path::new(path.trim()).ends_with("bad.toml"), "{path}");
}

#[test]
fn the_config_comes_from_the_flag_then_the_environment_then_the_default_location() {
    let demo = Demo::new(&[], "");
    demo.default_config("[care]\nlevel = \"careful\"\n");
    let named = demo.config("env.toml", "[care]\nlevel = \"thorough\"\n");
    let flagged = demo.config("flag.toml", "[care]\nlevel = \"balanced\"\n");
    let level = |env: &Environment, args: &[&str]| {
        let (code, out, err) = kipple(env, args);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        serde_json::from_str::<Value>(&out).unwrap()["level"].clone()
    };

    let plain = demo.env(&[]);
    let with_env = demo.env(&[("KIPPLE_CONFIG", &named)]);
    let flag = ["--config", flagged.to_str().unwrap(), "scan", "--json"];

    assert_eq!(level(&plain, &["scan", "--json"]), "careful");
    assert_eq!(level(&with_env, &["scan", "--json"]), "thorough");
    assert_eq!(level(&with_env, &flag), "balanced");
    // The flag works after the command too.
    let after = ["scan", "--json", "--config", flagged.to_str().unwrap()];
    assert_eq!(level(&with_env, &after), "balanced");
}

#[test]
fn scan_honours_the_configured_level_and_a_rules_own_level() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let careful = demo.config("careful.toml", "[care]\nlevel = \"careful\"\n");
    let node_careful = demo.config(
        "node.toml",
        "[rules.\"builtin.node.node-modules\"]\nlevel = \"careful\"\n",
    );
    let balanced = scan_with(&env, &demo.config("balanced.toml", ""));
    let node = |report: &Value| -> Vec<(String, String, String)> {
        findings(report)
            .iter()
            .filter(|f| f["rule"] == "builtin.node.node-modules")
            .map(|f| {
                let level = f["level"].as_str().unwrap().to_owned();
                (
                    path_of(f).display().to_string(),
                    bucket(f).to_owned(),
                    level,
                )
            })
            .collect()
    };

    // Reinstalled dependencies are Safe at Balanced and only Review at Careful. Windows
    // doesn't show the process paths they need, so there they are never Safe (04 §4).
    assert_ne!(node(&balanced).len(), 0);
    if kipple_platform::PLATFORM != kipple_core::Platform::Windows {
        assert!(node(&balanced).iter().any(|(_, b, _)| b == "safe"));
    }
    for report in [scan_with(&env, &careful), scan_with(&env, &node_careful)] {
        assert_ne!(node(&report).len(), 0);
        for (path, bucket, level) in node(&report) {
            assert_ne!(bucket, "safe", "{path}");
            assert_eq!(level, "careful", "{path}");
        }
    }
    // A rule's own level leaves every other rule at the global one.
    let report = scan_with(&env, &node_careful);
    assert_eq!(report["level"], "balanced");
    let cargo = findings(&report)
        .iter()
        .find(|f| f["rule"] == CARGO)
        .unwrap();
    assert_eq!(cargo["level"], "balanced");
}

#[test]
fn with_the_cargo_rule_turned_off_nothing_under_a_target_is_safe_or_review() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let configs = [
        demo.config(
            "exclude.toml",
            "[exclude]\nrules = [\"builtin.cargo.target\"]\n",
        ),
        demo.config(
            "disabled.toml",
            "[rules.\"builtin.cargo.target\"]\nenabled = false\n",
        ),
    ];
    for file in configs {
        let report = scan_with(&env, &file);
        let targets: Vec<&Value> = findings(&report)
            .iter()
            .filter(|f| path_of(f).components().any(|c| c.as_os_str() == "target"))
            .collect();
        // The rule still runs, so its walk still stops at each target.
        assert!(targets.iter().any(|f| f["rule"] == CARGO));
        for finding in targets {
            assert_eq!(bucket(finding), "protected", "{finding}");
        }
        for finding in findings(&report).iter().filter(|f| f["rule"] == CARGO) {
            assert!(reasons(finding).contains(&"disabled"), "{finding}");
        }
    }
}

#[test]
fn a_kept_finding_is_protected_as_kept_in_the_report_the_json_the_events_and_explain() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let target = rust_app_target(&scan_with(&env, &demo.config("empty.toml", "")));
    let text = format!(
        "[keep]\npaths = [{{ path = {:?}, note = \"benchmark baseline\" }}]\n",
        target.to_str().unwrap()
    );
    let file = demo.config("keep.toml", &text);
    let flag = file.to_str().unwrap();

    let report = scan_with(&env, &file);
    let kept = findings(&report)
        .iter()
        .find(|f| path_of(f) == target)
        .unwrap();
    assert_eq!(bucket(kept), "protected");
    assert_eq!(reasons(kept), ["kept"]);

    let (_, human, _) = kipple(&env, &["--config", flag, "scan"]);
    let line = human.lines().find(|l| l.contains("rust-app")).unwrap();
    assert!(
        line.contains("protected") && line.contains("(kept)"),
        "{line}"
    );

    let (_, events, _) = kipple(&env, &["--config", flag, "scan", "--events", "jsonl"]);
    let updated = events
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|r| {
            r["type"] == "finding-updated" && r["verdict"]["reasons"] == serde_json::json!(["kept"])
        })
        .count();
    assert_eq!(updated, 1, "{events}");

    let (_, explained, _) = kipple(
        &env,
        &["--config", flag, "explain", target.to_str().unwrap()],
    );
    assert!(
        explained.contains("Protected at the balanced level: You chose to keep it."),
        "{explained}"
    );
    assert!(explained.contains("benchmark baseline"), "{explained}");
}

#[test]
fn a_kept_path_inside_a_finding_or_with_other_case_holds_it_and_a_missing_one_is_warned_about() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let target = rust_app_target(&scan_with(&env, &demo.config("empty.toml", "")));
    // A file inside, and the project above it written with other case. The
    // other case is missing only where the file system tells case apart.
    let inside = target.join("release").join("bench-baseline");
    let project = target.parent().unwrap();
    let shouted = project.with_file_name("RUST-APP");
    for kept in [&inside, &shouted] {
        let text = format!("[keep]\npaths = [{:?}]\n", kept.to_str().unwrap());
        let file = demo.config("keep.toml", &text);

        let (code, out, err) = kipple(
            &env,
            &["--config", file.to_str().unwrap(), "scan", "--json"],
        );

        assert_eq!(code, ExitCode::SUCCESS);
        let warned = err.contains("warning: keep.paths:") && err.contains("doesn't exist");
        assert_eq!(warned, !kept.exists(), "{kept:?}: {err}");
        let report: Value = serde_json::from_str(&out).unwrap();
        let held = findings(&report)
            .iter()
            .find(|f| path_of(f) == target)
            .unwrap();
        assert_eq!(reasons(held), ["kept"], "{kept:?}");
    }
}

#[test]
fn an_excluded_project_is_not_walked_and_is_listed_as_excluded() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let target = rust_app_target(&scan_with(&env, &demo.config("empty.toml", "")));
    let project = target.parent().unwrap().to_owned();
    let text = format!("[exclude]\npaths = [{:?}]\n", project.to_str().unwrap());
    let report = scan_with(&env, &demo.config("exclude.toml", &text));

    assert!(
        findings(&report)
            .iter()
            .all(|f| !path_of(f).starts_with(&project))
    );
    let omissions: Vec<&Value> = report["integrations"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["omissions"].as_array().unwrap())
        .filter(|o| o["kind"] == "excluded")
        .collect();
    assert_ne!(omissions.len(), 0);
    for omission in omissions {
        assert_eq!(
            PathBuf::from(omission["path"]["display"].as_str().unwrap()),
            project
        );
        assert!(
            omission["message"]
                .as_str()
                .unwrap()
                .contains("excluded by config")
        );
    }
}

#[test]
fn an_excluded_path_inside_a_finding_holds_the_finding_above_it() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let target = rust_app_target(&scan_with(&env, &demo.config("empty.toml", "")));
    let text = format!(
        "[exclude]\npaths = [{:?}]\n",
        target.join("release").to_str().unwrap()
    );

    let report = scan_with(&env, &demo.config("exclude.toml", &text));

    let held = findings(&report)
        .iter()
        .find(|f| path_of(f) == target)
        .unwrap();
    assert_eq!(reasons(held), ["excluded"]);
}

#[test]
fn a_tilde_path_under_an_unknown_home_holds_every_finding_as_excluded() {
    let demo = Demo::new(&["home"], "");
    let env = demo.env(&[]);
    let file = demo.config("keep.toml", "[keep]\npaths = [\"~/projects/rust-app\"]\n");

    let (code, out, err) = kipple(
        &env,
        &["--config", file.to_str().unwrap(), "scan", "--json"],
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(err.contains("home directory is unknown"), "{err}");
    let report: Value = serde_json::from_str(&out).unwrap();
    assert_ne!(findings(&report).len(), 0);
    for finding in findings(&report).iter().filter(|f| !f["verdict"].is_null()) {
        assert!(reasons(finding).contains(&"excluded"), "{finding}");
    }
}

#[test]
fn the_configs_project_roots_replace_the_defaults_and_a_missing_one_is_an_omission() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let text = "[scan]\nproject_roots = [\"~/projects/web\", \"~/projects/gone\"]\n";
    let report = scan_with(&env, &demo.config("roots.toml", text));
    let web = demo.path("home/projects/web");

    let dev: Vec<PathBuf> = findings(&report)
        .iter()
        .filter(|f| f["lens"] == "dev")
        .map(path_of)
        .collect();
    assert_ne!(dev.len(), 0);
    assert!(
        dev.iter().all(|p| p.ends_with("web/node_modules")),
        "{dev:?}"
    );
    let unresolved = report["integrations"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["omissions"].as_array().unwrap())
        .any(|o| o["kind"] == "root-unresolved" && o["message"].as_str().unwrap().contains("gone"));
    assert!(unresolved, "{report}");
    // `--root` replaces them in turn.
    let (_, out, _) = kipple(
        &env,
        &[
            "--config",
            demo.path("roots.toml").to_str().unwrap(),
            "scan",
            "--json",
            "--root",
            demo.path("home/projects/tool").to_str().unwrap(),
        ],
    );
    let given: Value = serde_json::from_str(&out).unwrap();
    assert!(
        findings(&given)
            .iter()
            .filter(|f| f["lens"] == "dev")
            .all(|f| !path_of(f).starts_with(&web))
    );
}

// Catches an empty list (onboarding with every root unchecked, or settings after the
// last root was removed) bringing the default project roots back while the screen shows
// none.
#[test]
fn an_empty_list_of_project_roots_scans_no_project_and_only_root_replaces_it() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let file = demo.config(
        "roots.toml",
        "[scan]\nproject_roots = [\"~/projects/web\"]\n",
    );
    let at = location(&file);
    let roots = kipple_platform::resolve_roots(&env, &[]);
    // The write the first run and the settings panel make, and the settings the home
    // screen rescans with after it.
    let config = config::write(&at, &[Edit::ProjectRoots(Vec::new())], &roots, &catalog()).unwrap();
    assert!(
        fs::read_to_string(&file)
            .unwrap()
            .contains("project_roots = []")
    );
    let loaded = config::Loaded {
        location: at,
        exists: true,
        config,
    };
    let settings = config::Settings::from_loaded(&env, loaded, &[]).unwrap();
    assert!(settings.roots.all(SymbolicRoot::ProjectRoots).is_empty());

    let dev = |report: &Value| {
        findings(report)
            .iter()
            .filter(|f| f["lens"] == "dev")
            .map(path_of)
            .collect::<Vec<_>>()
    };
    assert_eq!(dev(&scan_with(&env, &file)), Vec::<PathBuf>::new());
    // `--root` still replaces it.
    let (code, out, err) = kipple(
        &env,
        &[
            "--config",
            file.to_str().unwrap(),
            "scan",
            "--json",
            "--root",
            demo.path("home/projects/web").to_str().unwrap(),
        ],
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    let given = dev(&serde_json::from_str(&out).unwrap());
    assert_ne!(given.len(), 0);
    assert!(
        given.iter().all(|p| p.ends_with("web/node_modules")),
        "{given:?}"
    );
}

#[test]
#[cfg(unix)]
fn a_kept_path_written_through_a_linked_root_holds_the_finding_where_the_link_leads() {
    let demo = Demo::new(&["config.project-roots"], "");
    let real = demo.path("home/projects");
    let link = demo.path("home/code");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut roots = fs::read_to_string(&demo.roots_file).unwrap();
    writeln!(roots, "config.project-roots = {}", link.display()).unwrap();
    fs::write(&demo.roots_file, roots).unwrap();
    let env = demo.env(&[]);
    let text = format!(
        "[keep]\npaths = [{:?}]\n",
        link.join("rust-app/target").to_str().unwrap()
    );

    let report = scan_with(&env, &demo.config("keep.toml", &text));

    let target = rust_app_target(&report);
    assert!(target.starts_with(fs::canonicalize(&real).unwrap()) || target.starts_with(&real));
    let held = findings(&report)
        .iter()
        .find(|f| path_of(f) == target)
        .unwrap();
    assert_eq!(reasons(held), ["kept"]);
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn catalog() -> config::Catalog {
    config::catalog(&Builtin::load().unwrap())
}

fn location(path: &Path) -> Location {
    Location {
        source: Source::Flag,
        path: Some(path.to_owned()),
    }
}

#[test]
fn a_rule_of_any_platform_can_be_named_so_a_synced_config_loads_everywhere() {
    let ids: Vec<String> = catalog().iter().map(|id| format!("{:?}", id.0)).collect();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, format!("[exclude]\nrules = [{}]\n", ids.join(", "))).unwrap();

    let loaded = config::load(&location(&file), &catalog()).unwrap();

    assert_eq!(loaded.config.exclude_rules.len(), ids.len());
    assert!(ids.iter().any(|id| id.contains("pacman")));
}

#[test]
fn an_edit_keeps_comments_and_every_other_key_and_survives_an_editors_change() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(
        &file,
        "# my settings\n[care]\nlevel = \"careful\" # for now\n\n[exclude]\nrules = [\"builtin.cargo.target\"]\n",
    )
    .unwrap();
    let at = location(&file);
    config::load(&at, &catalog()).unwrap();
    // An editor changes the file after kipple loaded it.
    let edited = fs::read_to_string(&file).unwrap()
        + "\n# added in an editor\n[clean]\nmethod = \"trash\"\n";
    fs::write(&file, edited).unwrap();

    let written = config::write(
        &at,
        &[
            Edit::Level(Level::Thorough),
            Edit::Keep {
                path: dir.path().join("p/target"),
                note: Some("baseline".to_owned()),
            },
            Edit::RuleEnabled {
                rule: RuleId("builtin.node.node-modules".to_owned()),
                enabled: false,
            },
            Edit::RuleEnabled {
                rule: RuleId(CARGO.to_owned()),
                enabled: true,
            },
        ],
        &Roots::default(),
        &catalog(),
    )
    .unwrap();

    let text = fs::read_to_string(&file).unwrap();
    for kept in [
        "# my settings",
        "# added in an editor",
        "method = \"trash\"",
        "note = \"baseline\"",
    ] {
        assert!(text.contains(kept), "{kept}: {text}");
    }
    assert_eq!(written.level, Some(Level::Thorough));
    assert_eq!(written.method, Some(kipple_core::Method::Trash));
    assert_eq!(written.exclude_rules.len(), 0);
    assert!(
        written
            .rules
            .iter()
            .any(|rule| rule.id.0 == "builtin.node.node-modules" && rule.settings.disabled)
    );
    assert_eq!(written.keep.len(), 1);
    assert_eq!(config::load(&at, &catalog()).unwrap().config, written);

    let undone = config::write(
        &at,
        &[
            Edit::Unkeep(dir.path().join("p/target")),
            Edit::RuleEnabled {
                rule: RuleId("builtin.node.node-modules".to_owned()),
                enabled: true,
            },
        ],
        &Roots::default(),
        &catalog(),
    )
    .unwrap();
    assert_eq!(undone.keep.len(), 0);
    assert_eq!(undone.rules.len(), 0);
}

/// A wrong `[ui]` value silently ignored would draw a screen the user didn't ask for.
#[test]
fn every_ui_value_kipple_cant_use_is_an_error_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    let cases = [
        // A theme is a file name, so a name that can't be one is refused here; whether
        // the theme exists is only asked by what draws.
        (
            "[ui]\ntheme = \"../dusk\"",
            "line 2, column 9: ui.theme: expected a theme name",
        ),
        ("[ui]\ntheme = \"Dusk\"", "ui.theme: expected a theme name"),
        (
            "[ui]\ndetail = \"left\"",
            "ui.detail: expected auto, right, bottom or off",
        ),
        ("[ui]\nbars = \"no\"", "ui.bars: expected true or false"),
        ("[ui]\nsummary = 0", "ui.summary: expected true or false"),
        (
            "[ui]\nglyphs = \"emoji\"",
            "ui.glyphs: expected unicode or ascii",
        ),
    ];
    for (text, expected) in cases {
        fs::write(&file, text).unwrap();

        let error = config::load(&location(&file), &catalog()).unwrap_err();

        assert!(error.to_string().contains(expected), "{text}: {error}");
    }
}

/// `[ui]` read into the wrong option, or defaults that don't draw the dashboard, would
/// change the home screen behind the user's back. A theme that doesn't exist must not
/// make the config unusable: only what draws looks it up.
#[test]
fn the_ui_options_read_as_written_and_default_to_the_dashboard() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "[care]\nlevel = \"careful\"\n").unwrap();
    let defaults = config::load(&location(&file), &catalog())
        .unwrap()
        .config
        .ui;
    assert_eq!(defaults, config::UiConfig::default());
    assert_eq!(
        (
            defaults.theme,
            defaults.summary,
            defaults.detail,
            defaults.bars
        ),
        (None, true, DetailPlace::Auto, true)
    );
    assert_eq!(defaults.glyphs, Glyphs::Unicode);

    fs::write(
        &file,
        "[ui]\ntheme = \"no-such-theme-2\"\nsummary = false\ndetail = \"off\"\nbars = false\nglyphs = \"ascii\"\n",
    )
    .unwrap();
    let ui = config::load(&location(&file), &catalog())
        .unwrap()
        .config
        .ui;

    assert_eq!(ui.theme.as_deref(), Some("no-such-theme-2"));
    assert_eq!(
        (ui.summary, ui.detail, ui.bars, ui.glyphs),
        (false, DetailPlace::Off, false, Glyphs::Ascii)
    );
    let demo = Demo::new(&[], "");
    let named = demo.config("ui.toml", "[ui]\ntheme = \"no-such-theme\"\n");
    let (code, out, err) = kipple(
        &demo.env(&[]),
        &["--config", named.to_str().unwrap(), "scan", "--json"],
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(serde_json::from_str::<Value>(&out).is_ok());
}

/// The example in 03 §6 held keys that exit 2, so a user copying it got an unusable config.
#[test]
fn the_example_config_in_the_spec_loads() {
    let spec = include_str!("../../../docs/03-functional-spec.md");
    let example = spec
        .split_once("### Example (not the defaults)")
        .and_then(|(_, rest)| rest.split_once("```toml\n"))
        .and_then(|(_, rest)| rest.split_once("\n```"))
        .map(|(toml, _)| toml)
        .expect("03 §6 has a toml example");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, example).unwrap();

    let loaded = config::load(&location(&file), &catalog());

    assert!(loaded.is_ok(), "{loaded:?}");
}

/// Undoing a keep removed every entry equal ignoring case, so on a case-sensitive file
/// system it also unprotected a different directory the user never chose.
#[test]
fn undoing_a_keep_removes_only_the_entry_chosen_not_one_differing_by_case() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    let chosen = dir.path().join("p").join("target");
    let other = dir.path().join("p").join("Target");
    fs::write(
        &file,
        format!(
            "[keep]\npaths = ['{}', '{}']\n",
            chosen.display(),
            other.display()
        ),
    )
    .unwrap();

    let undone = config::write(
        &location(&file),
        &[Edit::Unkeep(chosen)],
        &Roots::default(),
        &catalog(),
    )
    .unwrap();

    let left: Vec<&Path> = undone
        .keep
        .iter()
        .map(|kept| Path::new(&kept.path))
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].ends_with("p/Target"), "{left:?}");
}

#[test]
fn the_first_write_creates_the_directory_and_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("kipple").join("config.toml");

    let written = config::write(
        &location(&file),
        &[Edit::Level(Level::Careful)],
        &Roots::default(),
        &catalog(),
    )
    .unwrap();

    assert_eq!(written.level, Some(Level::Careful));
    assert!(
        fs::read_to_string(&file)
            .unwrap()
            .contains("level = \"careful\"")
    );
}

#[test]
fn a_file_mid_edit_or_an_unknown_directory_refuses_the_write_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    fs::write(&file, "[care\n").unwrap();

    let refused = config::write(
        &location(&file),
        &[Edit::Level(Level::Careful)],
        &Roots::default(),
        &catalog(),
    );
    let unknown = Location {
        source: Source::Default,
        path: None,
    };
    let nowhere = config::write(
        &unknown,
        &[Edit::Level(Level::Careful)],
        &Roots::default(),
        &catalog(),
    );

    assert!(
        refused
            .unwrap_err()
            .message
            .contains("no longer reads as a valid config")
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "[care\n");
    assert!(nowhere.unwrap_err().message.contains("nothing was written"));
}

#[test]
#[cfg(unix)]
fn a_path_that_isnt_utf8_is_refused_with_a_message() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    let path = dir.path().join(OsStr::from_bytes(b"menu\xe9"));

    let refused = config::write(
        &location(&file),
        &[Edit::Keep { path, note: None }],
        &Roots::default(),
        &catalog(),
    );

    assert!(refused.unwrap_err().message.contains("isn't valid UTF-8"));
    assert!(fs::symlink_metadata(&file).is_err());
}

// Catches a config writing terminal escapes through kipple's own messages: a key name is
// copied into the error and a kept path into a warning, and TOML decodes `\u001b`.
#[test]
fn config_errors_and_warnings_never_write_a_control_character_to_the_terminal() {
    let demo = Demo::new(&[], "");
    let scanned = |file: &Path, want: ExitCode| {
        let args = ["--config", file.to_str().unwrap(), "scan", "--json"];
        let (code, _, err) = kipple(&demo.env(&[]), &args);
        assert_eq!(code, want, "{err}");
        assert!(!err.chars().any(|c| c.is_control() && c != '\n'), "{err:?}");
        err
    };
    let bad = demo.config("bad.toml", "\"\\u001b[2J\" = 1\n");
    let failed = scanned(&bad, ExitCode::from(USAGE));
    assert!(failed.contains("unknown key \\u{1b}[2J"), "{failed}");

    let kept = demo.config("kept.toml", "[keep]\npaths = [\"~/\\u001b[2J\"]\n");
    let warned = scanned(&kept, ExitCode::SUCCESS);
    assert!(
        warned.contains("warning: keep.paths: ~/\\u{1b}[2J"),
        "{warned}"
    );
}

// Catches `config show` leaving out the home screen's settings: `[ui]` with its defaults
// filled in, and each `[keys]` entry that changes a default, so a user can check what
// applies; and a `,` key (the default for settings) printed so it reads like a separator.
#[test]
fn config_show_prints_the_ui_settings_and_the_keys_that_differ_from_the_defaults() {
    let demo = Demo::new(&[], "");
    let env = demo.env(&[]);
    let show = |file: &Path| {
        let (code, shown, err) = kipple(
            &env,
            &["--config", file.to_str().unwrap(), "config", "show"],
        );
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        shown
    };
    let plain = show(&demo.config("plain.toml", ""));
    for line in [
        "ui.theme              kipple (default)",
        "ui.summary            true",
        "ui.detail             auto",
        "ui.bars               true",
        "ui.glyphs             unicode",
    ] {
        assert!(plain.lines().any(|l| l == line), "{line}:\n{plain}");
    }
    assert!(!plain.contains("keys."), "{plain}");

    let text = "[ui]\ntheme = \"dusk\"\ndetail = \"off\"\nglyphs = \"ascii\"\n\
                [keys]\nselect = [\"x\", \"space\"]\nsettings = [\",\", \"S\"]\nmove-down = [\"j\", \"down\"]\ncopy = []\n";
    let custom = show(&demo.config("custom.toml", text));
    for line in [
        "ui.theme              dusk",
        "ui.detail             off",
        "ui.glyphs             ascii",
        "keys.select           [\"x\", \"space\"]",
        "keys.settings         [\",\", \"S\"]",
        "keys.copy             none",
    ] {
        assert!(custom.lines().any(|l| l == line), "{line}:\n{custom}");
    }
    assert!(
        !custom.contains("keys.move-down"),
        "same as the default:\n{custom}"
    );
}
