//! A clean from the home screen end to end (`docs/03-functional-spec.md` §2 J3, spec
//! M3c pinned 4–9 and 18): the real loop, its workers and the real executor on
//! `demo-home`, driven by scripted keys on a test terminal, every root redirected into a
//! temp dir through the hidden test-roots override. Nothing here reaches the real home,
//! trash or data directory.
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

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead, Cursor, Read};
use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use clap::Parser as _;
use kipple::config::{self, Settings};
use kipple::setup::Builtin;
use kipple::theme::{Catalog, Colours, Glyphs};
use kipple::tui::{self, Appearance, Input, Key, Keys};
use kipple::{Answers, Cli, Io, Person};
use kipple_core::{CancelToken, Clock, RunStore as _, Waited};
use kipple_platform::{Environment, FileRunStore};
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use tempfile::TempDir;

const ACME: &str = "home/projects/clients/acme/api/target";
const RUST_APP: &str = "home/projects/rust-app/target";
/// Recent at Balanced: a Review finding, never cleaned by the command line.
const WEB: &str = "home/projects/web/node_modules";
/// Wide enough that no line of a clean's screens breaks.
const WIDTH: u16 = 200;
const HEIGHT: u16 = 60;

/// `demo-home` in a temp dir, the roots file that points every root into it, and an
/// empty config file of its own.
struct Demo {
    temp: TempDir,
    roots: PathBuf,
    config: PathBuf,
}

impl Demo {
    fn new() -> Self {
        Self::without(&[])
    }

    /// The demo with the roots `left_out` unknown.
    fn without(left_out: &[&str]) -> Self {
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
        let config = temp.path().join("config.toml");
        fs::write(&config, "").unwrap();
        Self {
            temp,
            roots,
            config,
        }
    }

    fn at(&self, relative: &str) -> PathBuf {
        self.temp.path().join(relative)
    }

    /// The machine the roots file describes, with its config named by `KIPPLE_CONFIG`,
    /// and `PATH`, so git can be asked.
    fn env(&self) -> Environment {
        let mut vars = vec![
            (
                OsString::from("KIPPLE_TEST_ROOTS"),
                self.roots.clone().into_os_string(),
            ),
            (
                OsString::from(config::VAR),
                self.config.clone().into_os_string(),
            ),
        ];
        vars.extend(std::env::var_os("PATH").map(|path| (OsString::from("PATH"), path)));
        Environment::from_vars(vars)
    }

    /// kipple's data directory in the demo, as the run store has it on this system.
    fn data(&self) -> PathBuf {
        if cfg!(windows) {
            self.at("home/AppData/Roaming/kipple/data")
        } else {
            self.at("home/.local/share/kipple")
        }
    }

    /// Opens the home screen on a test terminal and presses `script` in turn, then `q`,
    /// returning each frame the script looked at and the last one drawn.
    fn home(&self, script: Vec<Step>) -> Frames {
        let builtin = Builtin::load().unwrap();
        let env = self.env();
        let settings = Settings::load(&env, None, &[], &config::catalog(&builtin)).unwrap();
        let screen = Rc::new(RefCell::new(TestBackend::new(WIDTH, HEIGHT)));
        let mut terminal = Terminal::new(Shared(Rc::clone(&screen))).unwrap();
        let mut keys = Script {
            steps: script.into(),
            screen: Rc::clone(&screen),
            looked: Vec::new(),
        };
        let appearance =
            Appearance::new(Catalog::builtin(), "kipple", Colours::Off, Glyphs::Ascii).unwrap();

        tui::run(&mut terminal, &mut keys, &env, &settings, appearance).unwrap();

        assert!(
            keys.steps.is_empty(),
            "the screen closed before the script ended"
        );
        let last = text(&screen.borrow());
        Frames {
            looked: keys.looked,
            last,
        }
    }

    /// Runs `kipple <args>` in this process as a person at a terminal who answers
    /// `answer`.
    fn answer(&self, args: &[&str], answer: &str) -> Ran {
        let cli = Cli::parse_from(std::iter::once("kipple").chain(args.iter().copied()));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut io = Io::redirected(&mut out, &mut err);
        io.person = Person::AtTerminal;
        io.stdin = Box::new(Answer(Cursor::new(answer.as_bytes().to_vec())));
        let code = kipple::run(&cli, &self.env(), io);
        Ran {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }
}

/// What a run of `kipple` in this process did.
struct Ran {
    code: ExitCode,
    out: String,
    err: String,
}

/// What a person does next at the home screen.
enum Step {
    /// Presses this key once the screen is idle: the scan, planning or run over.
    Press(Key),
    /// Keeps the frame drawn so far.
    Look,
    /// Does this while the screen is idle, as something else on the machine would.
    Meanwhile(Box<dyn FnOnce()>),
}

fn press(keys: &[Key]) -> Vec<Step> {
    keys.iter().copied().map(Step::Press).collect()
}

fn typed(text: &str) -> Vec<Step> {
    text.chars().map(|c| Step::Press(Key::Char(c))).collect()
}

/// Where the one finding a filter leaves is in the tree.
#[derive(Clone, Copy)]
enum Bucket {
    /// Under the Safe heading and its group.
    Safe,
    /// Under the empty Safe heading, then the Review heading and its group.
    Review,
}

/// `/` with `query` and `enter`, then the cursor on the one finding left, in `bucket`.
fn filtered_to(query: &str, bucket: Bucket) -> Vec<Step> {
    let mut steps = press(&[Key::Char('/')]);
    steps.extend(typed(query));
    steps.extend(press(&[Key::Enter, Key::Char('g')]));
    let down = match bucket {
        Bucket::Safe => 2,
        Bucket::Review => 3,
    };
    steps.extend((0..down).map(|_| Step::Press(Key::Char('j'))));
    steps
}

struct Script {
    steps: VecDeque<Step>,
    screen: Rc<RefCell<TestBackend>>,
    looked: Vec<String>,
}

impl Keys for Script {
    fn next(&mut self, timeout: Duration) -> io::Result<Option<Input>> {
        // The loop waits a frame at most while something runs, and longer once idle.
        if timeout <= Duration::from_millis(16) {
            return Ok(None);
        }
        loop {
            match self.steps.pop_front() {
                None => return Ok(Some(Input::Key(Key::Char('q')))),
                Some(Step::Press(key)) => return Ok(Some(Input::Key(key))),
                Some(Step::Look) => self.looked.push(text(&self.screen.borrow())),
                Some(Step::Meanwhile(act)) => act(),
            }
        }
    }
}

/// The frames a script looked at, and the last one drawn.
struct Frames {
    looked: Vec<String>,
    last: String,
}

/// `screen`'s buffer as text, each row trimmed, paths written with `/`.
fn text(screen: &TestBackend) -> String {
    let buffer = screen.buffer();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        let row: String = (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
            .collect();
        text.push_str(row.trim_end());
        text.push('\n');
    }
    text.replace(MAIN_SEPARATOR, "/")
}

/// The rows of a screen's main area, without the frame around them: what a clean's
/// screens list under their title.
fn rows(frame: &str) -> Vec<String> {
    frame
        .lines()
        .skip(1)
        .take_while(|row| !row.starts_with('+'))
        .map(|row| {
            row.trim_start_matches('|')
                .trim_end_matches('|')
                .trim()
                .to_owned()
        })
        .filter(|row| !row.is_empty())
        .collect()
}

/// A test terminal the script can look at while the loop draws on it.
struct Shared(Rc<RefCell<TestBackend>>);

impl io::Write for Shared {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Backend for Shared {
    type Error = <TestBackend as Backend>::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.0.borrow_mut().draw(content)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.0.borrow_mut().get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.0.borrow_mut().set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.0.borrow_mut().clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.0.borrow().size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.0.borrow_mut().window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().flush()
    }
}

/// A terminal a person types `answer` at, after the question.
struct Answer(Cursor<Vec<u8>>);

impl Read for Answer {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl BufRead for Answer {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.0.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.0.consume(amount);
    }
}

impl Answers for Answer {
    fn discard_typeahead(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn wait(&mut self, _: &CancelToken) -> io::Result<Waited> {
        Ok(Waited::Ready)
    }
}

/// Whether `path` is there, a link included.
fn present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

// Catches a Review finding never reaching a run from the home screen (planned Safe-only,
// as the command line plans), `y` running something other than what the review showed,
// or the result not showing what the run did.
#[test]
fn s_enter_y_on_a_review_finding_removes_it_and_the_result_shows_it() {
    let demo = Demo::new();
    let mut script = filtered_to("web/node_modules", Bucket::Review);
    script.extend(press(&[Key::Char('s'), Key::Enter]));
    script.push(Step::Look);
    script.extend(press(&[Key::Char('y')]));

    let frames = demo.home(script);

    let review = &frames.looked[0];
    assert!(review.contains("Review: clean 1 item"), "{review}");
    assert!(review.contains("~/projects/web/node_modules"), "{review}");
    assert!(review.contains("needs review: recent"), "{review}");
    assert!(!present(&demo.at(WEB)), "the reviewed finding stays");
    assert!(
        present(&demo.at(RUST_APP)),
        "a finding that wasn't selected went"
    );
    let result = &frames.last;
    assert!(result.contains("Run "), "{result}");
    let line = result
        .lines()
        .find(|row| row.contains("~/projects/web/node_modules"))
        .unwrap_or_else(|| panic!("the item isn't in the result: {result}"));
    assert!(line.contains("done"), "{line}");
    assert!(result.contains("Done."), "{result}");
}

// Catches the review and the result wording a plan or a run other than the command
// line does (pinned 18): the Safe bucket planned from the home screen reads as the
// prompt of `kipple clean` reads, and its run as the receipt of `kipple clean` reads,
// with paths below the home directory shown from `~` as the whole home screen shows
// them.
#[test]
fn the_review_and_the_result_say_what_the_command_line_says() {
    let screen = Demo::new();
    let mut script = press(&[Key::Char('c')]);
    script.push(Step::Look);
    script.extend(press(&[Key::Char('y')]));
    let frames = screen.home(script);
    let cli = Demo::new();
    let home = cli.at("home").display().to_string();
    let homed = |text: &str| text.replace(&home, "~").replace(MAIN_SEPARATOR, "/");

    let asked = cli.answer(&["clean"], "n\n");
    let cleaned = cli.answer(&["clean"], "y\n");

    assert_eq!(cleaned.code, ExitCode::SUCCESS, "{}", cleaned.err);

    let prompt: Vec<String> = homed(&asked.err)
        .lines()
        .take_while(|line| !line.starts_with("Needs review in the TUI"))
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect();
    let reviewed = rows(&frames.looked[0]);
    assert_eq!(reviewed[1..], prompt[..], "{}", asked.err);
    let receipt: Vec<String> = homed(&cleaned.out)
        .lines()
        .skip(1)
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect();
    let result = rows(&frames.last);
    assert_eq!(result[1..], receipt[..], "{}", cleaned.out);
}

// Catches apply running what the review froze over what is there now: a file changed
// inside a reviewed directory after the review opened stays, its item isn't done, and
// nothing outside the plan goes.
#[test]
fn a_file_changed_after_the_review_opened_stays_and_nothing_else_goes() {
    let demo = Demo::new();
    let changed = demo.at(ACME).join("debug/demo");
    let changing = changed.clone();
    let mut script = filtered_to("acme/api/target", Bucket::Safe);
    script.extend(press(&[Key::Char('s'), Key::Enter]));
    script.push(Step::Meanwhile(Box::new(move || {
        fs::write(&changing, "rebuilt").unwrap();
    })));
    script.extend(press(&[Key::Char('y')]));

    let frames = demo.home(script);

    assert!(present(&changed), "a file changed after the review went");
    assert!(
        present(&demo.at(RUST_APP)),
        "a finding that wasn't selected went"
    );
    let result = &frames.last;
    let line = result
        .lines()
        .find(|row| row.contains("~/projects/clients/acme/api/target"))
        .unwrap_or_else(|| panic!("the item isn't in the result: {result}"));
    assert!(line.contains("partly done"), "{line}");
}

// Catches a run judging under the policy the review was planned with after the config
// changed on disk (04 §3.6): an item whose level changed is skipped as rejudged and
// stays.
#[test]
fn a_config_edited_after_the_review_opened_skips_the_item_as_rejudged() {
    let demo = Demo::new();
    let config = demo.config.clone();
    let mut script = filtered_to("acme/api/target", Bucket::Safe);
    script.extend(press(&[Key::Char('s'), Key::Enter]));
    script.push(Step::Meanwhile(Box::new(move || {
        fs::write(&config, "[care]\nlevel = \"careful\"\n").unwrap();
    })));
    script.extend(press(&[Key::Char('y')]));

    let frames = demo.home(script);

    assert!(
        present(&demo.at(ACME).join("debug")),
        "the rejudged item went"
    );
    let result = &frames.last;
    assert!(result.contains("skipped"), "{result}");
    assert!(
        result.contains("the level changed since review"),
        "{result}"
    );
}

// Catches a config that no longer loads at confirm being ignored: nothing runs, the
// reason is shown, and the review stays to leave.
#[test]
fn a_config_made_invalid_after_the_review_opened_runs_nothing() {
    let demo = Demo::new();
    let config = demo.config.clone();
    let mut script = filtered_to("acme/api/target", Bucket::Safe);
    script.extend(press(&[Key::Char('s'), Key::Enter]));
    script.push(Step::Meanwhile(Box::new(move || {
        fs::write(&config, "[care]\nlevel = \"reckless\"\n").unwrap();
    })));
    script.extend(press(&[Key::Char('y')]));
    script.push(Step::Look);

    let frames = demo.home(script);

    assert!(present(&demo.at(ACME).join("debug")), "something ran");
    assert!(!present(&demo.data().join("runs")), "a run was stored");
    let refused = &frames.looked[0];
    assert!(refused.contains("Review: clean 1 item"), "{refused}");
    assert!(refused.contains("the config no longer loads"), "{refused}");
}

// Catches a clean with nowhere to record its run, where nothing could reconcile it
// after a crash, running anyway: with kipple's own folders unknown nothing can be
// planned, the review says why, and `y` runs nothing.
#[test]
fn with_an_unknown_data_folder_nothing_runs_and_the_review_says_why() {
    let demo = Demo::without(&["xdg.data", "known.roaming-app-data"]);
    let mut script = filtered_to("acme/api/target", Bucket::Safe);
    script.extend(press(&[Key::Char('s'), Key::Enter, Key::Char('y')]));
    script.push(Step::Look);

    let frames = demo.home(script);

    assert!(present(&demo.at(ACME).join("debug")), "something ran");
    let review = &frames.looked[0];
    assert!(review.contains("Review: clean 0 items"), "{review}");
    assert!(
        review.contains("~/projects/clients/acme/api/target: "),
        "{review}"
    );
}

#[derive(Debug)]
struct Epoch;

impl Clock for Epoch {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
}

// Catches two kiplles changing the same machine at once: with the run store's lock held
// by another, the run is refused, the refusal shown, and nothing goes.
#[test]
fn a_lock_another_kipple_holds_refuses_the_run_and_says_so() {
    let demo = Demo::new();
    let store = FileRunStore::new(Some(demo.data()), Arc::new(Epoch));
    let held = store.lock(&CancelToken::new()).unwrap();
    let mut script = filtered_to("acme/api/target", Bucket::Safe);
    script.extend(press(&[Key::Char('s'), Key::Enter, Key::Char('y')]));

    script.push(Step::Look);

    let frames = demo.home(script);

    drop(held);
    assert!(present(&demo.at(ACME).join("debug")), "something ran");
    assert!(frames.last.contains("another kipple"), "{}", frames.last);
}

/// Runs the built kipple with nothing to answer: `clean --only builtin.cargo.target
/// --yes`, crashing just before its first item, so its run is left pending.
#[expect(
    clippy::disallowed_methods,
    reason = "the end-to-end test runs the built kipple, not a native tool"
)]
fn crash_a_clean(demo: &Demo) {
    let output = Command::new(env!("CARGO_BIN_EXE_kipple"))
        .args(["clean", "--only", "builtin.cargo.target", "--yes"])
        .env_clear()
        .env("KIPPLE_TEST_ROOTS", &demo.roots)
        .env(config::VAR, &demo.config)
        .envs(std::env::var_os("SYSTEMROOT").map(|root| ("SYSTEMROOT", root)))
        .env("KIPPLE_TEST_CRASH_AT", "0")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(99),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// Catches a run a crash left pending being run again, or reconciled out of sight: the
// home screen's run reconciles it first and its result says what was found.
#[test]
fn a_run_a_crash_left_pending_is_reconciled_first_and_shown() {
    let demo = Demo::new();
    crash_a_clean(&demo);
    let mut script = filtered_to("web/node_modules", Bucket::Review);
    script.extend(press(&[Key::Char('s'), Key::Enter, Key::Char('y')]));

    let frames = demo.home(script);

    let result = &frames.last;
    assert!(result.contains("was interrupted"), "{result}");
    assert!(
        present(&demo.at(RUST_APP)),
        "the crashed run's item ran again"
    );
    assert!(!present(&demo.at(WEB)), "the run after it didn't run");
}
