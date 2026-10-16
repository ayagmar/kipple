//! Keeping (`p`, `P`) and the settings panel (`docs/03-functional-spec.md` §2 J2, §5):
//! every change is written to an injected config file in a temp dir by the same
//! `config::write` the loop calls, and the screen changes only from what was written.

use std::fs;
use std::path::PathBuf;

use kipple_core::{
    Caution, Hold, Level, Loss, Method, Modified, ResolvedRoot, RootSource, Roots, RuleId,
    RuleSettings, SymbolicRoot, Verdict,
};

use ratatui::style::Color;

use super::{
    CARGO, DAY, NODE, colour, draw, found, home, measured, press, safe, screen, selected, to, typed,
};
use crate::config::{self, Catalog, Config, Loaded, Location, Settings, Source};
use crate::tui::app::{App, Effect, Key, Mode, Msg, Saved, Shelf};
use crate::tui::view::Look;

const CARGO_RULE: &str = "builtin.cargo.target";
const NODE_RULE: &str = "builtin.node.node-modules";

/// A temp dir with a home directory and the config file the screen is told to use.
struct Disk {
    temp: tempfile::TempDir,
    roots: Roots,
    catalog: Catalog,
}

impl Disk {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(home.join("projects")).unwrap();
        let mut roots = Roots::default();
        roots.insert(
            SymbolicRoot::Home,
            Ok(ResolvedRoot::new(
                SymbolicRoot::Home,
                home,
                RootSource::Env("HOME"),
            )),
        );
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

    /// The config file, which doesn't exist until the first write.
    fn file(&self) -> PathBuf {
        self.temp.path().join("config").join("config.toml")
    }

    fn location(&self) -> Location {
        Location {
            source: Source::Flag,
            path: Some(self.file()),
        }
    }

    fn text(&self) -> String {
        fs::read_to_string(self.file()).unwrap()
    }

    /// What `config` gives over these roots, as the loop has it after a write.
    fn saved(&self, config: Config) -> Saved {
        let (policy, warnings) = config.policy(&self.roots);
        let settings = Settings {
            loaded: Loaded {
                location: self.location(),
                exists: true,
                config,
            },
            roots: self.roots.clone(),
            policy,
            warnings,
        };
        Saved::of(&settings)
    }

    /// Does what the loop does with each effect the screen asks for: writes the config
    /// and answers. The effects it doesn't carry out, in order.
    fn run(&self, app: &mut App) -> Vec<Effect> {
        let mut left = Vec::new();
        while let Some(effect) = app.take_effect() {
            let Effect::Write(edits) = effect else {
                left.push(effect);
                continue;
            };
            let written = config::write(&self.location(), &edits, &self.roots, &self.catalog);
            app.update(match written {
                Ok(config) => Msg::Saved(Box::new(self.saved(config))),
                Err(error) => Msg::NotSaved(error.to_string()),
            });
        }
        left
    }

    /// Every file below the temp dir, relative to it.
    fn files(&self) -> Vec<PathBuf> {
        let base = self.temp.path();
        let mut found = Vec::new();
        let mut dirs = vec![base.to_owned()];
        while let Some(dir) = dirs.pop() {
            let entries = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path());
            let (more, files): (Vec<_>, Vec<_>) = entries.partition(|path| path.is_dir());
            dirs.extend(more);
            found.extend(
                files
                    .iter()
                    .map(|f| f.strip_prefix(base).unwrap().to_owned()),
            );
        }
        found.sort();
        found
    }
}

/// A home screen over `disk` with the config `config` and two projects' findings in
/// the home directory: 1 a Cargo `target`, 2 a `node_modules`, both Safe at Balanced.
fn screen_on(disk: &Disk, config: Config) -> App {
    let mut app = App::new(super::catalog(), 160, 48);
    app.configure(disk.saved(config));
    let projects = disk.home().join("projects");
    let api = projects.join("api").join("target");
    let web = projects.join("web").join("node_modules");
    found(&mut app, 1, CARGO, CARGO_RULE, api.to_str().unwrap());
    found(&mut app, 2, NODE, NODE_RULE, web.to_str().unwrap());
    measured(&mut app, 1, 512, safe());
    measured(&mut app, 2, 300, safe());
    app
}

fn shelf(app: &App, id: u64) -> Shelf {
    app.row(kipple_core::FindingId(id)).unwrap().shelf()
}

fn verdict(app: &App, id: u64) -> Verdict {
    let row = app.row(kipple_core::FindingId(id)).unwrap();
    row.verdict.clone().unwrap()
}

/// Opens settings, unless they are open, and moves to the `nth` line the cursor can
/// stand on.
fn settings_at(app: &mut App, nth: usize) {
    if app.mode != Mode::Settings {
        press(app, &[Key::Char(',')]);
    }
    assert_eq!(app.mode, Mode::Settings);
    press(app, &[Key::Char('g')]);
    press(app, &vec![Key::Down; nth]);
}

// Catches the screen showing a finding as kept before the config holds it, the note
// being dropped, the kept finding staying selected, or the write landing anywhere but
// the config file the screen was given.
#[test]
fn p_keeps_the_focused_finding_with_its_note_once_the_config_file_says_so() {
    let disk = Disk::new();
    let mut app = screen_on(&disk, Config::default());
    to(&mut app, 1);
    press(&mut app, &[Key::Char('s'), Key::Char('p')]);
    typed(&mut app, "baseline");
    press(&mut app, &[Key::Enter]);

    assert!(app.selected.is_empty(), "a kept finding stayed selected");
    assert_eq!(shelf(&app, 1), Shelf::Safe, "kept before it was written");
    assert_eq!(disk.run(&mut app), []);

    assert_eq!(verdict(&app, 1), Verdict::Protected(vec![Hold::Kept]));
    assert_eq!(shelf(&app, 2), Shelf::Safe);
    let text = disk.text();
    assert!(text.contains("note = \"baseline\""), "{text}");
    assert!(
        text.contains('~'),
        "a path in the home isn't written with ~: {text}"
    );
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.starts_with("Kept "), "{notice}");
    assert_eq!(disk.files(), [PathBuf::from("config").join("config.toml")]);
}

// Catches a failed write being shown as kept, or changing the file.
#[test]
fn a_keep_the_file_refuses_shows_why_and_changes_nothing() {
    let disk = Disk::new();
    let mut app = screen_on(&disk, Config::default());
    // Someone is editing it, and it doesn't read as a config right now.
    let broken = "[care]\nlevel = \"reckless\"\n";
    fs::create_dir_all(disk.file().parent().unwrap()).unwrap();
    fs::write(disk.file(), broken).unwrap();
    to(&mut app, 1);
    press(&mut app, &[Key::Char('p'), Key::Enter]);
    disk.run(&mut app);

    assert_eq!(shelf(&app, 1), Shelf::Safe);
    assert_eq!(app.policy.keep, []);
    assert_eq!(disk.text(), broken);
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.starts_with("Nothing was changed"), "{notice}");
    assert!(notice.contains("line 2, column 9"), "{notice}");
}

// Catches a path TOML can't hold being written lossily, so the config keeps a
// different path than the one shown.
#[cfg(unix)]
#[test]
fn a_finding_whose_path_isnt_utf8_is_refused_with_a_message_and_nothing_is_written() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let disk = Disk::new();
    let mut app = App::new(super::catalog(), 160, 48);
    app.configure(disk.saved(Config::default()));
    let name = OsStr::from_bytes(b"project-\xff");
    let path = disk.home().join("projects").join(name).join("target");
    app.update(Msg::Scan(Box::new(kipple_core::ScanEvent::FindingAdded {
        finding: kipple_core::Finding {
            id: kipple_core::FindingId(1),
            integration: CARGO,
            rule: RuleId(CARGO_RULE.to_owned()),
            category: kipple_core::Category::BuildArtifact,
            path,
            evidence: Vec::new(),
            size: None,
            facts: None,
        },
    })));
    measured(&mut app, 1, 512, safe());
    to(&mut app, 1);
    press(&mut app, &[Key::Char('p'), Key::Enter]);
    disk.run(&mut app);

    assert_eq!(shelf(&app, 1), Shelf::Safe);
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.contains("isn't valid UTF-8"), "{notice}");
    assert!(disk.files().is_empty(), "{:?}", disk.files());
}

// Catches `P` keeping only the focused finding, or leaving the rule's other findings
// selected.
#[test]
fn shift_p_turns_the_rule_off_which_keeps_and_unselects_every_finding_of_it() {
    let disk = Disk::new();
    let mut app = screen_on(&disk, Config::default());
    let other = disk.home().join("projects").join("cli").join("target");
    found(&mut app, 3, CARGO, CARGO_RULE, other.to_str().unwrap());
    measured(&mut app, 3, 64, safe());
    press(&mut app, &[Key::Char('c')]);
    assert_eq!(selected(&app), [1, 2, 3]);
    to(&mut app, 1);
    press(&mut app, &[Key::Char('P')]);

    assert_eq!(selected(&app), [2]);
    disk.run(&mut app);

    for id in [1, 3] {
        assert_eq!(verdict(&app, id), Verdict::Protected(vec![Hold::Disabled]));
    }
    assert_eq!(shelf(&app, 2), Shelf::Safe);
    let text = disk.text();
    assert!(text.contains("enabled = false"), "{text}");
}

// Catches an undo in settings that doesn't reach the config, or a screen that keeps
// judging with the old policy after it.
#[test]
fn settings_undo_a_kept_item_and_a_turned_off_rule() {
    let disk = Disk::new();
    let mut app = screen_on(&disk, Config::default());
    to(&mut app, 1);
    press(&mut app, &[Key::Char('p'), Key::Enter]);
    disk.run(&mut app);
    press(&mut app, &[Key::Char('g')]);
    to(&mut app, 2);
    press(&mut app, &[Key::Char('P')]);
    disk.run(&mut app);
    assert_eq!(shelf(&app, 1), Shelf::Protected);
    assert_eq!(shelf(&app, 2), Shelf::Protected);

    // No project roots: the level, add another, the method, then the kept item and the
    // rule.
    settings_at(&mut app, 3);
    press(&mut app, &[Key::Char('d')]);
    disk.run(&mut app);
    assert_eq!(shelf(&app, 1), Shelf::Safe);
    assert_eq!(app.config.keep, []);

    settings_at(&mut app, 3);
    press(&mut app, &[Key::Char('d')]);
    disk.run(&mut app);
    assert_eq!(shelf(&app, 2), Shelf::Safe);
    let text = disk.text();
    assert!(!text.contains("enabled"), "{text}");
    assert!(!text.contains("target"), "{text}");
}

// Catches a level switch that needs a rescan, lets findings join the selection, or
// drops ones the selection rules keep: a finding chosen by hand stays while eligible;
// one a shortcut added leaves once it is recent.
#[test]
fn a_level_switch_rebuckets_in_place_and_takes_out_only_what_the_selection_rules_say() {
    let disk = Disk::new();
    let mut app = screen_on(&disk, Config::default());
    // 10 days old: Safe at Balanced (7), recent at Careful (14).
    let tenth = kipple_core::Facts {
        modified: Modified::Ago(10 * DAY),
        ..safe()
    };
    measured(&mut app, 1, 512, tenth.clone());
    measured(&mut app, 2, 300, tenth);
    let old = disk.home().join("projects").join("cli").join("target");
    found(&mut app, 3, CARGO, CARGO_RULE, old.to_str().unwrap());
    measured(&mut app, 3, 64, safe());
    // Loses rollback ability: Review at Balanced and Careful, Safe at Thorough.
    let rollback = disk.home().join("projects").join("old").join("target");
    found(&mut app, 4, CARGO, CARGO_RULE, rollback.to_str().unwrap());
    let mut facts = safe();
    if let Some(consequence) = &mut facts.consequence {
        consequence.loss = &[Loss::RollbackAbility];
    }
    measured(&mut app, 4, 32, facts);
    to(&mut app, 1);
    press(&mut app, &[Key::Char('s'), Key::Char('c')]);
    assert_eq!(selected(&app), [1, 2, 3]);

    // From Balanced one step down to Careful.
    settings_at(&mut app, 0);
    press(&mut app, &[Key::Char('h')]);
    let effects = disk.run(&mut app);

    assert!(effects.is_empty(), "a level switch rescanned: {effects:?}");
    assert_eq!(app.policy.level, Level::Careful);
    assert_eq!(verdict(&app, 1), Verdict::Review(vec![Caution::Recent]));
    assert_eq!(selected(&app), [1, 3], "only the recent shortcut pick left");
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.contains("1 finding left the selection"), "{notice}");

    for _ in 0..2 {
        press(&mut app, &[Key::Char('l')]);
        disk.run(&mut app);
    }
    assert_eq!(app.policy.level, Level::Thorough);
    assert_eq!(shelf(&app, 4), Shelf::Safe);
    assert_eq!(selected(&app), [1, 3], "a finding joined the selection");
    assert!(disk.text().contains("level = \"thorough\""));
}

// Catches a root change that keeps the old scan's findings or selection, whose IDs mean
// nothing in the next scan.
#[test]
fn adding_or_removing_a_project_root_rescans_and_says_the_selection_was_cleared() {
    let disk = Disk::new();
    let config = Config {
        project_roots: Some(vec!["~/projects".to_owned()]),
        ..Config::default()
    };
    let mut app = screen_on(&disk, config);
    press(&mut app, &[Key::Char('c')]);

    // Level, ~/projects, then "add another".
    settings_at(&mut app, 2);
    press(&mut app, &[Key::Enter]);
    typed(&mut app, "~/work");
    press(&mut app, &[Key::Enter]);
    let effects = disk.run(&mut app);

    assert_eq!(effects, [Effect::Rescan]);
    assert!(app.rows.is_empty() && app.selected.is_empty());
    assert_eq!(
        app.roots,
        [PathBuf::from("~/projects"), PathBuf::from("~/work")]
    );
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.contains("Scanning again"), "{notice}");
    assert!(
        notice.contains("selection (2 findings) was cleared"),
        "{notice}"
    );

    settings_at(&mut app, 1);
    press(&mut app, &[Key::Char('d')]);
    assert_eq!(disk.run(&mut app), [Effect::Rescan]);
    assert_eq!(app.roots, [PathBuf::from("~/work")]);
}

// Catches removing the last root writing nothing, which leaves the defaults scanned
// while the panel lists none.
#[test]
fn removing_the_last_project_root_writes_an_empty_list_and_rescans() {
    let disk = Disk::new();
    let config = Config {
        project_roots: Some(vec!["~/projects".to_owned()]),
        ..Config::default()
    };
    let mut app = screen_on(&disk, config);
    press(&mut app, &[Key::Char('c')]);

    settings_at(&mut app, 1);
    press(&mut app, &[Key::Char('d')]);

    assert_eq!(disk.run(&mut app), [Effect::Rescan]);
    assert_eq!(app.roots, Vec::<PathBuf>::new());
    assert!(
        disk.text().contains("project_roots = []"),
        "{}",
        disk.text()
    );
}

// Catches the stored method not being written, or written as something else.
#[test]
fn the_default_method_is_stored() {
    let disk = Disk::new();
    let mut app = screen_on(&disk, Config::default());
    // No project roots: the level, add another, then the method.
    settings_at(&mut app, 2);
    press(&mut app, &[Key::Enter]);
    disk.run(&mut app);

    assert_eq!(app.config.method, Some(Method::Trash));
    assert!(disk.text().contains("method = \"trash\""));
}

// Catches esc in the note prompt keeping the finding anyway.
#[test]
fn esc_at_the_note_keeps_nothing_and_writes_nothing() {
    let disk = Disk::new();
    let mut app = screen_on(&disk, Config::default());
    to(&mut app, 1);
    press(&mut app, &[Key::Char('p')]);
    typed(&mut app, "never mind");
    press(&mut app, &[Key::Esc]);

    assert_eq!(disk.run(&mut app), []);
    assert_eq!(disk.files(), Vec::<PathBuf>::new());
    assert_eq!(shelf(&app, 1), Shelf::Safe);
}

/// The home of the main tests, with settings written in every section.
fn configured(width: u16, height: u16) -> App {
    let mut app = home();
    let config = Config {
        method: Some(Method::Trash),
        project_roots: Some(vec!["~/projects".to_owned(), "~/work".to_owned()]),
        keep: vec![(
            "~/projects/api/target".to_owned(),
            Some("benchmark baseline".to_owned()),
        )],
        ..Config::default()
    };
    let mut policy = app.policy.clone();
    policy.rules.insert(
        RuleId("builtin.pacman.cache".to_owned()),
        RuleSettings {
            disabled: true,
            ..RuleSettings::default()
        },
    );
    app.configure(Saved {
        file: Some(PathBuf::from("/home/ada/.config/kipple/config.toml")),
        config,
        policy,
        roots: vec![PathBuf::from("~/projects"), PathBuf::from("~/work")],
        home: Some(PathBuf::from("/home/ada")),
    });
    app.update(Msg::Resize { width, height });
    press(&mut app, &[Key::Char(','), Key::Down, Key::Down]);
    app
}

#[test]
fn settings_at_80_by_24() {
    let app = configured(80, 24);
    insta::assert_snapshot!(screen(&app, colour(), 80, 24));
}

#[test]
fn settings_at_160_by_48() {
    let app = configured(160, 48);
    insta::assert_snapshot!(screen(&app, colour(), 160, 48));
}

// Catches `a` adding a recent finding, which J2 keeps out of every group shortcut.
#[test]
fn a_leaves_recent_findings_out() {
    let mut app = home();
    // Review: finding 2 changed two days ago.
    let review = app
        .lines()
        .iter()
        .position(|line| line.node == super::Node::Shelf(Shelf::Review))
        .unwrap();
    press(&mut app, &vec![Key::Down; review]);
    press(&mut app, &[Key::Char('a')]);

    assert!(app.selected.is_empty(), "a recent finding was added");
    to(&mut app, 2);
    press(&mut app, &[Key::Char('s')]);
    assert_eq!(selected(&app), [2], "s still selects it by hand");
}

// Catches the settings panel, or the header while a scan runs, drawing a colour or a
// Unicode symbol under NO_COLOR and --ascii.
#[test]
fn settings_under_no_color_and_ascii_use_only_ascii_and_the_terminals_colours() {
    for (width, height) in [(80, 24), (160, 48)] {
        let app = configured(width, height);
        let plain = Look {
            color: false,
            ascii: true,
        };
        let buffer = draw(&app, plain, width, height);

        for cell in &buffer.content {
            assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset), "{cell:?}");
            assert!(cell.symbol().is_ascii(), "{cell:?}");
        }
    }
}

// Catches a root that is already listed being written again under another spelling,
// which would scan the same tree twice.
#[test]
fn a_project_root_already_listed_is_refused_however_it_is_spelled() {
    let disk = Disk::new();
    let config = Config {
        project_roots: Some(vec!["~/projects".to_owned()]),
        ..Config::default()
    };
    let mut app = screen_on(&disk, config);
    let spelled = format!(
        "{}{}",
        disk.home().join("PROJECTS").display(),
        std::path::MAIN_SEPARATOR
    );

    // Level, ~/projects, then "add another".
    settings_at(&mut app, 2);
    press(&mut app, &[Key::Enter]);
    typed(&mut app, &spelled);
    press(&mut app, &[Key::Enter]);

    assert_eq!(disk.run(&mut app), []);
    assert_eq!(app.roots, [PathBuf::from("~/projects")]);
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.contains("is already a project root"), "{notice}");
    assert_eq!(disk.files(), Vec::<PathBuf>::new());
}

// Catches a relative project root being written, which would be scanned from whatever
// directory kipple happens to start in.
#[test]
fn a_relative_project_root_is_refused_and_nothing_rescans() {
    let disk = Disk::new();
    let mut app = screen_on(&disk, Config::default());
    // No project roots: the level, then "add another".
    settings_at(&mut app, 1);
    press(&mut app, &[Key::Enter]);
    typed(&mut app, "work");
    press(&mut app, &[Key::Enter]);

    assert_eq!(disk.run(&mut app), []);
    assert_eq!(app.rows.len(), 2);
    assert_eq!(app.roots, Vec::<PathBuf>::new());
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.starts_with("Nothing was changed"), "{notice}");
    assert_eq!(disk.files(), Vec::<PathBuf>::new());
}
