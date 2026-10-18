//! The home screen (`docs/03-functional-spec.md` §2 J1 and §5): `kipple` with no
//! arguments in a terminal, after the first run's questions (J0) when there is no config
//! file yet.
//!
//! A state machine (`app`) and pure drawing (`ui`), driven by one loop that
//! reads keys and scan events and draws at most once a frame. It is read-only: selecting
//! marks findings and nothing else.

mod app;
mod filter;
mod onboarding;
mod settings;
mod ui;

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::thread::{self, Scope, ScopedJoinHandle};
use std::time::{Duration, Instant};

use kipple_core::{
    CancelToken, Engine, EventRx, ScanOutcome, ScanReport, ScanRequest, Selector, SymbolicRoot,
    event_channel, suggest,
};
use kipple_platform::{
    Environment, ProjectSearch, SearchLimits, SystemClock, kipple_dirs, search_projects,
};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub use self::app::Key;
use self::app::{App, Catalog, Effect, Msg, Saved};
pub use self::onboarding::{LevelTotals, due as first_run_due, level_totals};
use self::onboarding::{Onboarding, Outcome};
use self::settings::Found;
pub use self::ui::Look;
use crate::config::{self, Edit, Loaded, Settings};
use crate::setup::Builtin;

/// How long the loop waits for a key while the scan streams: one frame at 60 Hz.
const FRAME: Duration = Duration::from_millis(16);
/// How long it waits for a key once the scan has finished.
const IDLE: Duration = Duration::from_millis(250);
/// At most this many scan events are applied between two looks at the keyboard.
const BATCH: usize = 256;
/// How many undelivered events the stream holds before the scan waits.
const STREAM_CAPACITY: usize = 256;

/// Something from the keyboard or the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// A key was pressed.
    Key(Key),
    /// The terminal is now this size.
    Resize {
        /// Columns.
        width: u16,
        /// Rows.
        height: u16,
    },
}

/// Where input comes from: the terminal, or a script in a benchmark.
pub trait Keys {
    /// The next input, waiting at most `timeout` for it. `None` when nothing arrived in
    /// time.
    ///
    /// # Errors
    /// When the terminal can't be read.
    fn next(&mut self, timeout: Duration) -> io::Result<Option<Input>>;
}

/// Runs the home screen on `terminal` until it is closed. `y` writes to the backend, so it
/// is also an [`io::Write`]: the terminal's one writer.
///
/// It scans the roots of `settings` on the machine `env` describes, with their policy, and
/// returns how long the last scan took when it finished before the screen was closed.
/// Closing it earlier cancels the scan. Keeping a finding or changing a setting writes
/// the config file `settings` was loaded from, and nothing else.
///
/// # Errors
/// When the terminal can't be read or drawn, or the built-in rules don't load.
pub fn run<B: Backend + io::Write>(
    terminal: &mut Terminal<B>,
    keys: &mut dyn Keys,
    env: &Environment,
    settings: &Settings,
    look: Look,
) -> io::Result<Option<Duration>>
where
    B::Error: Send + Sync + 'static,
{
    let start = Start {
        env,
        settings,
        look,
        first: false,
    };
    session(terminal, keys, &start)
}

/// What a session runs with: the machine, the settings and the look, and whether the
/// first run's questions come before the home screen.
struct Start<'a> {
    env: &'a Environment,
    settings: &'a Settings,
    look: Look,
    first: bool,
}

/// [`run`], with the first run's questions over the home screen, while the scan already
/// runs, when `start` asks for them.
fn session<B: Backend + io::Write>(
    terminal: &mut Terminal<B>,
    keys: &mut dyn Keys,
    start: &Start<'_>,
) -> io::Result<Option<Duration>>
where
    B::Error: Send + Sync + 'static,
{
    let &Start {
        env,
        settings,
        look,
        first,
    } = start;
    let builtin = Builtin::load().map_err(io::Error::other)?;
    let engine = builtin.engine(env).map_err(io::Error::other)?;
    let size = terminal.size().map_err(io::Error::other)?;
    let mut app = App::new(Catalog::new(&builtin), size.width, size.height);
    app.configure(Saved::of(settings));
    let onboarding = first.then(|| Onboarding::new(&app));
    if first {
        app.start_search();
    }
    let markers = markers(&builtin);
    let store = Store {
        env,
        catalog: config::catalog(&builtin),
        settings: settings.clone(),
    };
    thread::scope(|scope| {
        let mut screen = Screen {
            terminal,
            look,
            app: &mut app,
            onboarding,
            store,
            scope,
            engine: &engine,
            markers: &markers,
            scan: None,
            search: None,
            elapsed: None,
        };
        screen.start();
        let ran = screen.effects().and_then(|()| screen.drive(keys));
        // Stops a scan or search still running, so leaving the scope doesn't wait for it.
        screen.stop();
        if let Some(search) = &screen.search {
            search.cancel.cancel();
        }
        ran.map(|()| screen.elapsed)
    })
}

/// Opens the home screen in this terminal and restores the terminal however it ends:
/// on quit, on error, and on panic (ratatui's panic hook). Without a config file at the
/// default location, the first run's questions come first (J0).
pub(crate) fn open(env: &Environment, settings: &Settings, look: Look) -> io::Result<()> {
    let first = onboarding::due(&settings.loaded);
    // Before `try_init`: it enables raw mode first, and that stays on if entering the
    // alternate screen then fails.
    let _restore = Restore;
    let mut terminal = ratatui::try_init()?;
    let start = Start {
        env,
        settings,
        look,
        first,
    };
    session(&mut terminal, &mut TerminalKeys, &start).map(|_| ())
}

/// Leaves raw mode and the alternate screen when dropped.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

/// The config file the home screen writes, and the settings it last gave.
struct Store<'e> {
    env: &'e Environment,
    catalog: config::Catalog,
    settings: Settings,
}

impl Store<'_> {
    /// Writes `edits` to the config file and rebuilds the settings from what was
    /// written. The answer for the home screen.
    fn write(&mut self, edits: &[Edit]) -> Msg {
        let location = &self.settings.loaded.location;
        let written = config::write(location, edits, &self.settings.roots, &self.catalog).and_then(
            |config| {
                let loaded = Loaded {
                    location: location.clone(),
                    exists: true,
                    config,
                };
                Settings::from_loaded(self.env, loaded, &[])
            },
        );
        match written {
            Ok(settings) => {
                let saved = Saved::of(&settings);
                self.settings = settings;
                Msg::Saved(Box::new(saved))
            }
            Err(error) => Msg::NotSaved(error.to_string()),
        }
    }
}

/// A scan in progress, or finished.
struct Scan<'scope> {
    cancel: CancelToken,
    received: EventRx,
    /// Until the report has been taken.
    report: Option<ScopedJoinHandle<'scope, ScanReport>>,
    started: Instant,
}

/// One home screen session: the state, the scan feeding it and the terminal drawing it.
struct Screen<'s, 'scope, 'env, B: Backend> {
    terminal: &'s mut Terminal<B>,
    look: Look,
    app: &'s mut App,
    /// The first run's questions, until they are answered.
    onboarding: Option<Onboarding>,
    store: Store<'env>,
    scope: &'scope Scope<'scope, 'env>,
    engine: &'env Engine,
    /// The file names that mark a project for the root search.
    markers: &'env [OsString],
    scan: Option<Scan<'scope>>,
    /// The root search, until its answer has been taken.
    search: Option<RootSearch<'scope>>,
    elapsed: Option<Duration>,
}

/// The search for roots to suggest, running on its own thread.
struct RootSearch<'scope> {
    cancel: CancelToken,
    found: ScopedJoinHandle<'scope, Found>,
}

impl<B: Backend + io::Write> Screen<'_, '_, '_, B>
where
    B::Error: Send + Sync + 'static,
{
    /// Starts a scan of the roots the settings give, cancelling the one running.
    fn start(&mut self) {
        self.stop();
        let settings = &self.store.settings;
        let request = ScanRequest {
            roots: settings.roots.clone(),
            lens: None,
            exclude: settings.policy.exclude.clone(),
        };
        let cancel = CancelToken::new();
        let (events, received) = event_channel(STREAM_CAPACITY, &cancel);
        let engine = self.engine;
        let token = cancel.clone();
        let report = self
            .scope
            .spawn(move || engine.scan(&request, &events, &token));
        self.scan = Some(Scan {
            cancel,
            received,
            report: Some(report),
            started: Instant::now(),
        });
        self.elapsed = None;
    }

    fn stop(&self) {
        if let Some(scan) = &self.scan {
            scan.cancel.cancel();
        }
    }

    /// Starts the search for roots to suggest, from the settings as they are now.
    fn search(&mut self) {
        let settings = self.store.settings.clone();
        let markers = self.markers;
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let found = self
            .scope
            .spawn(move || find_roots(&settings, markers, &token));
        self.search = Some(RootSearch { cancel, found });
    }

    fn scanning(&self) -> bool {
        self.scan.as_ref().is_some_and(|scan| scan.report.is_some())
    }

    /// Carries out what the home screen asked for: config writes, new scans, the root
    /// search and the clipboard.
    fn effects(&mut self) -> io::Result<()> {
        while let Some(effect) = self.app.take_effect() {
            match effect {
                Effect::Write(edits) => {
                    let answer = self.store.write(&edits);
                    self.app.update(answer);
                }
                Effect::Rescan => self.start(),
                Effect::Search => self.search(),
                Effect::Clipboard(text) => osc52(self.terminal.backend_mut(), &text)?,
            }
        }
        Ok(())
    }

    /// Reads keys and scan events until the screen is closed. A key is drawn at once;
    /// scan events are drawn at most once a frame.
    fn drive(&mut self, keys: &mut dyn Keys) -> io::Result<()> {
        self.draw()?;
        let mut drawn = Instant::now();
        let mut stale = false;
        while !self.app.quit {
            let wait = if self.scanning() { FRAME } else { IDLE };
            if let Some(input) = keys.next(wait)? {
                self.input(input);
                self.effects()?;
                self.draw()?;
                drawn = Instant::now();
                stale = false;
            }
            stale |= self.receive();
            stale |= self.suggested();
            if stale && drawn.elapsed() >= FRAME {
                self.draw()?;
                drawn = Instant::now();
                stale = false;
            }
        }
        Ok(())
    }

    /// Passes `input` to the first run's questions while they are open, else to the home
    /// screen. Finishing them asks the home screen to write the answers.
    fn input(&mut self, input: Input) {
        let key = match input {
            Input::Key(key) => key,
            Input::Resize { width, height } => {
                self.app.update(Msg::Resize { width, height });
                return;
            }
        };
        let Some(onboarding) = &mut self.onboarding else {
            self.app.update(Msg::Key(key));
            return;
        };
        match onboarding.key(key) {
            Outcome::Stay => {}
            Outcome::Quit => self.app.quit = true,
            Outcome::Finish(edits) => {
                self.onboarding = None;
                onboarding::finish(self.app, edits);
            }
        }
    }

    /// Applies the scan events that have arrived, at most a batch of them, and the
    /// report once the scan has ended. Whether anything changed.
    fn receive(&mut self) -> bool {
        let Some(scan) = &mut self.scan else {
            return false;
        };
        let mut changed = false;
        for _ in 0..BATCH {
            let Some(sequenced) = scan.received.poll(Duration::ZERO) else {
                break;
            };
            self.app.update(Msg::Scan(Box::new(sequenced.event)));
            changed = true;
        }
        if scan.received.ended()
            && let Some(report) = scan.report.take()
        {
            let report = report.join().unwrap_or_else(|_| ScanReport {
                outcome: ScanOutcome::Cancelled,
                integrations: Vec::new(),
                findings: Vec::new(),
                unchecked_system: 0,
                by_command_line: 0,
            });
            let elapsed = scan.started.elapsed();
            self.elapsed = Some(elapsed);
            self.app.update(Msg::Finished {
                report: Box::new(report),
                elapsed,
            });
            changed = true;
        }
        changed
    }

    /// Passes the root search's answer on once it has ended. Whether it had.
    fn suggested(&mut self) -> bool {
        if !self
            .search
            .as_ref()
            .is_some_and(|search| search.found.is_finished())
        {
            return false;
        }
        let Some(search) = self.search.take() else {
            return false;
        };
        // A search that panicked found nothing, and says it stopped early.
        let found = search.found.join().unwrap_or_default();
        if let Some(onboarding) = &mut self.onboarding {
            onboarding.suggested(&found);
        }
        self.app.update(Msg::Suggested(Box::new(found)));
        true
    }

    fn draw(&mut self) -> io::Result<()> {
        let (app, look, onboarding) = (&*self.app, self.look, self.onboarding.as_ref());
        self.terminal
            .draw(|frame| match onboarding {
                Some(onboarding) => ui::onboarding::render(onboarding, app, look, frame),
                None => ui::render(app, look, frame),
            })
            .map(|_| ())
            .map_err(io::Error::other)
    }
}

/// Sends `text` to the terminal's clipboard: OSC 52 with `text` in base64, so no byte of
/// it can end the escape, then a flush so it doesn't wait for the next frame. A
/// terminal that ignores OSC 52 copies nothing.
fn osc52(out: &mut impl io::Write, text: &str) -> io::Result<()> {
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()
}

/// `bytes` in standard base64, padded.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let [a, b, c] = [0, 1, 2].map(|i| chunk.get(i).copied().unwrap_or(0));
        let group = u32::from_be_bytes([0, a, b, c]);
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            let symbol = if index <= chunk.len() {
                ALPHABET
                    .get(usize::try_from((group >> shift) & 0x3f).unwrap_or(0))
                    .map_or('=', |&symbol| char::from(symbol))
            } else {
                '='
            };
            encoded.push(symbol);
        }
    }
    encoded
}

/// The file names that mark a project for the root search: the markers of every
/// built-in `project-artifact` rule.
fn markers(builtin: &Builtin) -> Vec<OsString> {
    let mut markers: Vec<OsString> = builtin
        .rules()
        .filter_map(|(_, rule)| match &rule.selector {
            Selector::ProjectArtifact { markers, .. } => Some(markers),
            Selector::DirsMatching { .. }
            | Selector::FilesMatching { .. }
            | Selector::Integration(_) => None,
        })
        .flatten()
        .map(|name| OsString::from(&name.0))
        .collect();
    markers.sort();
    markers.dedup();
    markers
}

/// Searches the home directory `settings` names for projects outside its project roots,
/// and turns them into suggested roots (D-053). With home unknown nothing can be
/// searched, so the answer says it stopped early.
fn find_roots(settings: &Settings, markers: &[OsString], cancel: &CancelToken) -> Found {
    let Some(home) = config::home(&settings.roots) else {
        return Found::default();
    };
    let roots: Vec<PathBuf> = settings
        .roots
        .all(SymbolicRoot::ProjectRoots)
        .iter()
        .filter_map(|root| root.as_ref().ok())
        .flat_map(|root| [root.path.clone(), root.configured().clone()])
        .collect();
    let dirs = kipple_dirs(&settings.roots);
    let mut skip = roots.clone();
    skip.extend(
        [dirs.config, dirs.data, dirs.cache]
            .into_iter()
            .filter_map(|dir| dir.path.ok()),
    );
    let exclude = &settings.policy.exclude;
    let search = ProjectSearch {
        home,
        markers,
        skip: &skip,
        exclude,
        limits: SearchLimits::default(),
    };
    let found = search_projects(&search, &SystemClock, cancel);
    Found {
        suggestions: suggest(home, &found.projects, &roots, exclude),
        complete: found.complete,
        unreadable: found.unreadable,
    }
}

/// The real keyboard, through crossterm.
struct TerminalKeys;

impl Keys for TerminalKeys {
    fn next(&mut self, timeout: Duration) -> io::Result<Option<Input>> {
        if !event::poll(timeout)? {
            return Ok(None);
        }
        Ok(match event::read()? {
            // Windows also reports releases; holding a key repeats it.
            Event::Key(key) if key.kind != KeyEventKind::Release => key_of(key).map(Input::Key),
            Event::Resize(width, height) => Some(Input::Resize { width, height }),
            Event::Key(_)
            | Event::FocusGained
            | Event::FocusLost
            | Event::Mouse(_)
            | Event::Paste(_) => None,
        })
    }
}

/// The home screen's key for a key press. Ctrl-C interrupts. A character held with both
/// Ctrl and Alt, other than an ASCII letter or digit, is typed as it is: Windows reports
/// Alt Gr that way, and Alt Gr types `@`, `{` or `ł` on many layouts. Any other key held
/// with Ctrl or Alt means nothing, so Ctrl-S and Ctrl-Alt-S don't select and Alt-Q
/// doesn't quit.
const fn key_of(key: KeyEvent) -> Option<Key> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Some(Key::Interrupt);
    }
    if let KeyCode::Char(c) = key.code
        && !c.is_ascii_alphanumeric()
        && key
            .modifiers
            .contains(KeyModifiers::CONTROL.union(KeyModifiers::ALT))
    {
        return Some(Key::Char(c));
    }
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL.union(KeyModifiers::ALT))
    {
        return None;
    }
    Some(match key.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab
        | KeyCode::Delete
        | KeyCode::Insert
        | KeyCode::F(_)
        | KeyCode::Null
        | KeyCode::CapsLock
        | KeyCode::ScrollLock
        | KeyCode::NumLock
        | KeyCode::PrintScreen
        | KeyCode::Pause
        | KeyCode::Menu
        | KeyCode::KeypadBegin
        | KeyCode::Media(_)
        | KeyCode::Modifier(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use ratatui::backend::CrosstermBackend;

    use super::*;

    /// What `osc52` writes for `text` through a terminal backend.
    fn sent(text: &str) -> Vec<u8> {
        let mut written = Vec::new();
        osc52(&mut CrosstermBackend::new(&mut written), text).unwrap();
        written
    }

    // Catches `y` writing a malformed escape the terminal ignores (wrong introducer,
    // selection or terminator), or a broken base64 a terminal decodes to another path.
    #[test]
    fn y_writes_the_path_as_one_osc_52_escape_in_base64() {
        assert_eq!(
            sent("/home/ada/projects/api"),
            b"\x1b]52;c;L2hvbWUvYWRhL3Byb2plY3RzL2FwaQ==\x07"
        );
        // Every padding length.
        for (text, encoded) in [
            ("", ""),
            ("a", "YQ=="),
            ("ab", "YWI="),
            ("abc", "YWJj"),
            ("abcd", "YWJjZA=="),
            ("abcde", "YWJjZGU="),
            ("abcdef", "YWJjZGVm"),
        ] {
            assert_eq!(base64(text.as_bytes()), encoded, "{text:?}");
        }
    }

    // Catches a path with control characters ending the escape early and writing the
    // rest to the terminal as commands.
    #[test]
    fn control_characters_in_a_path_stay_inside_the_escape() {
        let written = sent("/tmp/a\x07\x1b]0;owned\x07b");

        let controls: Vec<(usize, u8)> = written
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, byte)| byte.is_ascii_control())
            .collect();
        assert_eq!(controls, [(0, 0x1b), (written.len() - 1, 0x07)]);
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> Option<Key> {
        key_of(KeyEvent::new(code, modifiers))
    }

    // Catches a held Ctrl or Alt being dropped from the key, so Ctrl-S selects a
    // finding and Alt-Q closes the screen.
    #[test]
    fn keys_held_with_ctrl_or_alt_do_nothing() {
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            for code in [
                KeyCode::Char('s'),
                KeyCode::Char('q'),
                KeyCode::Char(' '),
                KeyCode::Enter,
            ] {
                assert_eq!(press(code, modifiers), None, "{modifiers:?} {code:?}");
            }
        }
        assert_eq!(press(KeyCode::Char('c'), KeyModifiers::ALT), None);
    }

    // Catches '@' or '{' typed with AltGr never reaching the filter on German and
    // French Windows layouts, where crossterm reports AltGr as Ctrl and Alt together.
    #[test]
    fn a_character_typed_with_altgr_is_typed() {
        let altgr = KeyModifiers::CONTROL.union(KeyModifiers::ALT);
        for c in ['@', '{', '\\'] {
            assert_eq!(press(KeyCode::Char(c), altgr), Some(Key::Char(c)), "{c:?}");
        }
        assert_eq!(press(KeyCode::Char('c'), altgr), Some(Key::Interrupt));
        assert_eq!(press(KeyCode::Enter, altgr), None);
    }

    // Catches Ctrl-Alt-S selecting a finding and Ctrl-Alt-Q closing the screen: Alt Gr
    // never types an ASCII letter or digit, so those are shortcuts, not text.
    #[test]
    fn ctrl_alt_with_an_ascii_letter_or_digit_does_nothing_but_altgr_letters_are_typed() {
        let altgr = KeyModifiers::CONTROL.union(KeyModifiers::ALT);
        for c in ['s', 'q', 'S', 'a', '1'] {
            assert_eq!(press(KeyCode::Char(c), altgr), None, "{c:?}");
        }
        for c in ['ł', 'ß', '€'] {
            assert_eq!(press(KeyCode::Char(c), altgr), Some(Key::Char(c)), "{c:?}");
        }
    }

    // Catches the modifier check swallowing the one modified key the screen answers.
    #[test]
    fn ctrl_c_interrupts_and_plain_keys_still_act() {
        assert_eq!(
            press(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Key::Interrupt)
        );
        assert_eq!(
            press(KeyCode::Char('q'), KeyModifiers::NONE),
            Some(Key::Char('q'))
        );
        assert_eq!(
            press(KeyCode::Char('S'), KeyModifiers::SHIFT),
            Some(Key::Char('S'))
        );
    }
}
