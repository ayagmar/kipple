//! The detail pane's scrolling and `y` (`docs/03-functional-spec.md` §5 Detail pane and
//! Keys, D-054).

use std::path::PathBuf;
use std::sync::Arc;

use kipple_core::{Advice, FindingId, IntegrationReport, ScanOutcome, ScanReport};

use super::{
    CARGO, PACMAN, colour, focus, found, home, measured, press, recent, safe, screen, sized, to,
    unknown_layout,
};
use crate::tui::app::{App, Effect, Key, Msg, Node, Saved, Shelf};

/// The detail pane's lines on an 80×24 screen, where it sits below the tree: between
/// the rule above it and the key bar's, inside the frame.
fn pane(app: &App) -> Vec<String> {
    let shown = screen(app, colour(), 80, 24);
    let lines: Vec<&str> = shown.lines().collect();
    let rules: Vec<usize> = (0..lines.len())
        .filter(|&at| lines.get(at).is_some_and(|line| line.starts_with('├')))
        .collect();
    let (Some(&top), Some(&bottom)) = (rules.get(1), rules.get(2)) else {
        panic!("no detail pane in {shown}");
    };
    lines
        .get(top + 1..bottom)
        .unwrap_or_default()
        .iter()
        .map(|&line| {
            let inside = line.trim_start_matches('│').trim_end_matches('│');
            inside
                .strip_prefix(' ')
                .unwrap_or(inside)
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn says_more(pane: &[String]) -> bool {
    pane.last().is_some_and(|line| line.starts_with("more"))
}

// Catches a finding's detail cut off at 80×24 with no sign that more follows and no way
// to read the rest, and `J` scrolling past the end into an empty pane.
#[test]
fn the_detail_pane_scrolls_with_j_and_k_and_its_last_line_says_more_follows() {
    let mut app = sized(80, 24);
    let top = pane(&app);
    assert!(says_more(&top), "{top:#?}");

    press(&mut app, &[Key::Char('J')]);
    let down = pane(&app);
    assert_eq!(down.first(), top.get(1), "{down:#?}");

    press(&mut app, &[Key::Char('J'); 40]);
    let end = pane(&app);
    assert!(!says_more(&end), "{end:#?}");
    assert!(end.iter().any(|line| line.starts_with("Rule ")), "{end:#?}");
    press(&mut app, &[Key::Char('K')]);
    assert!(
        says_more(&pane(&app)),
        "the last line says more follows again after scrolling up"
    );

    press(&mut app, &[Key::Char('K'); 40]);
    assert_eq!(pane(&app), top);
}

// Catches the scroll carried over to the next line, which would open another finding's
// detail somewhere in its middle, with its title out of sight.
#[test]
fn moving_the_cursor_brings_the_detail_pane_back_to_its_top() {
    let mut app = sized(80, 24);
    let top = pane(&app);

    press(
        &mut app,
        &[Key::Char('J'), Key::Char('J'), Key::Down, Key::Up],
    );

    assert_eq!(pane(&app), top);
}

/// What the pane shows now, and whether it is its top: `K` all the way up changes nothing.
fn at_top(app: &mut App) -> bool {
    let shown = pane(app);
    press(app, &[Key::Char('K'); 40]);
    pane(app) == shown
}

// Catches the scroll kept when a scan event takes the focused project line away and the
// pane shows the line now in its place, which then opens in its middle (03 §5: another
// line starts at the top).
#[test]
fn the_line_that_takes_a_vanished_lines_place_shows_its_detail_from_the_top() {
    let mut app = sized(80, 24);
    found(
        &mut app,
        6,
        CARGO,
        "builtin.cargo.target",
        "/home/ada/projects/api/cli/target",
    );
    measured(&mut app, 6, 10, safe());
    // Findings of api in other buckets lengthen its detail, so it scrolls.
    found(
        &mut app,
        7,
        CARGO,
        "builtin.cargo.target",
        "/home/ada/projects/api/old/target",
    );
    measured(&mut app, 7, 10, recent());
    found(
        &mut app,
        8,
        CARGO,
        "builtin.cargo.target",
        "/home/ada/projects/api/odd/target",
    );
    measured(&mut app, 8, 10, unknown_layout());
    let api = app
        .lines()
        .iter()
        .map(|line| line.node)
        .find(|node| matches!(node, Node::Project(Shelf::SAFE, ..)))
        .unwrap();
    for _ in 0..app.lines().len() {
        if focus(&app) == Some(api) {
            break;
        }
        press(&mut app, &[Key::Up]);
    }
    assert_eq!(focus(&app), Some(api));
    press(&mut app, &[Key::Char('J'); 3]);
    assert_ne!(
        app.detail_first(api),
        0,
        "the project's detail doesn't scroll"
    );

    // Now recent, it leaves Safe, and api has one finding left there.
    measured(&mut app, 6, 10, recent());

    assert!(
        matches!(focus(&app), Some(Node::Finding(_))),
        "{:?}",
        focus(&app)
    );
    assert!(at_top(&mut app), "the detail shows from the top");
}

// Catches the scroll kept across a scan started again, so the first line focused in the
// new scan opens in its middle, its title and path out of sight.
#[test]
fn a_line_of_a_new_scan_shows_its_detail_from_the_top() {
    let mut app = sized(80, 24);
    press(&mut app, &[Key::Char('J'); 3]);

    let saved = Saved {
        file: None,
        config: app.config.clone(),
        policy: app.policy.clone(),
        roots: vec![PathBuf::from("~/work")],
        home: app.home.clone(),
    };
    app.update(Msg::Saved(Box::new(saved)));
    assert_eq!(app.take_effect(), Some(Effect::Rescan));
    found(
        &mut app,
        1,
        CARGO,
        "builtin.cargo.target",
        "/home/ada/work/api/target",
    );
    // Protected, so it is the last line and one key reaches it.
    measured(&mut app, 1, 10, unknown_layout());
    press(&mut app, &[Key::Char('G')]);

    assert_eq!(focus(&app), Some(Node::Finding(FindingId(1))));
    assert!(at_top(&mut app), "the detail shows from the top");
}

// Catches `J` counting lines that aren't there when everything fits, so `K` would
// have to be pressed for nothing before the pane moves again.
#[test]
fn j_does_nothing_when_the_detail_fits() {
    let mut app = sized(160, 48);
    press(&mut app, &[Key::Char('g')]);

    press(&mut app, &[Key::Char('J'); 3]);

    let node = focus(&app).unwrap();
    assert_eq!(app.detail_first(node), 0);
}

// Catches `y` sending another line's path, a path changed on the way (escaped, made
// relative, or with `~`), or nothing at all.
#[test]
fn y_on_a_finding_asks_the_loop_to_send_exactly_its_path() {
    let mut app = sized(80, 24);
    to(&mut app, 4);
    let path = app.row(FindingId(4)).unwrap().finding.path.clone();

    press(&mut app, &[Key::Char('y')]);

    assert_eq!(
        app.take_effect(),
        Some(Effect::Clipboard(path.to_str().unwrap().to_owned()))
    );
    assert_eq!(
        app.notice.as_deref(),
        Some("Path sent to the terminal clipboard.")
    );
}

// Catches `y` on a heading or gap sending some path anyway, such as the first finding
// below it.
#[test]
fn y_on_a_heading_does_nothing() {
    let mut app = sized(80, 24);
    press(&mut app, &[Key::Char('g')]);
    assert_eq!(focus(&app), Some(Node::Shelf(Shelf::SAFE)));

    press(&mut app, &[Key::Char('y')]);

    assert_eq!(app.take_effect(), None);
    assert_eq!(app.notice, None);
}

// Catches a path that isn't UTF-8 sent lossily, so the clipboard holds a path that
// doesn't exist, without a word about it.
#[cfg(unix)]
#[test]
fn y_refuses_a_path_that_isnt_utf8_with_a_message() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    use kipple_core::{Category, Finding, RuleId, ScanEvent};

    use super::CARGO;
    use crate::tui::app::Msg;

    let mut app = sized(80, 24);
    let path = std::path::Path::new(OsStr::from_bytes(b"/home/ada/caf\xe9/target"));
    app.update(Msg::Scan(Box::new(ScanEvent::FindingAdded {
        finding: Finding {
            id: FindingId(9),
            integration: CARGO,
            rule: RuleId("builtin.cargo.target".to_owned()),
            category: Category::BuildArtifact,
            path: path.to_owned(),
            evidence: Vec::new(),
            size: None,
            facts: None,
        },
    })));
    to(&mut app, 9);

    press(&mut app, &[Key::Char('y')]);

    assert_eq!(app.take_effect(), None);
    assert!(
        app.notice
            .as_deref()
            .is_some_and(|notice| notice.contains("isn't valid UTF-8")),
        "{:?}",
        app.notice
    );
}

/// The detail pane's lines at 160×48, where it sits right of the tree.
fn beside(app: &App) -> Vec<String> {
    screen(app, colour(), 160, 48)
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('│').collect();
            // The frame's two sides and the line between the tree and the pane.
            (parts.len() == 4).then(|| parts.get(2).map(|pane| (*pane).to_owned()))?
        })
        .collect()
}

// Catches a hold shown in the detail pane as a terse code (`unknown: layout`) instead of
// the sentence that says what it means (F4).
#[test]
fn the_detail_pane_says_why_a_finding_is_held_in_sentences() {
    let mut app = home();
    to(&mut app, 3);

    let words = beside(&app).join(" ");
    let words: Vec<&str> = words.split_whitespace().collect();

    let held = "kipple doesn't recognise what is inside, so it can't tell what is safe to \
                remove. kipple can't tell what removing it would lose or how it would come \
                back.";
    assert!(words.join(" ").contains(held), "{words:?}");
}

// Catches a snippet that spans lines drawn as one line with a literal `\n` (F2).
#[test]
fn the_detail_pane_shows_each_line_of_an_advice_snippet_on_its_own_line() {
    let mut app = home();
    let report = ScanReport {
        outcome: ScanOutcome::Completed,
        integrations: vec![IntegrationReport {
            id: PACMAN,
            omissions: Vec::new(),
            opened: Vec::new(),
            advice: vec![Advice {
                title: "Keep less in the journal".to_owned(),
                text: "journald keeps logs up to a size.".to_owned(),
                snippets: vec!["[Journal]\nSystemMaxUse=200M".to_owned()],
                references: Vec::new(),
            }],
        }],
        findings: Vec::new(),
        unchecked_system: 0,
        by_command_line: 0,
    };
    app.update(Msg::Finished {
        report: Arc::new(report),
        elapsed: std::time::Duration::from_secs(1),
    });
    to(&mut app, 5);

    let pane = beside(&app);
    assert!(!pane.iter().any(|line| line.contains("\\n")), "{pane:?}");
    for wanted in ["[Journal]", "SystemMaxUse=200M"] {
        assert!(
            pane.iter().any(|line| line.trim() == wanted),
            "{wanted}: {pane:?}"
        );
    }
}
