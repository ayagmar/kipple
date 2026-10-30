//! Rebinding keys with `[keys]` (`docs/03-functional-spec.md` §5 Keys and §6, D-056):
//! a rebound key does its action where that action is read and nowhere else, help and
//! the key bar name it, typed text is never rebound, and Ctrl-C and esc keep their
//! meaning. Each config is read from a file in a temp dir by `config::load`.

use std::fs;

use super::{Paint, focus, home, look, press, screen, selected, to, typed};
use crate::config::{self, Config, Location, Source};
use crate::theme::Glyphs;
use crate::tui::app::{App, Key, Mode};
use crate::tui::keymap::{self, Action};

/// What a config file holding `text` says.
fn keys(text: &str) -> Config {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    fs::write(&path, text).unwrap();
    let location = Location {
        source: Source::Flag,
        path: Some(path),
    };
    let catalog = super::catalog().rules.into_keys().collect();
    config::load(&location, &catalog).unwrap().config
}

/// [`home`] with the keys `text` gives.
fn home_with(text: &str) -> App {
    let mut app = home();
    app.config = keys(text);
    app
}

fn help(app: &mut App) -> String {
    press(app, &[Key::Char('?')]);
    let text = screen(app, look(Paint::Mono, Glyphs::Unicode), 160, 48);
    press(app, &[Key::Esc]);
    text
}

// Catches a binding read but not acted on, a default kept beside its replacement, or
// help and the key bar still naming the old key.
#[test]
fn a_rebound_key_does_its_action_and_help_and_the_key_bar_name_it() {
    let mut app = home_with("[keys]\nselect = [\"x\"]\nfirst = [\"home\"]\n");
    to(&mut app, 1);

    press(&mut app, &[Key::Char('x')]);
    assert_eq!(selected(&app), [1]);
    press(&mut app, &[Key::Char('s')]);
    assert_eq!(selected(&app), [1], "s no longer selects");

    let bar = screen(&app, look(Paint::Mono, Glyphs::Unicode), 160, 48);
    assert!(bar.contains("x select"), "{bar}");
    let help = help(&mut app);
    assert!(
        help.contains("x          select or unselect the finding"),
        "{help}"
    );
    assert!(!help.contains("s          select"), "{help}");
    // The keyboard's own home key is listed once it is all `first` has.
    assert!(help.contains("home G     first and last line"), "{help}");
}

// Catches a key taken in one mode being taken in every mode, or the action it was taken
// from losing its other keys.
#[test]
fn a_key_taken_from_another_action_leaves_it_only_in_that_mode() {
    let mut app = home_with("[keys]\nselect = [\"j\"]\n");
    to(&mut app, 1);

    press(&mut app, &[Key::Char('j')]);
    assert_eq!(selected(&app), [1]);
    assert_eq!(
        focus(&app),
        Some(super::Node::Finding(kipple_core::FindingId(1)))
    );
    let before = focus(&app);
    press(&mut app, &[Key::Down]);
    assert_ne!(focus(&app), before, "down still moves");
    let bar = screen(&app, look(Paint::Mono, Glyphs::Unicode), 160, 48);
    assert!(bar.contains("↓/k move"), "{bar}");

    press(&mut app, &[Key::Char(','), Key::Char('j')]);
    assert_eq!(app.mode, Mode::Settings);
    assert_eq!(app.panel.cursor, 1, "j still moves in settings");
}

// Catches a binding that takes away the way out: Ctrl-C quits from anywhere and esc
// closes settings, whatever quit and settings are bound to.
#[test]
fn ctrl_c_and_esc_keep_their_meaning_whatever_the_keys_say() {
    let mut app = home_with("[keys]\nquit = [\"Q\"]\nsettings = [\"S\"]\n");

    press(&mut app, &[Key::Char('S')]);
    assert_eq!(app.mode, Mode::Settings);
    press(&mut app, &[Key::Char('q'), Key::Char(',')]);
    assert_eq!(app.mode, Mode::Settings, "q and , no longer close settings");
    press(&mut app, &[Key::Esc]);
    assert_eq!(app.mode, Mode::Browse);

    press(&mut app, &[Key::Char('q')]);
    assert!(!app.quit, "q no longer quits once it is rebound");
    press(&mut app, &[Key::Interrupt]);
    assert!(app.quit, "ctrl-c still quits");
}

// Catches a rebound character moving the cursor instead of typing in the filter.
#[test]
fn in_the_filter_a_rebound_character_types_and_only_keys_that_type_nothing_move() {
    let mut app = home_with("[keys]\nmove-down = [\"n\", \"pagedown\"]\n");
    press(&mut app, &[Key::Char('/')]);
    typed(&mut app, "node");
    assert_eq!(app.filter, "node");

    let at = focus(&app);
    press(&mut app, &[Key::Down]);
    assert_eq!(focus(&app), at, "down no longer moves");
    press(&mut app, &[Key::PageDown]);
    assert_ne!(focus(&app), at, "pagedown moves");
}

// Catches two actions read in different modes being refused one key, which would make
// a key on the first run cost a key on the tree.
#[test]
fn one_key_can_do_one_thing_in_each_mode() {
    let config = keys("[keys]\nselect = [\"x\"]\ncheck = [\"x\"]\n");
    let keymap = config.keys.keymap();

    let x = Key::Char('x');
    assert_eq!(keymap.action(keymap::Mode::Browse, x), Some(Action::Select));
    assert_eq!(
        keymap.action(keymap::Mode::Onboarding, x),
        Some(Action::Check)
    );
    assert_eq!(
        keymap.action(keymap::Mode::Onboarding, Key::Char(' ')),
        None
    );
}

// Catches copy that names a default key after it was rebound, or names a key an action
// no longer has: the detail pane's scroll hint, the Safe bucket's "selects all of it"
// and the settings lines that say how to keep.
#[test]
fn the_screens_words_name_the_keys_as_bound_or_none() {
    let mut app = home_with(
        "[keys]\ndetail-down = [\"n\"]\ndetail-up = [\"m\"]\nselect-safe = [\"x\"]\nkeep = [\"z\"]\nkeep-rule = []\n",
    );
    app.update(crate::tui::app::Msg::Resize {
        width: 80,
        height: 24,
    });
    let bucket = screen(&app, look(Paint::Mono, Glyphs::Unicode), 80, 24);
    assert!(bucket.contains("x selects all of it."), "{bucket}");
    assert!(!bucket.contains("c selects"), "{bucket}");

    to(&mut app, 1);
    let finding = screen(&app, look(Paint::Mono, Glyphs::Unicode), 80, 24);
    assert!(finding.contains("n/m scroll"), "{finding}");
    assert!(!finding.contains("J/K"), "{finding}");

    press(&mut app, &[Key::Char(',')]);
    let settings = screen(&app, look(Paint::Mono, Glyphs::Unicode), 160, 48);
    assert!(
        settings.contains("nothing yet: z keeps a finding"),
        "{settings}"
    );
    let off = settings
        .lines()
        .find(|line| line.contains("Rules turned off"))
        .unwrap();
    assert!(
        off.trim_end_matches([' ', '│']).ends_with("none"),
        "{settings}"
    );
}
