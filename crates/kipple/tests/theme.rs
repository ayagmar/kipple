//! Themes end to end on `demo-home` (D-055, `docs/03-functional-spec.md` §3 and §5):
//! `kipple theme`, user themes in the config directory, `--theme` and `--color`, colour
//! in human output only, and what a broken theme does to each command. Built only with
//! the `test-roots` feature: every root, the config directory included, points into a
//! temp dir.

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
use kipple::theme::Themes;
use kipple::{Cli, USAGE};
use kipple_platform::Environment;

const BROKEN: &str = "version = 1\nextends = \"kipple\"\n[roles]\nsafe = { fg = \"grean\" }\n";

/// `demo-home` in a temp dir, and its environment with `vars` besides.
struct Demo {
    temp: tempfile::TempDir,
    roots: PathBuf,
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
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

    fn env(&self, vars: &[(&str, &str)]) -> Environment {
        let roots = (
            OsString::from("KIPPLE_TEST_ROOTS"),
            self.roots.clone().into_os_string(),
        );
        let others = vars
            .iter()
            .map(|&(k, v)| (OsString::from(k), OsString::from(v)));
        Environment::from_vars(std::iter::once(roots).chain(others))
    }

    /// The user theme directory, as kipple finds it.
    fn themes(&self) -> PathBuf {
        Themes::new(&self.env(&[])).dir().unwrap().to_owned()
    }

    /// Writes the user theme `name`.
    fn theme(&self, name: &str, text: &str) -> PathBuf {
        let dir = self.themes();
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{name}.toml"));
        fs::write(&file, text).unwrap();
        file
    }

    /// Writes a config file naming the theme `name` and returns its path.
    fn config(&self, name: &str) -> PathBuf {
        let file = self.temp.path().join("config.toml");
        fs::write(&file, format!("[ui]\ntheme = \"{name}\"\n")).unwrap();
        file
    }
}

/// Runs `kipple <args>`, its stdout a terminal or not, and returns its exit code, stdout
/// and stderr.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn kipple(env: &Environment, terminal: bool, args: &[&str]) -> (ExitCode, String, String) {
    let cli = Cli::parse_from(std::iter::once("kipple").chain(args.iter().copied()));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = kipple::run(&cli, env, terminal, &mut out, &mut err);
    let text = |bytes| String::from_utf8(bytes).unwrap();
    (code, text(out), text(err))
}

fn home(env: &Environment, args: &[&str]) -> (ExitCode, String) {
    let cli = Cli::parse_from(std::iter::once("kipple").chain(args.iter().copied()));
    let mut err = Vec::new();
    let code = kipple::home(&cli, env, &mut err);
    (code, String::from_utf8_lossy(&err).into_owned())
}

/// The lines of a theme file after its comment.
fn body(text: &str) -> String {
    text.lines()
        .skip_while(|line| line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

// Catches `theme show` printing something that doesn't make the same theme when saved as
// a user theme, and `theme list` or `theme path` not pointing at where it goes.
#[test]
fn a_shown_theme_saved_as_your_own_is_the_same_theme_and_is_listed() {
    let demo = Demo::new();
    let env = demo.env(&[]);
    let (path_code, path, _) = kipple(&env, false, &["theme", "path"]);
    assert_eq!(path_code, ExitCode::SUCCESS);
    assert_eq!(Path::new(path.trim_end()), demo.themes());

    let (show_code, shown, _) = kipple(&env, false, &["theme", "show", "kipple-dark"]);
    assert_eq!(show_code, ExitCode::SUCCESS);
    let file = demo.theme("copy", &shown);
    let (_, again, _) = kipple(&env, false, &["theme", "show", "copy"]);
    assert_eq!(body(&again), body(&shown));

    let (list_code, list, _) = kipple(&env, false, &["theme", "list"]);
    assert_eq!(list_code, ExitCode::SUCCESS);
    let copy = list.lines().find(|line| line.starts_with("copy ")).unwrap();
    assert!(copy.ends_with(&file.display().to_string()), "{list}");
    assert!(
        list.lines()
            .any(|l| l.starts_with("kipple ") && l.contains("the default"))
    );
}

// Catches a theme name as wide as the name column running into what `theme list` says
// of it.
#[test]
fn a_long_theme_name_is_set_apart_in_the_list() {
    let demo = Demo::new();
    let env = demo.env(&[]);
    demo.theme("solarized-autumn", "version = 1\nextends = \"kipple\"\n");

    let (code, list, _) = kipple(&env, false, &["theme", "list"]);

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(
        list.lines()
            .any(|line| line.starts_with("solarized-autumn ")),
        "{list}"
    );
}

// Catches a broken theme reaching data output: `scan --json` reads no theme, so its
// output and exit code are the same as with no theme at all, even in colour.
#[test]
fn a_broken_theme_never_changes_scan_json() {
    let demo = Demo::new();
    let env = demo.env(&[]);
    let plain = demo.temp.path().join("plain.toml");
    fs::write(&plain, "").unwrap();
    let (code, want, _) = kipple(
        &env,
        true,
        &["--config", plain.to_str().unwrap(), "scan", "--json"],
    );
    assert_eq!(code, ExitCode::SUCCESS);

    demo.theme("dusk", BROKEN);
    let config = demo.config("dusk");
    let flag = config.to_str().unwrap();
    for args in [
        vec!["--config", flag, "scan", "--json"],
        vec!["--config", flag, "--color", "always", "scan", "--json"],
        vec!["--config", flag, "--theme", "nowhere", "scan", "--json"],
    ] {
        let (themed, got, err) = kipple(&env, true, &args);
        assert_eq!(themed, ExitCode::SUCCESS, "{args:?}: {err}");
        assert_eq!(err, "", "{args:?}");
        assert!(!got.contains('\x1b'), "{args:?}");
        assert_eq!(got, want, "{args:?}");
    }
}

// Catches a broken theme failing a human command or warning more than once, and the
// warning not saying where the mistake is: it warns once, uses kipple, and exits 0.
#[test]
fn a_broken_theme_warns_once_in_human_output_and_kipple_draws_instead() {
    let demo = Demo::new();
    let file = demo.theme("dusk", BROKEN);
    let env = demo.env(&[]);
    let config = demo.config("dusk");
    let (code, out, err) = kipple(&env, true, &["--config", config.to_str().unwrap(), "scan"]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(err.matches("warning: theme dusk:").count(), 1, "{err}");
    assert!(err.contains(&file.display().to_string()), "{err}");
    assert!(
        err.contains("line 4, column 15") && err.trim_end().ends_with("using kipple"),
        "{err}"
    );
    assert!(out.contains("\x1b[32m"), "kipple's green for Safe:\n{out}");
}

// Catches colour in piped output, or colour that NO_COLOR, `--color never` or
// `--no-color` don't turn off; and `--color always` not turning it on when piped.
#[test]
fn colour_follows_the_flag_then_no_color_then_the_terminal() {
    let demo = Demo::new();
    let plain = demo.env(&[]);
    let no_color = demo.env(&[("NO_COLOR", "1")]);
    let cases: [(&Environment, bool, &[&str], bool); 7] = [
        (&plain, true, &["scan"], true),
        (&plain, false, &["scan"], false),
        (&no_color, true, &["scan"], false),
        (&plain, true, &["--color", "never", "scan"], false),
        (&plain, true, &["--no-color", "scan"], false),
        (&plain, false, &["--color", "always", "scan"], true),
        (&no_color, false, &["--color", "always", "rules"], true),
    ];
    for (env, terminal, args, coloured) in cases {
        let (code, out, err) = kipple(env, terminal, args);
        assert_eq!(code, ExitCode::SUCCESS, "{args:?}: {err}");
        assert_eq!(
            out.contains('\x1b'),
            coloured,
            "{args:?} terminal={terminal}"
        );
        // The words are the same either way.
        assert!(out.contains("System"), "{args:?}");
    }
    let (_, coloured, _) = kipple(&plain, true, &["scan"]);
    let (_, piped, _) = kipple(&plain, false, &["scan"]);
    let stripped = strip_sgr(&coloured);
    assert_eq!(stripped, piped);
}

/// `text` without its SGR escapes.
fn strip_sgr(text: &str) -> String {
    let mut out = String::new();
    let mut escape = false;
    for c in text.chars() {
        match (escape, c) {
            (false, '\x1b') => escape = true,
            (false, c) => out.push(c),
            (true, 'm') => escape = false,
            (true, _) => {}
        }
    }
    out
}

// Catches the home screen opening with a theme it can't draw, or failing without saying
// where: it exits 2 with the file, line and column before taking over the terminal.
#[test]
fn the_home_screen_refuses_a_theme_it_cant_use_with_where_and_exit_2() {
    let demo = Demo::new();
    let file = demo.theme("dusk", BROKEN);
    let env = demo.env(&[]);
    let config = demo.config("dusk");
    let flag = config.to_str().unwrap();

    let (broken, why) = home(&env, &["--config", flag]);
    assert_eq!(broken, ExitCode::from(USAGE));
    assert!(why.contains(&file.display().to_string()), "{why}");
    assert!(why.contains("line 4, column 15"), "{why}");

    let (missing, why_missing) = home(&env, &["--config", flag, "--theme", "nowhere"]);
    assert_eq!(missing, ExitCode::from(USAGE));
    assert!(
        why_missing.contains("there is no theme nowhere"),
        "{why_missing}"
    );

    let (shown, _, why_shown) = kipple(&env, false, &["theme", "show", "dusk"]);
    assert_eq!(shown, ExitCode::from(USAGE));
    assert!(why_shown.contains("line 4, column 15"), "{why_shown}");
}

// Catches `ui.theme` accepting a name no theme file can have, or a missing theme
// failing the config load: only the name is checked there.
#[test]
fn ui_theme_is_checked_as_a_name_and_only_drawing_looks_it_up() {
    let demo = Demo::new();
    let env = demo.env(&[]);
    let bad = demo.temp.path().join("bad.toml");
    fs::write(&bad, "[ui]\ntheme = \"Dusk\"\n").unwrap();
    let (code, _, err) = kipple(
        &env,
        false,
        &["--config", bad.to_str().unwrap(), "scan", "--json"],
    );
    assert_eq!(code, ExitCode::from(USAGE));
    assert!(
        err.contains("line 2, column 9") && err.contains("ui.theme"),
        "{err}"
    );

    let missing = demo.config("nowhere");
    let (scanned, _, warned) = kipple(
        &env,
        false,
        &["--config", missing.to_str().unwrap(), "scan"],
    );
    assert_eq!(scanned, ExitCode::SUCCESS, "{warned}");
    assert_eq!(warned, "", "piped output reads no theme");
}

// Catches doctor hiding a broken theme, or `doctor --json` reading theme files.
#[test]
fn doctor_lists_a_broken_theme_and_its_json_reads_none() {
    let demo = Demo::new();
    demo.theme("dusk", BROKEN);
    let env = demo.env(&[]);
    let config = demo.config("dusk");
    let flag = config.to_str().unwrap();
    let (_, out, _) = kipple(&env, false, &["--config", flag, "doctor"]);
    assert!(out.contains("dusk: can't be used"), "{out}");
    assert!(out.contains("line 4, column 15"), "{out}");
    let (_, json, _) = kipple(&env, false, &["--config", flag, "doctor", "--json"]);
    assert!(!json.contains("dusk"), "{json}");
}

// Catches a shared theme writing terminal escapes through kipple's own messages: a role
// name is copied into the error, and TOML decodes `\u001b` in a quoted key. Every place
// that reports a theme error writes it escaped.
#[test]
fn a_theme_error_never_writes_a_control_character_to_the_terminal() {
    let demo = Demo::new();
    demo.theme(
        "evil",
        "version = 1\n[roles]\n\"\\u001b]52;c;cm0gLXJm\\u0007\" = {}\n",
    );
    let env = demo.env(&[]);
    let (shown, _, show_err) = kipple(&env, false, &["theme", "show", "evil"]);
    let (_, _, rules_err) = kipple(
        &env,
        false,
        &["--color", "always", "--theme", "evil", "rules"],
    );
    let (opened, home_err) = home(&env, &["--theme", "evil"]);
    assert_eq!(shown, ExitCode::from(USAGE));
    assert_eq!(opened, ExitCode::from(USAGE));
    for err in [show_err, rules_err, home_err] {
        assert!(err.contains("unknown role"), "{err}");
        assert!(!err.chars().any(|c| c.is_control() && c != '\n'), "{err:?}");
        assert!(err.contains("\\u{1b}]52;c;cm0gLXJm\\u{7}"), "{err}");
    }
}
