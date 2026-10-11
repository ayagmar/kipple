//! `cargo xtask bench-tui` (`docs/11-delivery-plan.md`, M2): keypress-to-frame latency
//! of the home screen while it scans the 1M reference tree.
//!
//! The tree is `bench-tree`'s, with the marker each hot spot's project needs added, so
//! the Cargo and Node integrations find and size every `target` and `node_modules`. The
//! real home screen loop runs in process on a 160×48 test terminal while another thread
//! presses a key every 16 ms. A key's latency runs from when it was pressed to when the
//! next frame after the loop read it was flushed, so time spent waiting for the loop
//! counts. Only keys pressed while the scan ran count. A warm scan of the tree can take
//! well under a second, so the tree is scanned again until enough keys were pressed
//! during scans.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use kipple::tui::{self, Input, Key, Keys, Layout, Look, Options};
use kipple_platform::Environment;
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

use crate::{bench_tree, cmd};

/// The reference tree's size.
const ENTRIES: u64 = 1_000_000;
/// How often a key is pressed: faster than a held key repeats.
const PRESS_EVERY: Duration = Duration::from_millis(16);
/// The M2 budget for the 95th percentile.
const BUDGET: Duration = Duration::from_millis(50);
/// Keys pressed during a scan to collect, over as many scans as that takes.
const SAMPLES: usize = 200;
/// Fewer keys than this say nothing about a percentile.
const MIN_SAMPLES: usize = 50;
/// The most scans to run for them.
const MAX_SCANS: usize = 100;
/// Keys for the first scan: 10 s worth.
const FIRST_KEYS: usize = 625;
/// Keys pressed after a scan is expected to have ended, so it can't outrun them.
const SPARE_KEYS: usize = 10;
/// What is pressed, in turn: moving, switching lens and opening and closing help.
const KEYS: [Key; 12] = [
    Key::Down,
    Key::Down,
    Key::Down,
    Key::Up,
    Key::Up,
    Key::Up,
    Key::Tab,
    Key::Tab,
    Key::Tab,
    Key::Tab,
    Key::Char('?'),
    Key::Char('?'),
];

/// Measures the home screen on the `given` tree, or on a fresh 1M reference tree, over
/// as many scans as it takes to collect `SAMPLES` keys pressed during a scan. Fails when
/// their 95th percentile reaches the budget.
///
/// The budget is for the release build, so a debug build of xtask runs itself again as
/// a release build.
pub(crate) fn run(root: &Path, given: Option<&Path>) -> Result<()> {
    if cfg!(debug_assertions) {
        let mut release = cmd::command("cargo", root);
        release.args([
            "run",
            "--quiet",
            "--release",
            "--locked",
            "-p",
            "xtask",
            "--",
        ]);
        release.arg("bench-tui");
        if let Some(given) = given {
            release.arg("--tree").arg(given);
        }
        return cmd::run(&mut release);
    }
    let temp = tempfile::tempdir()?;
    let tree = if let Some(given) = given {
        given.to_owned()
    } else {
        let built = temp.path().join("tree");
        bench_tree::run(&built, ENTRIES)?;
        built
    };
    let projects = mark_projects(&tree)?;
    // Every root but the tree resolves inside an empty home and system root.
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let env = Environment::from_vars([(OsString::from("HOME"), home.clone().into_os_string())])
        .with_system_root(home);

    let mut latencies = Vec::new();
    let mut scans = Vec::new();
    // Enough keys for a long scan at first; then enough to outlast the last one.
    let mut keys = FIRST_KEYS;
    while latencies.len() < SAMPLES && scans.len() < MAX_SCANS {
        let measured = scan(&tree, &env, keys)?;
        latencies.extend(measured.latencies);
        if let Some(scanned) = measured.scanned {
            scans.push(scanned);
            let presses = scanned.as_millis() / PRESS_EVERY.as_millis() + 1;
            keys = usize::try_from(presses).unwrap_or(FIRST_KEYS) + SPARE_KEYS;
        } else {
            // The scan outlasted every key: all of them count.
            scans.push(Duration::MAX);
        }
    }
    latencies.sort_unstable();
    report(&latencies, &scans, projects)
}

/// One scan's keys and how long it took, unless it outlasted them.
struct Measured {
    latencies: Vec<Duration>,
    scanned: Option<Duration>,
}

/// Opens the home screen on `tree`, presses `keys` keys while it scans, then closes it.
fn scan(tree: &Path, env: &Environment, keys: usize) -> Result<Measured> {
    let flushes = Arc::new(Mutex::new(Vec::new()));
    let (shown, first_frame) = mpsc::channel();
    let backend = Timed {
        inner: TestBackend::new(160, 48),
        flushes: Arc::clone(&flushes),
        shown: Some(shown),
    };
    let mut terminal = Terminal::new(backend)?;
    let (keyboard, inbox) = mpsc::channel();
    let (stop, stopped) = mpsc::channel::<()>();
    let typist = thread::spawn(move || {
        // Typing starts once the screen is up, as a person's would.
        if first_frame.recv().is_err() {
            return;
        }
        for key in KEYS.iter().cycle().take(keys) {
            if keyboard.send((*key, Instant::now())).is_err() {
                return;
            }
            // A wait that ends early once the screen has closed.
            if stopped.recv_timeout(PRESS_EVERY) != Err(RecvTimeoutError::Timeout) {
                return;
            }
        }
    });
    let mut pressed = Pressed {
        inbox,
        delivered: Vec::new(),
    };
    let options = Options {
        layout: Layout::Buckets,
        look: Look {
            color: true,
            ascii: false,
        },
    };
    let started = Instant::now();
    let scanned = tui::run(
        &mut terminal,
        &mut pressed,
        env,
        &[tree.to_owned()],
        options,
    )?;
    drop(stop);
    typist
        .join()
        .map_err(|_| anyhow::anyhow!("the key thread panicked"))?;
    let flushes = flushes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let scan_end = scanned.map(|scanned| started + scanned);
    Ok(Measured {
        latencies: latencies(&pressed.delivered, &flushes, scan_end),
        scanned,
    })
}

/// Adds the marker each hot spot's project needs, so the integrations recognise it:
/// `Cargo.toml` and Cargo's `CACHEDIR.TAG` for a `target`, `package.json` and npm's
/// `.package-lock.json` for a `node_modules`. Returns how many projects there are.
fn mark_projects(tree: &Path) -> Result<usize> {
    let mut projects = 0;
    for entry in fs::read_dir(tree).with_context(|| format!("could not read {}", tree.display()))? {
        let project = entry?.path();
        projects += 1;
        if project.join("target").is_dir() {
            fs::write(project.join("Cargo.toml"), "[package]\nname = \"bench\"\n")?;
            fs::write(
                project.join("target").join("CACHEDIR.TAG"),
                "Signature: 8a477f597d28d172789f06886806bc55\n",
            )?;
        }
        if project.join("node_modules").is_dir() {
            fs::write(project.join("package.json"), "{}\n")?;
            fs::write(
                project.join("node_modules").join(".package-lock.json"),
                "{}\n",
            )?;
        }
    }
    Ok(projects)
}

/// Each key's latency: from when it was pressed to the first flush after the loop read
/// it, for the keys pressed before `scan_end` (all of them when the scan never ended).
fn latencies(
    delivered: &[(Instant, Instant)],
    flushes: &[Instant],
    scan_end: Option<Instant>,
) -> Vec<Duration> {
    delivered
        .iter()
        .filter(|(pressed, _)| scan_end.is_none_or(|end| *pressed < end))
        .filter_map(|(pressed, read)| {
            let frame = flushes.iter().find(|flush| *flush >= read)?;
            Some(frame.duration_since(*pressed))
        })
        .collect()
}

fn report(latencies: &[Duration], scans: &[Duration], projects: usize) -> Result<()> {
    let mut sorted = scans.to_vec();
    sorted.sort_unstable();
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or_default();
    let scan = if median == Duration::MAX {
        "longer than the keys".to_owned()
    } else {
        format!("{:.2} s", median.as_secs_f64())
    };
    if latencies.len() < MIN_SAMPLES {
        bail!(
            "only {} keys were pressed during {} scans (median {scan}); a percentile needs {MIN_SAMPLES}",
            latencies.len(),
            scans.len()
        );
    }
    let at = |q: usize| {
        latencies
            .get(latencies.len() * q / 100)
            .copied()
            .unwrap_or_default()
    };
    let p95 = at(95);
    writeln!(
        io::stderr(),
        "xtask: {} keys during {} scans of {projects} projects (median scan {scan}): p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms (budget: p95 under {} ms)",
        latencies.len(),
        scans.len(),
        ms(at(50)),
        ms(p95),
        ms(latencies.last().copied().unwrap_or_default()),
        BUDGET.as_millis()
    )?;
    if p95 >= BUDGET {
        bail!(
            "keypress-to-frame p95 is {:.1} ms, over the budget",
            ms(p95)
        );
    }
    Ok(())
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// The presser's keys, each recorded with when it was pressed and when the loop read it.
struct Pressed {
    inbox: Receiver<(Key, Instant)>,
    delivered: Vec<(Instant, Instant)>,
}

impl Keys for Pressed {
    fn next(&mut self, timeout: Duration) -> io::Result<Option<Input>> {
        match self.inbox.recv_timeout(timeout) {
            Ok((key, pressed)) => {
                self.delivered.push((pressed, Instant::now()));
                Ok(Some(Input::Key(key)))
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            // Out of keys: close the screen.
            Err(RecvTimeoutError::Disconnected) => Ok(Some(Input::Key(Key::Interrupt))),
        }
    }
}

/// A test terminal that records when each frame was flushed, and says when the first
/// one was.
struct Timed {
    inner: TestBackend,
    flushes: Arc<Mutex<Vec<Instant>>>,
    shown: Option<mpsc::Sender<()>>,
}

impl Backend for Timed {
    type Error = <TestBackend as Backend>::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()?;
        self.flushes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Instant::now());
        if let Some(shown) = self.shown.take() {
            // The typist is gone only if it panicked, which the join reports.
            shown.send(()).ok();
        }
        Ok(())
    }
}
