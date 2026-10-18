//! The first run (`docs/03-functional-spec.md` §2 J0): its steps, what finishing writes
//! to a config file in a temp dir through the same `config::write` the loop calls, the
//! rescan a change of roots asks for, and the screens at 80×24 and 160×48.

use std::fs;
use std::path::PathBuf;

use kipple_core::{
    Advice, IntegrationReport, Level, Method, ResolvedRoot, RootSource, Roots, ScanOutcome,
    ScanReport, SymbolicRoot,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::{PACMAN, colour, home};
use crate::config::{self, Catalog, Config, Edit, Loaded, Location, Settings, Source};
use crate::tui::app::{App, Effect, Key, Msg, Saved};
use crate::tui::onboarding::{self, Onboarding, Outcome, Step};

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

    fn settings(&self, exists: bool, config: Config) -> Settings {
        let (policy, warnings) = config.policy(&self.roots);
        Settings {
            loaded: Loaded {
                location: self.location(),
                exists,
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
        app.configure(Saved::of(&self.settings(false, Config::default())));
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
                Ok(config) => Msg::Saved(Box::new(Saved::of(&self.settings(true, config)))),
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
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| onboarding::render(onboarding, app, colour(), frame))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..height {
        let line: String = (0..width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
            .collect();
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

fn report_with_advice() -> ScanReport {
    ScanReport {
        outcome: ScanOutcome::Completed,
        integrations: vec![IntegrationReport {
            id: PACMAN,
            omissions: Vec::new(),
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
    assert!(written.exists);
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
    assert!(first.typing.is_some());
    assert_eq!(first.key(Key::Char('q')), Outcome::Stay, "q is typed");
    assert_eq!(first.key(Key::Interrupt), Outcome::Quit);
    assert!(!disk.file().exists());
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
    assert!(first.notice.is_some());
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

    assert!(onboarding::due(&loaded(
        Source::Default,
        Some(disk.file()),
        false
    )));
    assert!(!onboarding::due(&loaded(
        Source::Default,
        Some(disk.file()),
        true
    )));
    assert!(!onboarding::due(&loaded(Source::Default, None, false)));
    for named in [Source::Flag, Source::Env] {
        assert!(!onboarding::due(&loaded(named, Some(disk.file()), true)));
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
