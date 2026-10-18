//! `cargo xtask bench-tui` (`docs/11-delivery-plan.md`, M2): keypress-to-frame latency
//! of the home screen while it scans the 1M reference tree.
//!
//! The tree is `bench-tree`'s, with the marker each hot spot's project needs added, so
//! the Cargo and Node integrations find and size every `target` and `node_modules`. A
//! given tree must carry `bench-tree`'s sentinel, and markers are only ever new files. The
//! real home screen loop runs in process on a 160×48 test terminal, with a config file of
//! its own in a temp dir so the user's is never read, while another thread
//! presses a key every 16 ms. A key's latency runs from when it was pressed to when the
//! next frame after the loop read it was flushed, so time spent waiting for the loop
//! counts. Only keys pressed while the scan ran count. A warm scan of the tree can take
//! well under a second, so the tree is scanned again until enough keys were pressed
//! during scans.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use kipple::config::{self, Settings};
use kipple::setup::Builtin;
use kipple::theme::{self, Catalog, Colours, Depth, Glyphs};
use kipple::tui::{self, Input, Key, Keys};
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
/// What is pressed, in turn: moving, switching lens, cycling the sort order back to
/// size, and opening and closing help.
const KEYS: [Key; 15] = [
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
    Key::Char('o'),
    Key::Char('o'),
    Key::Char('o'),
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
    // A config of its own, so the real one is never read.
    let config = temp.path().join("config.toml");
    fs::write(&config, "[care]\nlevel = \"balanced\"\n")?;

    let mut latencies = Vec::new();
    let mut scans = Vec::new();
    // Enough keys for a long scan at first; then enough to outlast the last one.
    let mut keys = FIRST_KEYS;
    while latencies.len() < SAMPLES && scans.len() < MAX_SCANS {
        let measured = scan(&tree, &env, &config, keys)?;
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

/// Opens the home screen on `tree` with the config `file`, presses `keys` keys while it
/// scans, then closes it.
fn scan(tree: &Path, env: &Environment, file: &Path, keys: usize) -> Result<Measured> {
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
    let appearance = tui::Appearance::new(
        Catalog::builtin(),
        theme::DEFAULT,
        Colours::On(Depth::TrueColor),
        Glyphs::Unicode,
    )?;
    let builtin = Builtin::load().map_err(anyhow::Error::msg)?;
    let catalog = config::catalog(&builtin);
    let settings = Settings::load(env, Some(file), &[tree.to_owned()], &catalog)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let started = Instant::now();
    let scanned = tui::run(&mut terminal, &mut pressed, env, &settings, appearance)?;
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
///
/// Only a tree `bench-tree` built is touched. Links are never followed, and an existing
/// marker is left as it is.
fn mark_projects(tree: &Path) -> Result<usize> {
    if !is_real(&tree.join(bench_tree::SENTINEL), fs::FileType::is_file) {
        bail!(
            "{} has no {} file, so bench-tree didn't build it; bench-tui only adds markers to a bench-tree tree",
            tree.display(),
            bench_tree::SENTINEL
        );
    }
    let mut projects = 0;
    for entry in fs::read_dir(tree).with_context(|| format!("could not read {}", tree.display()))? {
        let project = entry?.path();
        if !is_real(&project, fs::FileType::is_dir) {
            continue;
        }
        projects += 1;
        let target = project.join("target");
        if is_real(&target, fs::FileType::is_dir) {
            add_marker(&project.join("Cargo.toml"), "[package]\nname = \"bench\"\n")?;
            add_marker(
                &target.join("CACHEDIR.TAG"),
                "Signature: 8a477f597d28d172789f06886806bc55\n",
            )?;
        }
        let node_modules = project.join("node_modules");
        if is_real(&node_modules, fs::FileType::is_dir) {
            add_marker(&project.join("package.json"), "{}\n")?;
            add_marker(&node_modules.join(".package-lock.json"), "{}\n")?;
        }
    }
    Ok(projects)
}

/// Whether `path` itself, not what a link at it points to, is of the `kind`.
fn is_real(path: &Path, kind: fn(&fs::FileType) -> bool) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| kind(&meta.file_type()))
}

/// Writes a new file. Anything already at `path`, a link included, is left as it is.
fn add_marker(path: &Path, content: &str) -> Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file
            .write_all(content.as_bytes())
            .with_context(|| format!("could not write {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not create {}", path.display())),
    }
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

/// What the home screen writes besides frames, such as a clipboard escape for `y`,
/// which the benchmark never presses. Dropped.
impl io::Write for Timed {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-project tree with both hot spots, carrying the sentinel when `generated`.
    fn tree(generated: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        if generated {
            fs::write(dir.path().join(bench_tree::SENTINEL), "sentinel\n").unwrap();
        }
        let project = dir.path().join("project-0000");
        fs::create_dir_all(project.join("target")).unwrap();
        fs::create_dir_all(project.join("node_modules")).unwrap();
        dir
    }

    /// Every file under `dir` with its bytes, sorted.
    fn files(dir: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let mut found = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                found.extend(files(&path));
            } else {
                found.push((path.clone(), fs::read(&path).unwrap_or_default()));
            }
        }
        found.sort();
        found
    }

    #[test]
    fn refuses_a_tree_without_the_sentinel_and_writes_nothing() {
        let dir = tree(false);
        fs::write(
            dir.path().join("project-0000").join("keep.txt"),
            "user data",
        )
        .unwrap();
        let before = files(dir.path());

        let error = mark_projects(dir.path()).unwrap_err();

        assert!(error.to_string().contains(bench_tree::SENTINEL), "{error}");
        assert_eq!(files(dir.path()), before);
    }

    #[test]
    fn existing_manifests_survive_marking_byte_for_byte() {
        let dir = tree(true);
        let project = dir.path().join("project-0000");
        let cargo = b"[package]\nname = \"mine\"\nversion = \"1.2.3\"\n";
        let package = b"{\"name\": \"mine\", \"private\": true}\n";
        fs::write(project.join("Cargo.toml"), cargo).unwrap();
        fs::write(project.join("package.json"), package).unwrap();

        assert_eq!(mark_projects(dir.path()).unwrap(), 1);

        assert_eq!(fs::read(project.join("Cargo.toml")).unwrap(), cargo);
        assert_eq!(fs::read(project.join("package.json")).unwrap(), package);
        assert!(project.join("target").join("CACHEDIR.TAG").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_manifest_linked_outside_the_tree_is_not_followed() {
        let dir = tree(true);
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("Cargo.toml");
        fs::write(&victim, "precious\n").unwrap();
        let missing = outside.path().join("package.json");
        let project = dir.path().join("project-0000");
        std::os::unix::fs::symlink(&victim, project.join("Cargo.toml")).unwrap();
        std::os::unix::fs::symlink(&missing, project.join("package.json")).unwrap();

        mark_projects(dir.path()).unwrap();

        assert_eq!(fs::read(&victim).unwrap(), b"precious\n");
        assert!(!missing.exists(), "a dangling link was followed");
    }

    #[cfg(unix)]
    #[test]
    fn a_hot_spot_linked_outside_the_tree_gets_no_marker() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(bench_tree::SENTINEL), "sentinel\n").unwrap();
        let project = dir.path().join("project-0000");
        fs::create_dir(&project).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), project.join("target")).unwrap();
        std::os::unix::fs::symlink(outside.path(), project.join("node_modules")).unwrap();

        mark_projects(dir.path()).unwrap();

        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
