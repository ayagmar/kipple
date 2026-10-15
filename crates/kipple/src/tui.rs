//! The home screen (`docs/03-functional-spec.md` §2 J1 and §5): `kipple` with no
//! arguments in a terminal.
//!
//! A state machine (`app`) and a pure renderer (`view`), driven by one loop that
//! reads keys and scan events and draws at most once a frame. It is read-only: selecting
//! marks findings and nothing else.

mod app;
mod filter;
mod view;

use std::io;
use std::thread::{self, ScopedJoinHandle};
use std::time::{Duration, Instant};

use kipple_core::{CancelToken, EventRx, ScanOutcome, ScanReport, ScanRequest, event_channel};
use kipple_platform::Environment;
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub use self::app::Key;
use self::app::{App, Catalog, Msg};
pub use self::view::Look;
use crate::config::Settings;
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

/// Runs the home screen on `terminal` until it is closed.
///
/// It scans the roots of `settings` on the machine `env` describes, with their policy, and
/// returns how long the scan took when it finished before the screen was closed. Closing
/// it earlier cancels the scan.
///
/// # Errors
/// When the terminal can't be read or drawn, or the built-in rules don't load.
pub fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    keys: &mut dyn Keys,
    env: &Environment,
    settings: &Settings,
    look: Look,
) -> io::Result<Option<Duration>>
where
    B::Error: Send + Sync + 'static,
{
    let builtin = Builtin::load().map_err(io::Error::other)?;
    let engine = builtin.engine(env).map_err(io::Error::other)?;
    let request = ScanRequest {
        roots: settings.roots.clone(),
        lens: None,
        exclude: settings.policy.exclude.clone(),
    };
    let size = terminal.size().map_err(io::Error::other)?;
    let mut app = App::new(Catalog::new(&builtin), size.width, size.height);
    app.policy = settings.policy.clone();
    let cancel = CancelToken::new();
    let (events, received) = event_channel(STREAM_CAPACITY, &cancel);
    thread::scope(|scope| {
        let started = Instant::now();
        let scan = scope.spawn(|| {
            let events = events;
            engine.scan(&request, &events, &cancel)
        });
        let mut screen = Screen {
            terminal,
            look,
            app: &mut app,
            received,
            scan: Some(scan),
            started,
            elapsed: None,
        };
        let ran = screen.drive(keys);
        // Stops a scan still running, so leaving the scope doesn't wait for it.
        cancel.cancel();
        ran.map(|()| screen.elapsed)
    })
}

/// Opens the home screen in this terminal and restores the terminal however it ends:
/// on quit, on error, and on panic (ratatui's panic hook).
pub(crate) fn open(env: &Environment, settings: &Settings, look: Look) -> io::Result<()> {
    // Before `try_init`: it enables raw mode first, and that stays on if entering the
    // alternate screen then fails.
    let _restore = Restore;
    let mut terminal = ratatui::try_init()?;
    run(&mut terminal, &mut TerminalKeys, env, settings, look).map(|_| ())
}

/// Leaves raw mode and the alternate screen when dropped.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

/// One home screen session: the state, the scan feeding it and the terminal drawing it.
struct Screen<'s, 'scope, B: Backend> {
    terminal: &'s mut Terminal<B>,
    look: Look,
    app: &'s mut App,
    received: EventRx,
    scan: Option<ScopedJoinHandle<'scope, ScanReport>>,
    started: Instant,
    elapsed: Option<Duration>,
}

impl<B: Backend> Screen<'_, '_, B>
where
    B::Error: Send + Sync + 'static,
{
    /// Reads keys and scan events until the screen is closed. A key is drawn at once;
    /// scan events are drawn at most once a frame.
    fn drive(&mut self, keys: &mut dyn Keys) -> io::Result<()> {
        self.draw()?;
        let mut drawn = Instant::now();
        let mut stale = false;
        while !self.app.quit {
            let wait = if self.scan.is_some() { FRAME } else { IDLE };
            if let Some(input) = keys.next(wait)? {
                self.app.update(match input {
                    Input::Key(key) => Msg::Key(key),
                    Input::Resize { width, height } => Msg::Resize { width, height },
                });
                self.draw()?;
                drawn = Instant::now();
                stale = false;
            }
            stale |= self.receive();
            if stale && drawn.elapsed() >= FRAME {
                self.draw()?;
                drawn = Instant::now();
                stale = false;
            }
        }
        Ok(())
    }

    /// Applies the scan events that have arrived, at most a batch of them, and the
    /// report once the scan has ended. Whether anything changed.
    fn receive(&mut self) -> bool {
        let mut changed = false;
        for _ in 0..BATCH {
            let Some(sequenced) = self.received.poll(Duration::ZERO) else {
                break;
            };
            self.app.update(Msg::Scan(Box::new(sequenced.event)));
            changed = true;
        }
        if self.received.ended()
            && let Some(scan) = self.scan.take()
        {
            let report = scan.join().unwrap_or_else(|_| ScanReport {
                outcome: ScanOutcome::Cancelled,
                integrations: Vec::new(),
                findings: Vec::new(),
            });
            let elapsed = self.started.elapsed();
            self.elapsed = Some(elapsed);
            self.app.update(Msg::Finished {
                report: Box::new(report),
                elapsed,
            });
            changed = true;
        }
        changed
    }

    fn draw(&mut self) -> io::Result<()> {
        let (app, look) = (&*self.app, self.look);
        self.terminal
            .draw(|frame| view::render(app, look, frame))
            .map(|_| ())
            .map_err(io::Error::other)
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
    use super::*;

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
