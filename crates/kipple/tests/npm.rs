//! npm's download cache end to end (spec M4 pinned 33, `docs/04-safety-model.md` §6): the
//! root the platform resolves from npm's own configuration and what stays closed, then a
//! semantic plan from scan to receipt through the command line and the home screen, run by
//! a fake `node` that stands in for npm. Every root is redirected into a temp dir, so no
//! real npm, `~/.npm` or `.npmrc` is read and nothing of the real home is touched. Unix
//! only: on Windows npm is found as `npm.cmd`, which these installs don't have.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "a temp dir or path that can't be made fails the test"
)]

#[path = "../../kipple-adapters/tests/fixtures/tree.rs"]
mod tree;

#[path = "../../kipple-adapters/tests/fixtures/npm/fake_node.rs"]
mod fake_node;
#[path = "../../kipple-adapters/tests/fixtures/npm/v11_20_0.rs"]
mod npm_v11_20_0;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead, Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use clap::Parser as _;
use kipple::config::{self, Settings};
use kipple::doctor::{ConfigStatus, Probed, Report, RootCheck};
use kipple::setup::Builtin;
use kipple::theme::{Catalog, Colours, Glyphs, Ink};
use kipple::tui::{self, Appearance, Input, Key, Keys};
use kipple::{Answers, Cli, Io, Person};
use kipple_adapters::builtin::descriptors;
use kipple_core::{
    CancelToken, FsProbe as _, HeldPath, IntegrationId, Inventory, Lineage, NativeTool, Observer,
    Owners, Platform, ProcessSnapshot, Protections, Resolution, Roots, SymbolicRoot,
    ToolResolver as _, Waited,
};
use kipple_platform::{Environment, NoFollowFs, PLATFORM, PathTools, kipple_dirs, resolve_roots};
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use tree::FixtureTree;

// `tree.rs` keeps `dead_code` off the builder methods most binaries leave unused, but not
// this one, which every other binary that includes it uses.
const _: () = {
    let _ = FixtureTree::modified;
    let _ = (npm_v11_20_0::VERSION, npm_v11_20_0::ENGINES);
};

const CACHE: &str = "builtin.npm.cache";
const INVENTORY: &str = "builtin.npm.inventory";

/// What every `.npmrc` here holds beside its settings: a registry token, which nothing
/// kipple writes or shows may carry.
const TOKEN: &str = "npm_kippleTestToken0123456789abcdefABCDEF";

/// The clean runs only on Linux (spec M4 pinned 39): elsewhere the cache is reported.
const CLEANS: bool = matches!(PLATFORM, Platform::Linux);

/// What a home screen of this size shows fits on one line.
const WIDTH: u16 = 200;
const HEIGHT: u16 = 60;

/// What a path's ancestors are, as the host reads them.
fn lineage(path: &Path) -> Lineage {
    let identity = |at: &Path| NoFollowFs.metadata(at).ok().and_then(|m| m.identity);
    Lineage(path.ancestors().map(identity).collect())
}

/// Whether the host keeps `integration` out of `path`.
fn closed(roots: &Roots, data: &Path, path: &Path, integration: &'static str) -> bool {
    let held = |at: &Path| HeldPath {
        forms: vec![at.to_owned()],
        lineage: lineage(at),
        note: None,
    };
    let protections = Protections::of_host(roots, Some(data), &descriptors(), &held);
    let found = protections.check(
        path,
        &lineage(path),
        IntegrationId(integration),
        &Owners::default(),
    );
    found.is_some()
}

fn make_runnable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A machine in a temp dir: npm 11.20.0 installed below `npm/` with a fake `node` beside
/// it, the only directory on `PATH`, and its cache at `~/.npm` as an install and an `npx`
/// run leave it, with a `.npmrc` holding [`TOKEN`].
struct Machine {
    _temp: tempfile::TempDir,
    at: PathBuf,
    /// The roots file that points every root kipple reads into the temp dir.
    roots: PathBuf,
}

impl Machine {
    fn new(npmrc: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let at = temp.path().canonicalize().unwrap();
        let mut tree = FixtureTree::new();
        npm_v11_20_0::install(&mut tree, "npm");
        npm_v11_20_0::cache(&mut tree, "home/.npm");
        tree.file("home/.npmrc", npmrc.as_bytes())
            .dir("home/.config")
            .dir("home/.cache")
            .dir("home/.local/share")
            .dir("home/.local/state");
        tree.build_in(&at).unwrap();
        make_runnable(&at.join("npm/lib/node_modules/npm/bin/npm-cli.js"));
        fake_node::build(&at.join("npm/bin"), npm_v11_20_0::NODE).unwrap();
        let mut file = String::new();
        for (name, relative) in [
            ("home", "home"),
            ("xdg.config", "home/.config"),
            ("xdg.cache", "home/.cache"),
            ("xdg.data", "home/.local/share"),
            ("xdg.state", "home/.local/state"),
            ("npm.cache", "home/.npm"),
            ("npm.default-cache", "home/.npm"),
        ] {
            writeln!(file, "{name} = {}", at.join(relative).display()).unwrap();
        }
        // The cache was filled two months ago: outside every level's recent window.
        let now = SystemTime::now() + Duration::from_hours(24 * 60);
        let seconds = now.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        writeln!(file, "now = {}", seconds.as_secs()).unwrap();
        let roots = at.join("roots.txt");
        fs::write(&roots, file).unwrap();
        Self {
            _temp: temp,
            at,
            roots,
        }
    }

    fn at(&self, relative: &str) -> PathBuf {
        self.at.join(relative)
    }

    /// The download cache's `_cacache`.
    fn cacache(&self) -> PathBuf {
        self.at("home/.npm/_cacache")
    }

    /// The machine as a run sees it: the roots file, the home, and `PATH` with npm alone.
    fn env(&self) -> Environment {
        let vars = [
            ("KIPPLE_TEST_ROOTS", self.roots.clone().into_os_string()),
            ("HOME", self.at("home").into_os_string()),
            ("PATH", self.at("npm/bin").into_os_string()),
        ];
        Environment::from_vars(vars.map(|(name, value)| (OsString::from(name), value)))
    }

    /// The machine as the platform resolves it without the test-roots override: its home
    /// and npm alone, the system's fixed paths in the temp dir.
    fn resolved_env(&self) -> Environment {
        let vars = [
            ("HOME", self.at("home").into_os_string()),
            ("PATH", self.at("npm/bin").into_os_string()),
        ];
        Environment::from_vars(vars.map(|(name, value)| (OsString::from(name), value)))
            .with_system_root(self.at("no-system"))
    }

    /// Makes the fake npm end `cache clean` with `code`, having removed only part of the
    /// cache, as npm does when a file in it can't be removed.
    fn npm_exits_with(&self, code: u8) {
        fs::write(self.at("npm/bin/node.exit"), code.to_string()).unwrap();
    }

    /// Each command the fake node was started with, one line each: its arguments, a tab,
    /// and its working directory.
    fn started(&self) -> Vec<String> {
        let log = fs::read_to_string(self.at("npm/bin/node.log")).unwrap_or_default();
        log.lines().map(str::to_owned).collect()
    }

    /// The `cache clean` commands among [`Machine::started`].
    fn cleans(&self) -> Vec<String> {
        let started = self.started();
        started
            .into_iter()
            .filter(|line| line.contains(" cache clean"))
            .collect()
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

    /// Runs `kipple <args>` with nobody at a terminal.
    fn run(&self, args: &[&str]) -> Ran {
        let cli = Cli::parse_from(std::iter::once("kipple").chain(args.iter().copied()));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = kipple::run(&cli, &self.env(), Io::redirected(&mut out, &mut err));
        Ran {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    /// Opens the home screen, presses `keys` in turn once it is idle, looking at the screen
    /// before each, then `q`. Returns what it looked at and the last frame drawn.
    fn home(&self, keys: &[char]) -> Frames {
        let builtin = Builtin::load().unwrap();
        let env = self.env();
        let settings = Settings::load(&env, None, &[], &config::catalog(&builtin)).unwrap();
        let screen = Rc::new(RefCell::new(TestBackend::new(WIDTH, HEIGHT)));
        let mut terminal = Terminal::new(Shared(Rc::clone(&screen))).unwrap();
        let mut script = Script {
            keys: keys.iter().copied().map(Key::Char).collect(),
            screen: Rc::clone(&screen),
            looked: Vec::new(),
        };
        let appearance =
            Appearance::new(Catalog::builtin(), "kipple", Colours::Off, Glyphs::Ascii).unwrap();

        tui::run(&mut terminal, &mut script, &env, &settings, appearance).unwrap();

        let last = text(&screen.borrow());
        Frames {
            looked: script.looked,
            last,
        }
    }

    /// Every file below kipple's data directory, as text: its journal, receipts and runs.
    fn stored(&self) -> String {
        let mut stored = String::new();
        read_below(&self.at("home/.local/share"), &mut stored);
        stored
    }
}

/// Appends every file below `dir` to `text`.
fn read_below(dir: &Path, text: &mut String) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            read_below(&path, text);
        } else {
            text.push_str(&String::from_utf8_lossy(&fs::read(&path).unwrap()));
        }
    }
}

/// What a run of `kipple` in this process did.
struct Ran {
    code: ExitCode,
    out: String,
    err: String,
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

/// Keys a person presses once the screen is idle, looking at it before each.
struct Script {
    keys: VecDeque<Key>,
    screen: Rc<RefCell<TestBackend>>,
    looked: Vec<String>,
}

impl Keys for Script {
    fn next(&mut self, timeout: Duration) -> io::Result<Option<Input>> {
        // The loop waits a frame at most while something runs, and longer once idle.
        if timeout <= Duration::from_millis(16) {
            return Ok(None);
        }
        self.looked.push(text(&self.screen.borrow()));
        let key = self.keys.pop_front().unwrap_or(Key::Char('q'));
        Ok(Some(Input::Key(key)))
    }
}

/// The frames a script looked at, and the last one drawn.
struct Frames {
    looked: Vec<String>,
    last: String,
}

/// `screen`'s buffer as text, each row trimmed.
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
    text
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

/// The plain cache setting every machine here starts with, beside the token.
fn npmrc() -> String {
    format!("cache=~/.npm\n//registry.npmjs.org/:_authToken={TOKEN}\n")
}

/// Catches the interim the Packs review found: a cache moved by `.npmrc` read as absent, so
/// the `node_modules` an `npx` run leaves below it is open to the Node integration, which
/// would offer a cache npm reuses as a project's dependencies.
#[test]
fn a_cache_moved_by_npmrc_keeps_node_out_of_npx() {
    let machine = Machine::new(&format!(
        "cache=~/code/npm-cache\n//registry.npmjs.org/:_authToken={TOKEN}\n"
    ));
    let mut tree = FixtureTree::new();
    npm_v11_20_0::cache(&mut tree, "home/code/npm-cache");
    tree.file("home/code/app/package.json", b"{}");
    tree.build_in(&machine.at).unwrap();
    let roots = resolve_roots(&machine.resolved_env(), &[]);
    let cache = machine.at("home/code/npm-cache");
    let resolved = roots.get(SymbolicRoot::NpmCache).unwrap().as_ref().unwrap();
    assert_eq!(resolved.path, cache);
    let data = machine.at("home/.local/share/kipple");

    let npx = cache.join("_npx/0123456789abcdef/node_modules");
    let project = machine.at("home/code/app");

    assert!(
        closed(&roots, &data, &npx, "node"),
        "npx's node_modules is closed"
    );
    assert!(
        !closed(&roots, &data, &project, "node"),
        "a project beside the cache stays open"
    );
}

/// Catches a semantic plan that never reaches its command, runs one other than the review
/// showed, or takes more than `_cacache`: from scan to receipt through the command line
/// and through the home screen, a person who reviewed `npm cache clean` and said yes gets
/// exactly that command, in the cache root, and `_npx` and `_logs` stay. Off Linux the
/// cache is only reported, and nothing runs.
#[test]
fn semantic_plan_end_to_end() {
    let cli = Machine::new(&npmrc());
    let screen = Machine::new(&npmrc());

    let scan = cli.run(&["scan"]);
    let cleaned = cli.answer(&["clean"], "y\n");
    let frames = screen.home(&['c', 'y']);

    assert_eq!(scan.code, ExitCode::SUCCESS, "{}", scan.err);
    let cacache = cli.cacache().display().to_string();
    let line = scan.out.lines().find(|line| line.ends_with(&cacache));
    let rule = if CLEANS { CACHE } else { INVENTORY };
    assert!(line.is_some_and(|line| line.contains(rule)), "{}", scan.out);
    for machine in [&cli, &screen] {
        ran_only_the_reviewed_clean(machine);
    }
    if !CLEANS {
        return;
    }
    assert_eq!(cleaned.code, ExitCode::SUCCESS, "{}", cleaned.err);
    let command = "npm cache clean --force --cache=/proc/self/cwd";
    assert!(cleaned.err.contains(command), "{}", cleaned.err);
    assert!(
        cleaned.out.contains("done         native"),
        "{}",
        cleaned.out
    );
    let review = &frames.looked[1];
    assert!(review.contains("Review: clean 1 item"), "{review}");
    assert!(review.contains(command), "{review}");
    assert!(
        frames.last.contains("done         native"),
        "{}",
        frames.last
    );
}

/// That `machine` kept `_npx` and `_logs`, and where the clean runs, started npm's script
/// once with `cache clean` in the cache root and lost `_cacache`; elsewhere started none.
fn ran_only_the_reviewed_clean(machine: &Machine) {
    for kept in ["home/.npm/_npx", "home/.npm/_logs"] {
        assert!(machine.at(kept).exists(), "{kept} went");
    }
    let ran = machine.cleans();
    if !CLEANS {
        assert!(machine.cacache().exists(), "the cache went");
        assert_eq!(ran, Vec::<String>::new());
        return;
    }
    assert!(!machine.cacache().exists(), "the cache stays");
    let script = machine.at("npm/lib/node_modules/npm/bin/npm-cli.js");
    let expected = format!(
        "{} cache clean --force --cache=/proc/self/cwd",
        script.display()
    );
    assert_eq!(ran.len(), 1, "{ran:?}");
    let (args, cwd) = ran.first().unwrap().split_once('\t').unwrap();
    assert!(args.starts_with(&expected), "{args}");
    assert_eq!(Path::new(cwd), machine.at("home/.npm"));
}

/// Catches npm's exit 1 or 243 read as done, though npm stops at the first entry it can't
/// remove and leaves the rest, or the command run again by the next clean: either code
/// with part of the cache gone is an item kipple can't tell about, npm's own words are
/// shown, and the remains are held, not cleaned.
#[test]
fn exit_1_and_243_are_partial_failures() {
    if !CLEANS {
        return;
    }
    for code in [1, 243] {
        let machine = Machine::new(&npmrc());
        machine.npm_exits_with(code);

        let ran = machine.answer(&["clean"], "y\n");
        let again = machine.answer(&["clean"], "y\n");

        assert_eq!(ran.code, ExitCode::from(1), "{code}: {}", ran.err);
        assert!(
            ran.out.contains("can't tell   native"),
            "{code}: {}",
            ran.out
        );
        let exit = format!("command failed (exit code {code})");
        assert!(ran.out.contains(&exit), "{code}: {}", ran.out);
        assert!(ran.err.contains("| npm error code EACCES"), "{}", ran.err);
        assert!(
            machine.at("home/.npm/_cacache/content-v2").exists(),
            "{code}: the fake left this"
        );
        assert_eq!(machine.cleans().len(), 1, "{code}: {}", again.err);
    }
}

/// Catches `--dry-run` reaching npm, which deletes anyway when given it: a dry run starts
/// no `cache clean` at all, and the real run's command never holds the flag.
#[test]
fn dry_run_never_passed() {
    let machine = Machine::new(&npmrc());

    let dry = machine.answer(&["clean", "--dry-run"], "");
    let started_by_dry_run = machine.cleans();
    machine.answer(&["clean"], "y\n");

    assert!(
        dry.out.contains("Dry run: nothing was changed."),
        "{}",
        dry.out
    );
    assert_eq!(started_by_dry_run, Vec::<String>::new());
    let started = machine.started();
    assert!(
        started.iter().all(|line| !line.contains("dry-run")),
        "{started:?}"
    );
    assert_eq!(machine.cleans().len(), usize::from(CLEANS), "{started:?}");
}

/// Catches the clean run by consent given ahead (founder decision 16): `--yes` naming its
/// rule, and a plan file, leave it out and say it needs a review, and nothing starts npm
/// but its version check.
#[test]
fn yes_never_runs_it() {
    let machine = Machine::new(&npmrc());

    let yes = machine.run(&["clean", "--only", CACHE, "--yes"]);
    let plan = machine.run(&["plan", "-o", "-"]);

    assert!(machine.cacache().exists(), "the cache went");
    assert_eq!(machine.cleans(), Vec::<String>::new());
    let plan_file: serde_json::Value = serde_json::from_str(&plan.out).unwrap();
    assert_eq!(plan_file["items"], serde_json::json!([]), "{}", plan.out);
    if CLEANS {
        let refused = format!(
            "{}: a tool's own command runs only after you review it",
            machine.cacache().display()
        );
        assert!(yes.out.contains(&refused), "{}", yes.out);
        assert!(plan.err.contains(&refused), "{}", plan.err);
    }
}

/// Catches a registry token in an `.npmrc` copied into what kipple says or keeps: npm's
/// configuration is read for the cache's place alone. Every way the user npmrc can fail
/// holds the token beside the failing line, and neither the roots, `doctor`, a scan, a
/// clean's prompt and receipt, the journal and run files, nor the command npm is given
/// carries it.
#[test]
fn token_in_npmrc_appears_nowhere() {
    let token = format!("//registry.npmjs.org/:_authToken={TOKEN}\n");
    let npmrcs = [
        npmrc(),
        format!("{token}cache=${{KIPPLE_TEST_UNSET}}/{TOKEN}\n"),
        format!("{token}cache={TOKEN}\n"),
        format!("{token}cache=\"{TOKEN}\n"),
        format!("{token}cache=~/one\nCACHE=~/{TOKEN}\n"),
    ];
    for npmrc in npmrcs {
        let machine = Machine::new(&npmrc);
        let roots = resolve_roots(&machine.resolved_env(), &[]);
        let mut said = format!("{roots:?}");
        for Resolution { resolved, .. } in roots.iter() {
            if let Err(error) = resolved {
                writeln!(said, "{error}").unwrap();
            }
        }
        let report = doctor(&machine, &roots);
        let mut printed = Vec::new();
        report.write_to(&mut printed, &Ink::plain()).unwrap();
        said.push_str(&String::from_utf8(printed).unwrap());
        said.push_str(&serde_json::to_string(&report.to_json()).unwrap());

        assert!(!said.contains(TOKEN), "{npmrc}: {said}");
    }
    let machine = Machine::new(&npmrc());
    let scan = machine.run(&["scan", "--json"]);
    let cleaned = machine.answer(&["clean"], "y\n");
    let history = machine.run(&["history", "--json"]);
    for (what, text) in [
        ("scan", scan.out + &scan.err),
        ("clean", cleaned.out + &cleaned.err),
        ("history", history.out + &history.err),
        ("stored", machine.stored()),
        ("npm", machine.started().join("\n")),
    ] {
        assert!(!text.contains(TOKEN), "{what}: {text}");
    }
    assert!(
        !machine.cacache().exists() || !CLEANS,
        "the clean didn't run"
    );
}

/// `doctor`'s report of `roots` on `machine`, as `kipple doctor` builds it, without the
/// process table it also reads.
fn doctor(machine: &Machine, roots: &Roots) -> Report {
    let env = machine.resolved_env();
    let checks = roots
        .iter()
        .map(|Resolution { root, resolved }| RootCheck {
            root,
            status: resolved.clone().map(|at| Probed {
                meta: NoFollowFs.metadata(&at.path),
                at,
            }),
        })
        .collect();
    Report {
        dirs: kipple_dirs(roots),
        config_file: None,
        config: ConfigStatus::default(),
        roots: checks,
        integrations: Vec::new(),
        tools: Vec::new(),
        npm: PathTools::new(&env).resolve(NativeTool::Npm),
        processes: Ok(ProcessSnapshot {
            processes: Vec::new(),
            inventory: Inventory::Complete,
            observer: Observer::PLAIN,
        }),
        permission_hint: None,
    }
}
