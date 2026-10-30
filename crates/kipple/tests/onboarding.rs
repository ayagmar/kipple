//! The first run on `demo-home` (`docs/03-functional-spec.md` §2 J0): when it is due,
//! and that the numbers its first step shows for each level are the totals
//! `kipple scan --json` gives at that level; and the home screen's header there once a
//! config exists, and what `y` writes to the terminal (§5). Built only with the
//! `test-roots` feature:
//! every root, the config directory included, points into a temp dir.

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
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, SystemTime};

use clap::Parser as _;
use kipple::config::{self, Settings};
use kipple::setup::Builtin;
use kipple::theme::{Catalog, Colours, Glyphs, Stdout};
use kipple::tui::Appearance;
use kipple::tui::{self, Input, Key, Keys, LevelTotals, first_run_due, level_totals};
use kipple::{Cli, USAGE};
use kipple_core::{CancelToken, EventChannel, Finding, Level, ScanRequest, event_channel};
use kipple_platform::Environment;
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use serde_json::Value;

/// `demo-home` in a temp dir, with every demo root but those `left_out` names.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn demo(left_out: &[&str]) -> (tempfile::TempDir, PathBuf) {
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
    writeln!(file, "now = {seconds}").unwrap();
    let roots = temp.path().join("roots.txt");
    fs::write(&roots, file).unwrap();
    (temp, roots)
}

/// The machine `roots` describes, with these variables besides.
fn env(roots: &Path, vars: &[(&str, &Path)]) -> Environment {
    let roots = (
        OsString::from("KIPPLE_TEST_ROOTS"),
        roots.as_os_str().to_owned(),
    );
    let others = vars
        .iter()
        .map(|(name, value)| (OsString::from(name), value.as_os_str().to_owned()));
    Environment::from_vars(std::iter::once(roots).chain(others))
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn settings(env: &Environment, builtin: &Builtin) -> Settings {
    Settings::load(env, None, &[], &config::catalog(builtin)).unwrap()
}

/// The findings of a scan with `settings`, as the home screen has them once it ends.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn scan(env: &Environment, builtin: &Builtin, settings: &Settings) -> Vec<Finding> {
    let engine = builtin.engine(env).unwrap();
    let request = ScanRequest {
        roots: settings.roots.clone(),
        lens: None,
        exclude: settings.policy.exclude.clone(),
    };
    let cancel = CancelToken::new();
    let EventChannel {
        tx: events,
        rx: received,
    } = event_channel(256, &cancel);
    thread::scope(|scope| {
        let scan = scope.spawn(|| {
            let events = events;
            engine.scan(&request, &events, &cancel)
        });
        received.for_each(drop);
        scan.join().unwrap().findings
    })
}

/// The Safe and Review totals of `kipple scan --json` at `level`.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn json_totals(env: &Environment, dir: &Path, level: Level) -> LevelTotals {
    let file = dir.join(format!("{}.toml", level.name()));
    fs::write(&file, format!("[care]\nlevel = \"{}\"\n", level.name())).unwrap();
    let cli = Cli::parse_from([
        "kipple",
        "--config",
        file.to_str().unwrap(),
        "scan",
        "--json",
    ]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = kipple::run(&cli, env, Stdout::Redirected, &mut out, &mut err);
    assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
    let report: Value = serde_json::from_slice(&out).unwrap();
    let mut totals = LevelTotals::default();
    for finding in report.get("findings").and_then(Value::as_array).unwrap() {
        let bytes = finding
            .pointer("/size/allocated")
            .and_then(Value::as_u64)
            .or_else(|| finding.pointer("/size/apparent").and_then(Value::as_u64))
            .unwrap_or(0);
        match finding.pointer("/verdict/bucket").and_then(Value::as_str) {
            Some("safe") => totals.safe += bytes,
            Some("review") => totals.review += bytes,
            _ => {}
        }
    }
    totals
}

// Catches the first step's numbers being judged another way than every other frontend
// judges, so a level shows more Safe there than `kipple scan` would offer.
#[test]
fn the_first_steps_numbers_are_the_scan_json_totals_at_each_level() {
    let (temp, roots) = demo(&[]);
    let env = env(&roots, &[]);
    let builtin = Builtin::load().unwrap();
    let settings = settings(&env, &builtin);
    assert!(first_run_due(&settings.loaded), "demo-home has no config");
    let findings = scan(&env, &builtin, &settings);

    let mut shown = Vec::new();
    for &level in Level::ALL {
        let live = level_totals(&findings, &settings.policy, level);
        assert_eq!(live, json_totals(&env, temp.path(), level), "{level}");
        shown.push(live);
    }
    assert!(shown.iter().any(|totals| totals.safe > 0), "{shown:?}");
}

// Catches the first run opening over a config the user already has, or one named in the
// environment.
#[test]
fn the_first_run_is_not_due_once_a_config_file_exists_or_one_is_named() {
    let (temp, roots) = demo(&[]);
    let builtin = Builtin::load().unwrap();
    let named = temp.path().join("named.toml");
    fs::write(&named, "").unwrap();
    let with_var = env(&roots, &[(config::VAR, &named)]);
    assert!(!first_run_due(&settings(&with_var, &builtin).loaded));

    let plain = env(&roots, &[]);
    let file = settings(&plain, &builtin).loaded.location.path.unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "").unwrap();

    assert!(!first_run_due(&settings(&plain, &builtin).loaded));
}

// Catches the home screen opening, and asking the first run's questions it can never
// save, when kipple's config directory is unknown.
#[test]
fn with_an_unknown_config_directory_the_home_screen_exits_2_before_opening() {
    let (_temp, roots) = demo(&["xdg.config", "known.roaming-app-data"]);
    let cli = Cli::parse_from(["kipple"]);
    let mut err = Vec::new();

    let code = kipple::home(&cli, &env(&roots, &[]), &mut err);

    assert_eq!(code, ExitCode::from(USAGE));
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("config directory is unknown"), "{err}");
}

/// One `q` once the first frame is up.
struct Quit {
    asked: bool,
}

impl Keys for Quit {
    fn next(&mut self, _timeout: Duration) -> io::Result<Option<Input>> {
        let first = !self.asked;
        self.asked = true;
        Ok(first.then_some(Input::Key(Key::Char('q'))))
    }
}

// Catches the project roots missing from the header, or squeezed off it, on an
// 80-column terminal once a config exists and the home screen opens without questions.
#[test]
fn on_demo_home_the_header_names_the_project_root_at_80_by_24() {
    let (temp, roots) = demo(&[]);
    let named = temp.path().join("named.toml");
    fs::write(&named, "").unwrap();
    let env = env(&roots, &[(config::VAR, &named)]);
    let builtin = Builtin::load().unwrap();
    let settings = settings(&env, &builtin);
    let mut terminal = Terminal::new(Recorded {
        screen: TestBackend::new(80, 24),
        written: Vec::new(),
    })
    .unwrap();
    let appearance =
        Appearance::new(Catalog::builtin(), "kipple", Colours::Off, Glyphs::Ascii).unwrap();

    tui::run(
        &mut terminal,
        &mut Quit { asked: false },
        &env,
        &settings,
        appearance,
    )
    .unwrap();

    let buffer = terminal.backend().screen.buffer();
    let header: String = (0..80)
        .map(|x| buffer.cell((x, 0)).map_or(" ", |cell| cell.symbol()))
        .collect();
    // The title line sits in the frame's top edge. However the temp dir is spelled on
    // this system, the root ends in `projects`.
    let title = header.trim_end_matches(['-', '+', ' ']);
    assert!(title.ends_with("projects"), "{header}");
    assert!(title.starts_with("+ kipple - Balanced - "), "{header}");
}

/// A test terminal that also keeps what the home screen writes besides its frames.
struct Recorded {
    screen: TestBackend,
    written: Vec<u8>,
}

impl io::Write for Recorded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.written.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Backend for Recorded {
    type Error = <TestBackend as Backend>::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.screen.draw(content)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.screen.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.screen.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.screen.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.screen.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.screen.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.screen.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.screen.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.screen.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.screen.flush()
    }
}

/// These keys, pressed once the scan has ended, which the loop shows by waiting for a
/// key longer than a frame.
struct AfterScan {
    keys: Vec<Key>,
}

impl Keys for AfterScan {
    fn next(&mut self, timeout: Duration) -> io::Result<Option<Input>> {
        if timeout <= Duration::from_millis(16) {
            return Ok(None);
        }
        Ok(Some(Input::Key(if self.keys.is_empty() {
            Key::Char('q')
        } else {
            self.keys.remove(0)
        })))
    }
}

/// `encoded` from base64.
#[expect(clippy::unwrap_used, reason = "a malformed escape fails the test")]
fn unbase64(encoded: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0_u32;
    let mut count = 0;
    let mut bytes = Vec::new();
    for symbol in encoded.bytes().take_while(|&symbol| symbol != b'=') {
        let value = ALPHABET.iter().position(|&s| s == symbol).unwrap();
        bits = (bits << 6) | u32::try_from(value).unwrap();
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push(u8::try_from((bits >> count) & 0xff).unwrap());
        }
    }
    bytes
}

// Catches `y` never reaching the terminal (the escape left unwritten, or written
// somewhere other than the terminal's one writer) and the loop sending a path other
// than the focused finding's.
#[test]
fn y_on_demo_home_writes_the_focused_path_to_the_terminal_as_osc_52() {
    let (temp, roots) = demo(&[]);
    let named = temp.path().join("named.toml");
    fs::write(&named, "").unwrap();
    let env = env(&roots, &[(config::VAR, &named)]);
    let builtin = Builtin::load().unwrap();
    let settings = settings(&env, &builtin);
    let backend = Recorded {
        screen: TestBackend::new(160, 48),
        written: Vec::new(),
    };
    let mut terminal = Terminal::new(backend).unwrap();
    let mut keys: Vec<Key> = "/rust-app/target".chars().map(Key::Char).collect();
    keys.extend([Key::Enter, Key::Char('g')]);
    keys.extend([Key::Char('j'); 2]);
    keys.push(Key::Char('y'));
    let appearance =
        Appearance::new(Catalog::builtin(), "kipple", Colours::Off, Glyphs::Ascii).unwrap();

    tui::run(
        &mut terminal,
        &mut AfterScan { keys },
        &env,
        &settings,
        appearance,
    )
    .unwrap();

    let written = &terminal.backend().written;
    let text = String::from_utf8_lossy(written);
    let payload = text
        .strip_prefix("\u{1b}]52;c;")
        .and_then(|rest| rest.strip_suffix('\u{7}'))
        .unwrap_or_else(|| panic!("not one OSC 52 escape: {text:?}"));
    let sent = String::from_utf8(unbase64(payload)).unwrap();
    assert!(
        Path::new(&sent).ends_with("projects/rust-app/target"),
        "{sent}"
    );
}
