//! The home screen (`docs/03-functional-spec.md` §2 J1 and §5): `kipple` with no
//! arguments in a terminal, after the first run's questions (J0) when there is no config
//! file yet.
//!
//! A state machine (`app`) and pure drawing (`ui`), driven by one loop that
//! reads keys and scan events and draws at most once a frame. Selecting marks findings
//! and nothing else; a clean is planned, reviewed, confirmed and run on workers (`work`)
//! whose answers and events the loop reads like the scan's.

mod app;
mod filter;
pub(crate) mod keymap;
mod onboarding;
mod settings;
mod themes;
mod ui;
mod work;

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, Scope, ScopedJoinHandle};
use std::time::{Duration, Instant};

use kipple_core::format::base64;
use kipple_core::{
    ApplyEvent, CancelToken, Engine, EventChannel, EventRx, Finished, Prepared, ScanOutcome,
    ScanReport, ScanRequest, Selection, Selector, StoreRefusal, SymbolicRoot, event_channel,
    suggest,
};
use kipple_platform::{
    Environment, ProjectSearch, SearchLimits, SystemClock, kipple_dirs, search_projects,
};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub use self::app::Key;
use self::app::{
    App, Catalog, Cleaning, Confirmation, Effect, Generation, Msg, Saved, needs_typing,
};
pub use self::onboarding::{LevelTotals, due as first_run_due, level_totals};
use self::onboarding::{Onboarding, Outcome};
use self::settings::Found;
use self::themes::Picker;
use self::work::{Caught, Failed, Ground, guarded};
use crate::config::{self, ConfigError, Edit, Loaded, Settings};
use crate::human::{outcome, review};
use crate::setup::{Builtin, BuiltinRule};
use crate::theme::{self, Colours, Glyphs, Look, ThemeError};

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

/// How the home screen is drawn: the themes it can switch between in settings, the one
/// in force, whether colour is used, and the glyphs.
#[derive(Debug, Clone)]
pub struct Appearance {
    themes: Picker,
    glyphs: Glyphs,
}

impl Appearance {
    /// The themes of `catalog` drawn with `colours`, `active` in force, and `glyphs`.
    ///
    /// # Errors
    /// When there is no theme `active` in `catalog`, or it can't be used.
    pub fn new(
        catalog: theme::Catalog,
        active: &str,
        colours: Colours,
        glyphs: Glyphs,
    ) -> Result<Self, ThemeError> {
        let themes = Picker::new(catalog, active, colours)?;
        Ok(Self { themes, glyphs })
    }
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
    appearance: Appearance,
) -> io::Result<Option<Duration>>
where
    B::Error: Send + Sync + 'static,
{
    let start = Start {
        env,
        settings,
        appearance,
        first: false,
        exit: Exit::Return,
    };
    session(terminal, keys, start)
}

/// How a session that runs a clean may end besides returning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    /// On the real terminal: the second Ctrl+C during a clean's apply restores the
    /// terminal and ends the process with 130, and leaving while one runs restores the
    /// terminal before it waits for the current item (spec M3c pinned 7, 8).
    Process,
    /// On a test terminal: the second Ctrl+C only closes the screen.
    Return,
}

/// What a session runs with: the machine, the settings and the appearance, whether
/// the first run's questions come before the home screen, and how it may end.
struct Start<'env> {
    env: &'env Environment,
    settings: &'env Settings,
    appearance: Appearance,
    first: bool,
    exit: Exit,
}

/// [`run`], with the first run's questions over the home screen, while the scan already
/// runs, when `start` asks for them.
fn session<B: Backend + io::Write>(
    terminal: &mut Terminal<B>,
    keys: &mut dyn Keys,
    start: Start<'_>,
) -> io::Result<Option<Duration>>
where
    B::Error: Send + Sync + 'static,
{
    let Start {
        env,
        settings,
        appearance,
        first,
        exit,
    } = start;
    let builtin = Builtin::load().map_err(io::Error::other)?;
    let engine = builtin.engine(env).map_err(io::Error::other)?;
    let size = terminal.size().map_err(io::Error::other)?;
    let mut app = App::new(Catalog::new(&builtin), size.width, size.height);
    app.configure(Saved::of(settings));
    app.themes = appearance.themes;
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
            glyphs: appearance.glyphs,
            app: &mut app,
            onboarding,
            store,
            scope,
            builtin: &builtin,
            engine: &engine,
            markers: &markers,
            scan: None,
            search: None,
            planning: None,
            applying: None,
            exit,
            elapsed: None,
        };
        screen.start();
        let ran = screen.effects().and_then(|()| screen.drive(keys));
        // Dropping the screen stops what still runs, so leaving the scope doesn't wait
        // for more than the item a run is on.
        ran.map(|()| screen.elapsed)
    })
}

/// Opens the home screen in this terminal and restores the terminal however it ends:
/// on quit, on error, and on a panic of the screen's thread (ratatui's panic hook, kept
/// for that thread only). A worker's panic is told on stderr once the terminal is
/// restored. Without a config file at the default location, the first run's questions
/// come first (J0).
pub(crate) fn open(
    env: &Environment,
    settings: &Settings,
    appearance: Appearance,
) -> io::Result<()> {
    let first = onboarding::due(&settings.loaded);
    let caught: Caught = Arc::new(Mutex::new(Vec::new()));
    // Before `try_init`: it enables raw mode first, and that stays on if entering the
    // alternate screen then fails.
    let _restore = Restore {
        caught: Arc::clone(&caught),
    };
    let mut terminal = ratatui::try_init()?;
    work::quiet_workers(thread::current().id(), caught);
    let start = Start {
        env,
        settings,
        appearance,
        first,
        exit: Exit::Process,
    };
    session(&mut terminal, &mut TerminalKeys, start).map(|_| ())
}

/// Leaves raw mode and the alternate screen when dropped, then tells what other threads'
/// panics said.
struct Restore {
    caught: Caught,
}

impl Drop for Restore {
    fn drop(&mut self) {
        ratatui::restore();
        work::tell(&self.caught);
    }
}

/// The config file the home screen writes, and the settings it last gave.
struct Store<'env> {
    env: &'env Environment,
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

impl Store<'_> {
    /// The settings the config file gives now, as a clean plans and runs under them
    /// (spec M3c pinned 4, 6).
    fn reload(&self) -> Result<Settings, ConfigError> {
        let location = &self.settings.loaded.location;
        config::reload(self.env, location, &[], &self.catalog)
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
struct Screen<'ui, 'scope, 'env, B: Backend> {
    terminal: &'ui mut Terminal<B>,
    glyphs: Glyphs,
    app: &'ui mut App,
    /// The first run's questions, until they are answered.
    onboarding: Option<Onboarding>,
    store: Store<'env>,
    scope: &'scope Scope<'scope, 'env>,
    engine: &'env Engine,
    /// The file names that mark a project for the root search.
    markers: &'env [OsString],
    /// The built-in rules a clean's workers build their machine from.
    builtin: &'env Builtin,
    scan: Option<Scan<'scope>>,
    /// The root search, until its answer has been taken.
    search: Option<RootSearch<'scope>>,
    /// The worker planning a clean, until its answer has been taken.
    planning: Option<Planning<'scope>>,
    /// The worker running a confirmed clean, until its answer has been taken.
    applying: Option<Running<'scope>>,
    exit: Exit,
    elapsed: Option<Duration>,
}

/// A clean being planned on its own thread, for one request.
struct Planning<'scope> {
    generation: Generation,
    /// `review.confirm_by_typing_above` in the config it plans under.
    threshold: Option<u64>,
    cancel: CancelToken,
    answer: ScopedJoinHandle<'scope, Result<Result<Prepared, String>, Failed>>,
}

/// A confirmed clean running on its own thread.
struct Running<'scope> {
    cancel: CancelToken,
    events: Receiver<ApplyEvent>,
    answer: ScopedJoinHandle<'scope, Result<Result<Finished, String>, Failed>>,
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
        let EventChannel {
            tx: events,
            rx: received,
        } = event_channel(STREAM_CAPACITY, &cancel);
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

    /// Whether a clean's worker is planning or running.
    const fn working(&self) -> bool {
        self.planning.is_some() || self.applying.is_some()
    }

    /// Starts planning `selection` of the finished scan for request `generation`, under
    /// the config as it is now, stopping the planning of an earlier request.
    fn plan(&mut self, generation: Generation, selection: Selection) {
        self.stop_planning();
        let Some(report) = self.app.report().cloned() else {
            return;
        };
        let settings = match self.store.reload() {
            Ok(settings) => settings,
            Err(error) => {
                let why = review::config_changed(&error.to_string());
                self.app.update(Msg::NotPlanned { generation, why });
                return;
            }
        };
        let threshold = settings.loaded.config.confirm_by_typing_above;
        let ground = Ground {
            builtin: self.builtin,
            env: self.store.env,
            settings,
        };
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let answer = self
            .scope
            .spawn(move || guarded(|| work::plan(ground, &report, &selection, &token)));
        self.planning = Some(Planning {
            generation,
            threshold,
            cancel,
            answer,
        });
    }

    /// Stops the planning under way; its answer is never read.
    fn stop_planning(&mut self) {
        if let Some(planning) = self.planning.take() {
            planning.cancel.cancel();
        }
    }

    /// Runs the plan the person confirmed `by`, under the config as it is now: refused,
    /// with nothing run, when that config doesn't load, kipple's data folder isn't known,
    /// or it now asks for the size to be typed and `by` didn't (pinned 6, 16).
    fn apply(&mut self, by: Confirmation) {
        let Some(prepared) = self.app.confirmed().cloned() else {
            return;
        };
        let settings = match self.store.reload() {
            Ok(settings) => settings,
            Err(error) => {
                let why = review::config_changed(&error.to_string());
                self.app.update(Msg::NotApplied(why));
                return;
            }
        };
        if kipple_dirs(&settings.roots).data.path.is_err() {
            let why = outcome::store_refusal(&StoreRefusal::NoDataDir);
            self.app.update(Msg::NotApplied(review::not_run(&why)));
            return;
        }
        let threshold = settings.loaded.config.confirm_by_typing_above;
        let total = prepared.plan().estimated_total();
        if by == Confirmation::Key && needs_typing(threshold, &total) {
            self.app.update(Msg::MustType);
            return;
        }
        let ground = Ground {
            builtin: self.builtin,
            env: self.store.env,
            settings,
        };
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let (sender, events) = mpsc::channel();
        let answer = self
            .scope
            .spawn(move || guarded(|| work::apply(ground, &prepared, sender, &token)));
        self.applying = Some(Running {
            cancel,
            events,
            answer,
        });
    }

    /// Stops the run after the item under way (the first Ctrl+C).
    fn stop_apply(&self) {
        if let Some(running) = &self.applying {
            running.cancel.cancel();
        }
    }

    /// The second Ctrl+C during a run: on the real terminal, restores it, says which run
    /// the next command reconciles, and ends kipple at once with 130 (pinned 8). The
    /// events the run already sent are passed on first, so a run stored just before is
    /// named.
    fn abort(&mut self) {
        if self.exit == Exit::Return {
            self.app.quit = true;
            return;
        }
        self.take_events(usize::MAX);
        ratatui::restore();
        let line = self.app.run().map_or_else(
            || review::INTERRUPTED_RECONCILING.to_owned(),
            |run| review::interrupted_run(run.as_str()),
        );
        crate::say(&mut io::stderr().lock(), &line);
        std::process::exit(i32::from(crate::CANCELLED));
    }

    /// Passes the planning worker's answer on once it has ended. A review it opens is
    /// drawn at once, and what was typed before it showed is thrown away. Whether
    /// anything is left to draw.
    fn planned(&mut self, keys: &mut dyn Keys) -> io::Result<bool> {
        if !self.answer_plan() {
            return Ok(false);
        }
        if !matches!(self.app.cleaning, Some(Cleaning::Reviewing(_))) {
            return Ok(true);
        }
        self.draw()?;
        self.discard_typed_ahead(keys)
    }

    /// Throws away what was typed and not yet read, as the command line does before its
    /// question (`docs/03-functional-spec.md` §3), so a key typed before the review
    /// showed can't confirm it. A resize among it still applies. Whether one did.
    fn discard_typed_ahead(&mut self, keys: &mut dyn Keys) -> io::Result<bool> {
        let mut resized = false;
        while let Some(input) = keys.next(Duration::ZERO)? {
            if let Input::Resize { .. } = input {
                self.input(input);
                resized = true;
            }
        }
        Ok(resized)
    }

    /// Passes the planning worker's answer on once it has ended. Whether it had.
    fn answer_plan(&mut self) -> bool {
        if !self
            .planning
            .as_ref()
            .is_some_and(|planning| planning.answer.is_finished())
        {
            return false;
        }
        let Some(planning) = self.planning.take() else {
            return false;
        };
        let generation = planning.generation;
        let msg = match planning.answer.join().unwrap_or(Err(Failed)) {
            Ok(Ok(prepared)) => Msg::Planned {
                generation,
                prepared: Box::new(prepared),
                threshold: planning.threshold,
            },
            Ok(Err(why)) => Msg::NotPlanned {
                generation,
                why: review::not_run(&why),
            },
            Err(Failed) => Msg::PlanFailed { generation },
        };
        self.app.update(msg);
        true
    }

    /// Passes on at most `most` of the events the run has sent. Whether there were any.
    fn take_events(&mut self, most: usize) -> bool {
        let Some(running) = &self.applying else {
            return false;
        };
        let mut changed = false;
        for _ in 0..most {
            match running.events.try_recv() {
                Ok(event) => {
                    self.app.update(Msg::Applied(Box::new(event)));
                    changed = true;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        changed
    }

    /// Applies the run's events that have arrived, at most a batch of them while it
    /// runs and all of them once it has ended, then its answer. Whether anything changed.
    fn ran(&mut self) -> bool {
        let Some(running) = &self.applying else {
            return false;
        };
        let ended = running.answer.is_finished();
        let changed = self.take_events(if ended { usize::MAX } else { BATCH });
        if !ended {
            return changed;
        }
        let Some(done) = self.applying.take() else {
            return changed;
        };
        let msg = match done.answer.join().unwrap_or(Err(Failed)) {
            Ok(Ok(finished)) => Msg::Ran(Box::new(finished)),
            Ok(Err(why)) => Msg::NotApplied(review::not_run(&why)),
            Err(Failed) => Msg::ApplyFailed,
        };
        self.app.update(msg);
        true
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
                Effect::Plan {
                    generation,
                    selection,
                } => self.plan(generation, selection),
                Effect::StopPlanning => self.stop_planning(),
                Effect::Apply(by) => self.apply(by),
                Effect::StopApply => self.stop_apply(),
                Effect::Abort => self.abort(),
            }
        }
        Ok(())
    }

    /// Reads keys and scan events until the screen is closed. A key is drawn at once;
    /// scan and clean events are drawn at most once a frame, and at once when they end.
    fn drive(&mut self, keys: &mut dyn Keys) -> io::Result<()> {
        self.draw()?;
        let mut drawn = Instant::now();
        let mut stale = false;
        while !self.app.quit {
            let wait = if self.scanning() || self.working() {
                FRAME
            } else {
                IDLE
            };
            if let Some(input) = keys.next(wait)? {
                self.input(input);
                self.effects()?;
                self.draw()?;
                drawn = Instant::now();
                stale = false;
            }
            stale |= self.receive();
            stale |= self.suggested();
            stale |= self.planned(keys)?;
            stale |= self.ran();
            // At most a frame's worth of changes waits while something runs; once it
            // has ended, what it ended with is drawn before the wait for a key.
            let busy = self.scanning() || self.working();
            if stale && (!busy || drawn.elapsed() >= FRAME) {
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
                report: Arc::new(report),
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
        let (app, onboarding) = (&*self.app, self.onboarding.as_ref());
        let look = Look {
            theme: app.themes.theme(),
            glyphs: self.glyphs,
        };
        self.terminal
            .draw(|frame| match onboarding {
                Some(onboarding) => ui::onboarding::render(onboarding, app, look, frame),
                None => ui::render(app, look, frame),
            })
            .map(|_| ())
            .map_err(io::Error::other)
    }
}

impl<B: Backend> Drop for Screen<'_, '_, '_, B> {
    /// Stops everything still running, so leaving the scope waits only for the item a
    /// run is on. On the real terminal, a run still going restores the terminal first
    /// and says it stops after that item (pinned 7).
    fn drop(&mut self) {
        if let Some(scan) = &self.scan {
            scan.cancel.cancel();
        }
        if let Some(search) = &self.search {
            search.cancel.cancel();
        }
        if let Some(planning) = &self.planning {
            planning.cancel.cancel();
        }
        if let Some(running) = &self.applying {
            running.cancel.cancel();
            if self.exit == Exit::Process {
                ratatui::restore();
                crate::say(&mut io::stderr().lock(), review::STOPPING);
            }
        }
    }
}

/// Sends `text` to the terminal's clipboard: OSC 52 with `text` in base64, so no byte of
/// it can end the escape, then a flush so it doesn't wait for the next frame. A
/// terminal that ignores OSC 52 copies nothing.
fn osc52(out: &mut impl io::Write, text: &str) -> io::Result<()> {
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()
}

/// The file names that mark a project for the root search: the markers of every
/// built-in `project-artifact` rule.
fn markers(builtin: &Builtin) -> Vec<OsString> {
    let mut markers: Vec<OsString> = builtin
        .rules()
        .filter_map(|BuiltinRule { rule, .. }| match &rule.selector {
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
    /// An event the screen doesn't use is passed over while more are already waiting, so
    /// `None` means nothing it uses is waiting.
    fn next(&mut self, timeout: Duration) -> io::Result<Option<Input>> {
        if !event::poll(timeout)? {
            return Ok(None);
        }
        loop {
            if let Some(input) = input_of(&event::read()?) {
                return Ok(Some(input));
            }
            if !event::poll(Duration::ZERO)? {
                return Ok(None);
            }
        }
    }
}

/// What the screen takes from a terminal event, if anything.
fn input_of(event: &Event) -> Option<Input> {
    match *event {
        // Windows also reports releases; holding a key repeats it.
        Event::Key(key) if key.kind != KeyEventKind::Release => key_of(key).map(Input::Key),
        Event::Resize(width, height) => Some(Input::Resize { width, height }),
        Event::Key(_)
        | Event::FocusGained
        | Event::FocusLost
        | Event::Mouse(_)
        | Event::Paste(_) => None,
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
