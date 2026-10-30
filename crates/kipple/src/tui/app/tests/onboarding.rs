//! The first run (`docs/03-functional-spec.md` §2 J0): its steps, what finishing writes
//! to a config file in a temp dir through the same `config::write` the loop calls, the
//! rescan a change of roots asks for, and the screens at 80×24 and 160×48.

use std::fs;
use std::path::PathBuf;

use kipple_core::{
    Advice, IntegrationReport, Level, Method, ResolvedRoot, RootSource, Roots, ScanOutcome,
    ScanReport, Suggestion, SymbolicRoot,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use super::{PACMAN, colour, home, probe, styled};
use crate::config::{
    self, Catalog, Config, ConfigPolicy, Edit, Loaded, Location, Settings, Source,
};
use crate::theme::Look;
use crate::tui::app::{App, Effect, Key, Msg, Saved};
use crate::tui::onboarding::{self, Onboarding, Outcome, Step};
use crate::tui::settings::Found;

/// Whether a config file is there.
#[derive(Clone, Copy, PartialEq, Eq)]
enum File {
    Exists,
    Missing,
}

/// A temp dir with a home directory holding `projects` and `code`, which kipple found as
/// project roots, and no config file at the default location yet.
struct Disk {
    temp: tempfile::TempDir,
    roots: Roots,
    catalog: Catalog,
}

impl Disk {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let mut roots = Roots::default();
        roots.insert(
            SymbolicRoot::Home,
            Ok(ResolvedRoot::new(
                SymbolicRoot::Home,
                home.clone(),
                RootSource::Env("HOME"),
            )),
        );
        for name in ["projects", "code"] {
            fs::create_dir_all(home.join(name)).unwrap();
            let source = RootSource::Default {
                base: SymbolicRoot::Home,
                relative: name,
            };
            let root = ResolvedRoot::new(SymbolicRoot::ProjectRoots, home.join(name), source);
            roots.add(SymbolicRoot::ProjectRoots, Ok(root));
        }
        let catalog = super::catalog().rules.into_keys().collect();
        Self {
            temp,
            roots,
            catalog,
        }
    }

    fn home(&self) -> PathBuf {
        self.temp.path().join("home")
    }

    fn file(&self) -> PathBuf {
        self.temp.path().join("config").join("config.toml")
    }

    fn location(&self) -> Location {
        Location {
            source: Source::Default,
            path: Some(self.file()),
        }
    }

    fn settings(&self, file: File, config: Config) -> Settings {
        let ConfigPolicy { policy, warnings } = config.policy(&self.roots);
        Settings {
            loaded: Loaded {
                location: self.location(),
                exists: file == File::Exists,
                config,
            },
            roots: self.roots.clone(),
            policy,
            warnings,
        }
    }

    /// The home screen as it opens without a config file, with `demo-home`-like findings.
    fn first_run(&self) -> (App, Onboarding) {
        let mut app = home();
        app.configure(Saved::of(&self.settings(File::Missing, Config::default())));
        let onboarding = Onboarding::new(&app);
        (app, onboarding)
    }

    /// Does what the loop does once the questions are finished: asks the home screen to
    /// write, writes, and answers. The effects it doesn't carry out, in order.
    fn finish(&self, app: &mut App, answers: Vec<Edit>) -> Vec<Effect> {
        onboarding::finish(app, answers);
        let mut left = Vec::new();
        while let Some(effect) = app.take_effect() {
            let Effect::Write(edits) = effect else {
                left.push(effect);
                continue;
            };
            let written = config::write(&self.location(), &edits, &self.roots, &self.catalog);
            app.update(match written {
                Ok(config) => Msg::Saved(Box::new(Saved::of(&self.settings(File::Exists, config)))),
                Err(error) => Msg::NotSaved(error.to_string()),
            });
        }
        left
    }

    fn text(&self) -> String {
        fs::read_to_string(self.file()).unwrap()
    }
}

/// Presses `keys` and returns the outcome of the last one.
fn answer(onboarding: &mut Onboarding, keys: &[Key]) -> Outcome {
    let mut outcome = Outcome::Stay;
    for &key in keys {
        outcome = onboarding.key(key);
    }
    outcome
}

fn finished(outcome: Outcome) -> Vec<Edit> {
    match outcome {
        Outcome::Finish(edits) => edits,
        other @ (Outcome::Stay | Outcome::Quit) => panic!("not finished: {other:?}"),
    }
}

fn screen(onboarding: &Onboarding, app: &App, width: u16, height: u16) -> String {
    let buffer = drawn(onboarding, app, width, height);
    let mut text = String::new();
    for y in 0..height {
        let line: String = (0..width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
            .collect();
        text.push_str(line.trim_end());
        text.push('\n');
    }
    // Paths shown from `~` are written with `/`, so a snapshot reads the same on Windows.
    text.replace(std::path::MAIN_SEPARATOR, "/")
}

fn drawn(onboarding: &Onboarding, app: &App, width: u16, height: u16) -> Buffer {
    drawn_in(colour(), onboarding, app, (width, height))
}

fn drawn_in(look: Look<'_>, onboarding: &Onboarding, app: &App, size: (u16, u16)) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
    terminal
        .draw(|frame| crate::tui::ui::onboarding::render(onboarding, app, look, frame))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn report_with_advice() -> ScanReport {
    ScanReport {
        outcome: ScanOutcome::Completed,
        integrations: vec![IntegrationReport {
            id: PACMAN,
            omissions: Vec::new(),
            opened: Vec::new(),
            advice: vec![Advice {
                title: "Keep fewer old packages with paccache".to_owned(),
                text: "paccache removes cached packages except the newest versions of each."
                    .to_owned(),
                snippets: vec!["sudo paccache -rk3".to_owned()],
                references: Vec::new(),
            }],
        }],
        findings: Vec::new(),
        unchecked_system: 0,
        by_command_line: 0,
    }
}

// Catches a first run that writes nothing when every step is skipped, so it comes back
// on every start, or one that writes the found roots and freezes the defaults.
#[test]
fn skipping_every_step_writes_only_care_level_balanced() {
    let disk = Disk::new();
    let (mut app, mut first) = disk.first_run();

    let edits = finished(answer(&mut first, &[Key::Tab; 4]));

    assert_eq!(edits, [Edit::Level(Level::Balanced)]);
    assert_eq!(disk.finish(&mut app, edits), []);
    let written = config::load(&disk.location(), &disk.catalog).unwrap();
    assert!(written.exists, "skipping every step still writes a config");
    assert_eq!(
        written.config,
        Config {
            level: Some(Level::Balanced),
            ..Config::default()
        }
    );
    assert!(!onboarding::due(&written), "the first run would come back");
}

// Catches an answer that isn't written, a key that is written without being chosen, or
// the home screen keeping the level it opened with.
#[test]
fn finishing_writes_exactly_the_answers_in_one_batch_and_rebuckets_at_the_level() {
    let disk = Disk::new();
    let (mut app, mut first) = disk.first_run();

    // Thorough; the roots as found; Trash; finish.
    let keys = [
        Key::Down,
        Key::Enter,
        Key::Enter,
        Key::Down,
        Key::Enter,
        Key::Enter,
    ];
    let edits = finished(answer(&mut first, &keys));

    assert_eq!(
        edits,
        [Edit::Level(Level::Thorough), Edit::Method(Method::Trash)]
    );
    assert_eq!(
        disk.finish(&mut app, edits),
        [],
        "unchanged roots rescanned"
    );
    let text = disk.text();
    assert!(text.contains("level = \"thorough\""), "{text}");
    assert!(text.contains("method = \"trash\""), "{text}");
    assert!(!text.contains("project_roots"), "{text}");
    assert_eq!(app.policy.level, Level::Thorough);
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.contains("saved in"), "{notice}");
}

// Catches a skipped step keeping the answer given before going back to it.
#[test]
fn going_back_and_skipping_a_step_drops_its_earlier_answer() {
    let (_, mut first) = Disk::new().first_run();
    answer(&mut first, &[Key::Enter, Key::Enter, Key::Down, Key::Enter]);
    assert_eq!(first.step, Step::Tips);

    answer(&mut first, &[Key::Esc]);
    assert_eq!(first.step, Step::Method);
    let edits = finished(answer(&mut first, &[Key::Tab, Key::Enter]));

    assert_eq!(edits, [Edit::Level(Level::Balanced)]);
}

// Catches esc on the first step leaving the questions, or back losing where the cursor
// was.
#[test]
fn esc_goes_back_one_step_and_stays_on_the_first() {
    let mut first = Onboarding::new(&home());
    answer(&mut first, &[Key::Esc]);
    assert_eq!(first.step, Step::Level);

    answer(&mut first, &[Key::Up, Key::Enter]);
    assert_eq!(first.step, Step::Roots);
    answer(&mut first, &[Key::Esc]);

    assert_eq!(first.step, Step::Level);
    assert_eq!(first.level, Level::Careful);
}

// Catches quitting writing the answers so far, which would skip the questions next time.
#[test]
fn quitting_at_any_step_even_while_typing_writes_nothing() {
    let disk = Disk::new();
    for (steps, quit) in [
        (0, Key::Char('q')),
        (2, Key::Char('q')),
        (3, Key::Interrupt),
    ] {
        let (mut app, mut first) = disk.first_run();
        answer(&mut first, &vec![Key::Enter; steps]);
        assert_eq!(first.key(quit), Outcome::Quit, "after {steps} steps");
        assert_eq!(app.take_effect(), None);
    }

    let (_, mut first) = disk.first_run();
    answer(&mut first, &[Key::Enter, Key::Down, Key::Down, Key::Enter]);
    assert!(first.typing.is_some(), "the root step is typing");
    assert_eq!(first.key(Key::Char('q')), Outcome::Stay, "q is typed");
    assert_eq!(first.key(Key::Interrupt), Outcome::Quit);
    assert!(
        !disk.file().exists(),
        "quitting while typing writes no config"
    );
}

// Catches a change of project roots that the home screen doesn't scan, or that writes
// the unchecked root.
#[test]
fn a_change_of_roots_is_written_and_the_home_screen_opens_on_a_rescan() {
    let disk = Disk::new();
    let (mut app, mut first) = disk.first_run();
    answer(&mut first, &[Key::Enter]);
    assert_eq!(first.step, Step::Roots);

    // Uncheck `projects`, then add a root on the "add another" line.
    answer(
        &mut first,
        &[Key::Char(' '), Key::Down, Key::Down, Key::Enter],
    );
    for c in "~/work".chars() {
        first.key(Key::Char(c));
    }
    answer(&mut first, &[Key::Enter]);
    let edits = finished(answer(&mut first, &[Key::Enter, Key::Tab, Key::Tab]));

    let roots = vec![disk.home().join("code"), PathBuf::from("~/work")];
    assert_eq!(
        edits,
        [Edit::Level(Level::Balanced), Edit::ProjectRoots(roots)]
    );
    assert_eq!(disk.finish(&mut app, edits), [Effect::Rescan]);
    let written = config::load(&disk.location(), &disk.catalog).unwrap();
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
        written.config.project_roots,
        Some(vec![format!("~{sep}code"), "~/work".to_owned()])
    );
    assert!(app.rows.is_empty(), "the old scan's findings stayed");
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.contains("Scanning again"), "{notice}");
}

// Catches unchecking every root writing nothing, which leaves the defaults scanned while
// the user chose none.
#[test]
fn unchecking_every_root_writes_an_empty_list_and_rescans() {
    let disk = Disk::new();
    let (mut app, mut first) = disk.first_run();
    answer(&mut first, &[Key::Enter]);

    answer(&mut first, &[Key::Char(' '), Key::Down, Key::Char(' ')]);
    let edits = finished(answer(&mut first, &[Key::Enter, Key::Tab, Key::Tab]));

    assert_eq!(
        edits,
        [Edit::Level(Level::Balanced), Edit::ProjectRoots(Vec::new())]
    );
    assert_eq!(disk.finish(&mut app, edits), [Effect::Rescan]);
    assert!(
        disk.text().contains("project_roots = []"),
        "{}",
        disk.text()
    );
}

// Catches a typed relative path being accepted, which would make the whole batch fail
// to write and every answer be lost.
#[test]
fn a_relative_root_is_refused_while_typing_and_can_be_corrected() {
    let mut first = Onboarding::new(&home());
    answer(&mut first, &[Key::Enter, Key::Up, Key::Enter]);
    for c in "work".chars() {
        first.key(Key::Char(c));
    }
    answer(&mut first, &[Key::Enter]);

    assert!(first.typing.is_some(), "a relative path was taken");
    assert!(first.notice.is_some(), "a relative root shows a notice");
    assert_eq!(first.roots.len(), 0, "{:?}", first.roots);
}

// Catches a root already in the list being added again under another spelling, which
// would scan the same tree twice.
#[test]
fn a_root_already_in_the_list_is_refused_however_it_is_spelled() {
    let (_, mut first) = Disk::new().first_run();
    answer(&mut first, &[Key::Enter, Key::Down, Key::Down, Key::Enter]);
    let sep = std::path::MAIN_SEPARATOR;
    for c in format!("~{sep}Code{sep}").chars() {
        first.key(Key::Char(c));
    }
    answer(&mut first, &[Key::Enter]);

    assert_eq!(first.roots.len(), 2, "{:?}", first.roots);
    let notice = first.notice.as_deref().unwrap();
    assert!(notice.contains("is already in the list"), "{notice}");
}

// Catches the first run appearing over a config file the user has, or one named on the
// command line or in the environment, or with no known place to save its answers.
#[test]
fn the_first_run_is_due_only_without_a_file_at_a_known_default_location() {
    let disk = Disk::new();
    let loaded = |source, path: Option<PathBuf>, exists| Loaded {
        location: Location { source, path },
        exists,
        config: Config::default(),
    };

    assert!(
        onboarding::due(&loaded(Source::Default, Some(disk.file()), false)),
        "the first run is due without a file at the default location"
    );
    assert!(
        !onboarding::due(&loaded(Source::Default, Some(disk.file()), true)),
        "a file at the default location means the first run isn't due"
    );
    assert!(
        !onboarding::due(&loaded(Source::Default, None, false)),
        "an unknown default location means the first run isn't due"
    );
    for named in [Source::Flag, Source::Env] {
        assert!(
            !onboarding::due(&loaded(named, Some(disk.file()), true)),
            "a named config means the first run isn't due"
        );
    }
}

#[test]
fn step_1_at_80_by_24_shows_each_level_with_its_numbers() {
    let disk = Disk::new();
    let (app, first) = disk.first_run();
    insta::assert_snapshot!(screen(&first, &app, 80, 24));
}

#[test]
fn step_1_at_160_by_48_shows_each_level_with_its_numbers() {
    let disk = Disk::new();
    let (app, first) = disk.first_run();
    insta::assert_snapshot!(screen(&first, &app, 160, 48));
}

#[test]
fn steps_2_to_4_at_80_by_24() {
    // Fixed paths, so the snapshot doesn't hold the temp dir.
    let mut app = home();
    app.home = Some(PathBuf::from("/home/ada"));
    app.roots = vec![
        PathBuf::from("/home/ada/projects"),
        PathBuf::from("/home/ada/code"),
    ];
    app.update(Msg::Finished {
        report: Box::new(report_with_advice()),
        elapsed: std::time::Duration::from_secs(1),
    });
    let mut first = Onboarding::new(&app);
    answer(&mut first, &[Key::Enter, Key::Char(' ')]);
    insta::assert_snapshot!("step_2_at_80_by_24", screen(&first, &app, 80, 24));
    answer(&mut first, &[Key::Enter, Key::Down]);
    insta::assert_snapshot!("step_3_at_80_by_24", screen(&first, &app, 80, 24));
    answer(&mut first, &[Key::Enter]);
    insta::assert_snapshot!("step_4_at_80_by_24", screen(&first, &app, 80, 24));
}

// Catches a snippet that spans lines drawn as one line with a literal `\n` (F2).
#[test]
fn step_4_shows_each_line_of_a_snippet_on_its_own_line() {
    let mut app = home();
    let mut report = report_with_advice();
    if let Some(advice) = report
        .integrations
        .first_mut()
        .and_then(|integration| integration.advice.first_mut())
    {
        advice.snippets = vec!["[Journal]\nSystemMaxUse=200M".to_owned()];
    }
    app.update(Msg::Finished {
        report: Box::new(report),
        elapsed: std::time::Duration::from_secs(1),
    });
    let mut first = Onboarding::new(&app);
    answer(&mut first, &[Key::Tab, Key::Tab, Key::Tab]);

    let text = screen(&first, &app, 80, 24);
    assert!(!text.contains("\\n"), "{text}");
    for line in ["[Journal]", "SystemMaxUse=200M"] {
        let inside = |l: &str| l.trim_matches(|c| c == '│' || c == ' ').to_owned();
        assert!(text.lines().any(|l| inside(l) == line), "{line}: {text}");
    }
}

/// The step drawn in the probe theme, as styled text, after checking that every cell has
/// some role's colour: one drawn in the terminal's own is out of every theme's reach.
fn in_roles(onboarding: &Onboarding, app: &App) -> String {
    let buffer = drawn_in(probe(), onboarding, app, (160, 48));
    for cell in &buffer.content {
        assert_ne!(cell.fg, ratatui::style::Color::Reset, "{cell:?}");
    }
    styled(&buffer)
}

// Catches a colour or a modifier in the first run's steps changing while their drawing
// code moves: the text snapshots see only the characters.
#[test]
fn steps_1_to_4_at_160_by_48_in_colour() {
    let mut app = home();
    app.roots = vec![PathBuf::from("/home/ada/projects")];
    app.update(Msg::Finished {
        report: Box::new(report_with_advice()),
        elapsed: std::time::Duration::from_secs(1),
    });
    let mut first = Onboarding::new(&app);
    app.start_search();
    let paths = [PathBuf::from("/home/ada/notes")];
    suggest(&mut app, &mut first, &paths, Search::Stopped);
    let mut shown = vec![styled(&drawn(&first, &app, 160, 48))];
    let mut probed = vec![in_roles(&first, &app)];
    for keys in [&[Key::Enter, Key::Down][..], &[Key::Enter], &[Key::Enter]] {
        answer(&mut first, keys);
        shown.push(styled(&drawn(&first, &app, 160, 48)));
        probed.push(in_roles(&first, &app));
    }
    insta::assert_snapshot!(shown.join("\n"));
    // Catches a step asking for the wrong role, which the default theme can't show.
    insta::assert_snapshot!("steps_1_to_4_in_their_roles", probed.join("\n"));
}

// Catches step 4 left blank when no tool kipple found has a retention setting.
#[test]
fn step_4_says_when_no_tip_is_known() {
    let mut app = home();
    app.update(Msg::Finished {
        report: Box::new(ScanReport {
            integrations: Vec::new(),
            ..report_with_advice()
        }),
        elapsed: std::time::Duration::from_secs(1),
    });
    let mut first = Onboarding::new(&app);
    answer(&mut first, &[Key::Tab, Key::Tab, Key::Tab]);

    let text = screen(&first, &app, 80, 24);
    assert!(text.contains("knows no retention setting"), "{text}");
}

/// Whether the root search went through the whole home folder.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Search {
    Complete,
    Stopped,
}

/// The search ending with `paths` suggested, each holding one project, as the loop
/// passes it to both the questions and the home screen.
fn suggest(app: &mut App, first: &mut Onboarding, paths: &[PathBuf], search: Search) {
    let found = Found {
        suggestions: paths
            .iter()
            .map(|path| Suggestion {
                path: path.clone(),
                projects: vec![path.join("app")],
            })
            .collect(),
        complete: search == Search::Complete,
        unreadable: 0,
    };
    first.suggested(&found);
    app.update(Msg::Suggested(Box::new(found)));
}

// Catches a suggestion arriving checked, so finishing step 2 as it stands writes roots
// the user never chose and freezes the defaults.
#[test]
fn suggestions_start_unchecked_and_finishing_without_one_writes_as_before() {
    let disk = Disk::new();
    let (mut app, mut first) = disk.first_run();
    app.start_search();
    let documents = disk.home().join("Documents");
    suggest(&mut app, &mut first, &[documents], Search::Complete);

    assert!(
        first.offers.iter().all(|offer| !offer.checked),
        "every suggestion starts unchecked"
    );
    let edits = finished(answer(
        &mut first,
        &[Key::Enter, Key::Enter, Key::Tab, Key::Tab],
    ));
    assert_eq!(edits, [Edit::Level(Level::Balanced)]);
    assert_eq!(disk.finish(&mut app, edits), []);
    assert_eq!(
        config::load(&disk.location(), &disk.catalog)
            .unwrap()
            .config
            .project_roots,
        None
    );
}

// Catches a checked suggestion being lost, written twice when also typed, or written
// some other way than with the other roots in the one batch finishing writes.
#[test]
fn a_checked_suggestion_is_written_once_with_the_other_roots() {
    let disk = Disk::new();
    let (mut app, mut first) = disk.first_run();
    let documents = disk.home().join("Documents");
    suggest(
        &mut app,
        &mut first,
        std::slice::from_ref(&documents),
        Search::Complete,
    );
    answer(&mut first, &[Key::Enter]);

    // projects, code, "add another", then the suggestion.
    answer(
        &mut first,
        &[Key::Down, Key::Down, Key::Down, Key::Char(' ')],
    );
    assert!(first.offers[0].checked, "space checks the first suggestion");
    // Typing it as well is refused: it is in the list.
    answer(&mut first, &[Key::Up, Key::Enter]);
    for c in documents.display().to_string().chars() {
        first.key(Key::Char(c));
    }
    answer(&mut first, &[Key::Enter]);
    let notice = first.notice.as_deref().unwrap();
    assert!(notice.contains("is already in the list"), "{notice}");

    let edits = finished(answer(
        &mut first,
        &[Key::Down, Key::Enter, Key::Tab, Key::Tab],
    ));
    let roots = vec![
        disk.home().join("projects"),
        disk.home().join("code"),
        documents,
    ];
    assert_eq!(
        edits,
        [Edit::Level(Level::Balanced), Edit::ProjectRoots(roots)]
    );
    assert_eq!(disk.finish(&mut app, edits), [Effect::Rescan]);
    let written = config::load(&disk.location(), &disk.catalog).unwrap();
    let written = written.config.project_roots.unwrap();
    assert_eq!(written.len(), 3, "{written:?}");
    assert!(written[2].ends_with("Documents"), "{written:?}");
}

// Catches a checked suggestion inside a root typed in the same step being written as a
// root nested in it (03 §6, D-053: a path inside a root is never suggested).
#[test]
fn a_checked_suggestion_inside_a_typed_root_is_not_written() {
    let disk = Disk::new();
    let (mut app, mut first) = disk.first_run();
    let uni = disk.home().join("Documents").join("uni");
    suggest(
        &mut app,
        &mut first,
        std::slice::from_ref(&uni),
        Search::Complete,
    );
    answer(&mut first, &[Key::Enter]);

    // projects, code, "add another", then the suggestion.
    answer(
        &mut first,
        &[Key::Down, Key::Down, Key::Down, Key::Char(' ')],
    );
    assert!(first.offers[0].checked, "space checks the first suggestion");
    answer(&mut first, &[Key::Up, Key::Enter]);
    for c in "~/Documents".chars() {
        first.key(Key::Char(c));
    }

    let edits = finished(answer(
        &mut first,
        &[Key::Enter, Key::Enter, Key::Tab, Key::Tab],
    ));
    let roots = vec![
        disk.home().join("projects"),
        disk.home().join("code"),
        PathBuf::from("~/Documents"),
    ];
    assert_eq!(
        edits,
        [Edit::Level(Level::Balanced), Edit::ProjectRoots(roots)]
    );
}

// Catches quitting after checking a suggestion writing it anyway.
#[test]
fn quitting_with_a_checked_suggestion_writes_nothing() {
    let disk = Disk::new();
    let (mut app, mut first) = disk.first_run();
    suggest(
        &mut app,
        &mut first,
        &[disk.home().join("Documents")],
        Search::Complete,
    );
    answer(
        &mut first,
        &[Key::Enter, Key::Down, Key::Down, Key::Down, Key::Char(' ')],
    );

    assert_eq!(answer(&mut first, &[Key::Char('q')]), Outcome::Quit);
    assert!(
        !disk.file().exists(),
        "quitting with a checked suggestion writes no config"
    );
}

#[test]
fn step_2_with_suggestions_at_80_by_24() {
    let mut app = home();
    app.home = Some(PathBuf::from("/home/ada"));
    app.roots = vec![PathBuf::from("/home/ada/projects")];
    let mut first = Onboarding::new(&app);
    app.start_search();
    let running = {
        answer(&mut first, &[Key::Enter]);
        screen(&first, &app, 80, 24)
    };
    assert!(running.contains("looking for projects…"), "{running}");
    let paths = [
        PathBuf::from("/home/ada/Documents/uni"),
        PathBuf::from("/home/ada/notes"),
    ];
    suggest(&mut app, &mut first, &paths, Search::Stopped);
    answer(&mut first, &[Key::Down, Key::Down, Key::Char(' ')]);
    let shown = screen(&first, &app, 80, 24);
    // The roots and the suggestions read from `~` (F1).
    assert!(!shown.contains("/home/ada"), "{shown}");
    insta::assert_snapshot!(shown);
}

#[test]
fn step_2_with_suggestions_at_160_by_48() {
    let mut app = home();
    app.home = Some(PathBuf::from("/home/ada"));
    app.roots = vec![PathBuf::from("/home/ada/projects")];
    let mut first = Onboarding::new(&app);
    app.start_search();
    let paths = [
        PathBuf::from("/home/ada/Documents/uni"),
        PathBuf::from("/home/ada/notes"),
    ];
    suggest(&mut app, &mut first, &paths, Search::Complete);
    answer(&mut first, &[Key::Enter, Key::Down, Key::Down]);
    insta::assert_snapshot!(screen(&first, &app, 160, 48));
}
