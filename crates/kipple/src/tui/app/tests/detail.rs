//! The detail pane's scrolling and `y` (`docs/03-functional-spec.md` §5 Detail pane and
//! Keys, D-054).

use kipple_core::FindingId;

use super::{colour, focus, press, screen, sized, to};
use crate::tui::app::{App, Effect, Key, Node, Shelf};

/// The detail pane's lines on an 80×24 screen, where it sits below the tree: between
/// its top border and the two footer lines.
fn pane(app: &App) -> Vec<String> {
    let shown = screen(app, colour(), 80, 24);
    let lines: Vec<&str> = shown.lines().collect();
    let top = lines
        .iter()
        .position(|line| line.starts_with('─'))
        .unwrap_or_else(|| panic!("no detail pane in {shown}"));
    lines
        .get(top + 1..lines.len() - 2)
        .unwrap_or_default()
        .iter()
        .map(|&line| line.to_owned())
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
    assert!(says_more(&pane(&app)));

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

// Catches `J` counting lines that aren't there when everything fits, so `K` would
// have to be pressed for nothing before the pane moves again.
#[test]
fn j_does_nothing_when_the_detail_fits() {
    let mut app = sized(160, 48);
    press(&mut app, &[Key::Char('g')]);

    press(&mut app, &[Key::Char('J'); 3]);

    assert_eq!(app.detail_scroll, 0);
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
    assert_eq!(focus(&app), Some(Node::Shelf(Shelf::Safe)));

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
