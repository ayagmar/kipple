//! The dashboard (`docs/03-functional-spec.md` §5 Layout, D-057): the frame, the summary
//! band, the tree beside or above the detail pane, and the key bar, at every size and
//! as `[ui]` sets them out.

use std::path::PathBuf;

use kipple_core::{Omission, ProbeError, ScanEvent};

use super::{
    CARGO, colour, finished, found, home, look, measured, opened, press, recent, safe, screen,
    sized, typed, unknown_layout,
};
use crate::config::DetailPlace;
use crate::tui::app::{App, Key, Msg};

/// The sizes the dashboard is drawn at: the smallest it must work at, the breakpoint
/// of the detail pane beside the tree, and two larger.
const SIZES: [(u16, u16); 4] = [(80, 24), (120, 40), (160, 48), (200, 60)];

/// `app` with what the scan couldn't read.
fn partial(app: &mut App) {
    let error = ProbeError::io(
        &PathBuf::from("/home/ada/projects/secret"),
        std::io::ErrorKind::PermissionDenied.into(),
    );
    app.update(Msg::Scan(Box::new(ScanEvent::IntegrationCompleted {
        integration: CARGO,
        omissions: vec![Omission::Unlisted { error }],
    })));
}

#[test]
fn the_dashboard_at_every_size_in_unicode_and_ascii() {
    for (width, height) in SIZES {
        let app = sized(width, height);
        for (name, ascii) in [("unicode", false), ("ascii", true)] {
            insta::assert_snapshot!(
                format!("dashboard_{width}x{height}_{name}"),
                screen(&app, look(true, ascii), width, height)
            );
        }
    }
}

// Catches an empty bucket of a partial scan reading as nothing to clean, in the cards
// and in the one-line band, and the title line losing the partial signal.
#[test]
fn a_partial_scan_says_so_in_the_title_and_on_each_empty_card() {
    let mut app = App::new(super::catalog(), 120, 40);
    partial(&mut app);
    for (width, height) in [(120, 40), (80, 24)] {
        app.update(Msg::Resize { width, height });
        let shown = screen(&app, colour(), width, height);
        let title = shown.lines().next().unwrap();
        assert!(title.contains("partial: 1 not covered"), "{shown}");
        // Each card says both, on one line or two; the one-line band says it once.
        let band: String = shown.lines().skip(1).take(2).collect();
        let cards = if width >= 100 { 3 } else { 1 };
        assert_eq!(band.matches("none found").count(), 3, "{shown}");
        assert_eq!(band.matches("scan partial").count(), cards, "{shown}");
        assert!(
            !band.contains("SAFE 0 B") && !band.contains("Safe 0 B"),
            "{shown}"
        );
    }
}

// Catches findings still being measured missing from the band, which would make its
// totals look final while sizes are still coming in.
#[test]
fn while_findings_are_measured_the_band_shows_how_many() {
    let mut app = App::new(super::catalog(), 120, 40);
    found(
        &mut app,
        1,
        CARGO,
        "builtin.cargo.target",
        "/home/ada/p/target",
    );
    found(
        &mut app,
        2,
        CARGO,
        "builtin.cargo.target",
        "/home/ada/q/target",
    );

    let wide = screen(&app, colour(), 120, 40);
    assert!(
        wide.lines().nth(1).unwrap().contains("MEASURING 2"),
        "{wide}"
    );
    app.update(Msg::Resize {
        width: 80,
        height: 24,
    });
    let narrow = screen(&app, colour(), 80, 24);
    let band = narrow.lines().nth(1).unwrap();
    assert!(band.contains("Measuring 2"), "{narrow}");
    assert!(band.contains("Safe none yet"), "{narrow}");
}

// Catches a total that vanishes when the band is turned off or doesn't fit: found,
// eligible, selected and what it frees must always show somewhere.
#[test]
fn every_total_shows_whether_or_not_the_band_does() {
    for (width, height) in [(80, 24), (120, 40), (120, 20), (99, 30)] {
        for summary in [true, false] {
            let mut app = sized(width, height);
            app.config.ui.summary = summary;
            let shown = screen(&app, colour(), width, height);
            for total in [
                "found 1.7 GiB",
                "eligible 814.0 MiB",
                "selected 1",
                "frees 512.0 MiB",
            ] {
                assert!(
                    shown.contains(total),
                    "{total} at {width}x{height}: {shown}"
                );
            }
            let banded = shown.contains("Review 300.0 MiB") || shown.contains("REVIEW 300.0 MiB");
            assert_eq!(banded, summary && height >= 24, "{shown}");
        }
    }
}

// Catches `[ui] detail` and `bars` ignored, or a pane drawn where it was turned off.
#[test]
fn ui_options_place_the_detail_pane_and_turn_bars_off() {
    let wanted = "Path          ~/projects/api/target";
    let mut app = sized(160, 48);
    app.config.ui.detail = DetailPlace::Off;
    let off = screen(&app, colour(), 160, 48);
    assert!(!off.contains("Why safe"), "{off}");

    app.config.ui.detail = DetailPlace::Bottom;
    let bottom = screen(&app, colour(), 160, 48);
    let at = |shown: &str| shown.lines().position(|line| line.contains(wanted));
    let tree = |shown: &str| {
        shown
            .lines()
            .position(|line| line.contains("api/target  Cargo"))
    };
    assert!(at(&bottom) > tree(&bottom), "{bottom}");

    app.config.ui.detail = DetailPlace::Right;
    app.update(Msg::Resize {
        width: 100,
        height: 40,
    });
    let right = screen(&app, colour(), 100, 40);
    // Beside the tree: the frame's sides and the line between them on its row.
    let beside = |line: &&str| line.contains(wanted) && line.matches('│').count() == 3;
    assert!(right.lines().any(|line| beside(&line)), "{right}");

    app.update(Msg::Resize {
        width: 160,
        height: 48,
    });
    assert!(screen(&app, colour(), 160, 48).contains('█'));
    app.config.ui.bars = false;
    let plain = screen(&app, colour(), 160, 48);
    assert!(!plain.contains('█'), "{plain}");
}

// Catches the key bar at its narrowest dropping how to get help or how to leave to make
// room for the selection, or the selection to make room for the keys.
#[test]
fn a_narrow_key_bar_keeps_help_quit_and_the_selection() {
    let app = sized(100, 30);
    let shown = screen(&app, colour(), 100, 30);
    let bar = shown.lines().rev().nth(1).unwrap();
    for wanted in ["? help", "q quit", "selected 1", "frees 512.0 MiB"] {
        assert!(bar.contains(wanted), "{wanted}: {bar}");
    }
    let mut settings = home();
    press(&mut settings, &[Key::Char(',')]);
    let panel = screen(&settings, colour(), 160, 48);
    assert!(
        panel.lines().rev().nth(1).unwrap().contains("esc back"),
        "{panel}"
    );
}

// Catches the layout's sums on the terminal's size overflowing on a tiny or odd one,
// which would panic and leave the terminal in raw mode.
#[test]
fn every_screen_draws_at_any_size_without_panicking() {
    let mut app = sized(160, 48);
    for keys in [&[][..], &[Key::Char('?')], &[Key::Esc, Key::Char(',')]] {
        press(&mut app, keys);
        for width in (0..=130).step_by(7).chain([99, 100, 119, 120]) {
            for height in (0..=30).step_by(3).chain([23, 24, 40]) {
                app.update(Msg::Resize { width, height });
                screen(&app, colour(), width, height);
            }
        }
    }
    // The detail pane beside the tree however narrow, with a card still measuring.
    found(
        &mut app,
        9,
        CARGO,
        "builtin.cargo.target",
        "/home/ada/q/target",
    );
    app.config.ui.detail = DetailPlace::Right;
    press(&mut app, &[Key::Esc, Key::Esc]);
    for width in (0..=130).step_by(5) {
        for height in [0, 1, 5, 24, 30] {
            app.update(Msg::Resize { width, height });
            screen(&app, colour(), width, height);
        }
    }
}

const GIB: u64 = 1024;

/// A scan with every signal the title line and the totals carry, at their widest: a
/// partial scan sorted by last change, one lens, a filter, sizes of three and four
/// digits and twelve findings selected.
fn busy(width: u16, height: u16) -> App {
    let mut app = App::new(super::catalog(), width, height);
    app.home = Some(PathBuf::from("/home/ada"));
    app.roots = vec![PathBuf::from("~/projects")];
    opened(&mut app, "/home/ada/projects");
    let target = |id: u64| format!("/home/ada/projects/p{id}/target");
    for id in 1..=12 {
        found(&mut app, id, CARGO, "builtin.cargo.target", &target(id));
        measured(&mut app, id, 70 * GIB, safe());
    }
    found(&mut app, 13, CARGO, "builtin.cargo.target", &target(13));
    measured(&mut app, 13, 50 * GIB, recent());
    found(&mut app, 14, CARGO, "builtin.cargo.target", &target(14));
    measured(&mut app, 14, 120 * GIB, unknown_layout());
    partial(&mut app);
    finished(&mut app);
    press(
        &mut app,
        &[Key::Tab, Key::Tab, Key::Char('o'), Key::Char('/')],
    );
    typed(&mut app, "target");
    press(&mut app, &[Key::Enter, Key::Char('c')]);
    app
}

// Catches a signal pushed off the edge of its line and clipped without a trace: the
// partial scan, the filter, the lens or the sort order on the title line, or a total
// (found, eligible, the selection and what it frees) on the key bar or the title.
#[test]
fn every_signal_shows_at_80_by_24_and_100_by_23_however_long_it_is() {
    for (width, height) in [(80, 24), (100, 23)] {
        let app = busy(width, height);
        let shown = screen(&app, colour(), width, height);
        let title = shown.lines().next().unwrap();
        for signal in ["partial", "change", "Dev", "target"] {
            assert!(
                title.contains(signal),
                "{signal} at {width}x{height}: {shown}"
            );
        }
        for total in [
            "found 1010.0 GiB",
            "eligible 890.0 GiB",
            "selected 12",
            "frees 840.0 GiB",
        ] {
            assert!(
                shown.contains(total),
                "{total} at {width}x{height}: {shown}"
            );
        }
    }
}

// Catches the one-line band clipping what follows the buckets: that sizes are still
// being measured, and that an empty bucket is only empty because the scan is partial.
#[test]
fn the_one_line_band_keeps_measuring_and_scan_partial_with_wide_amounts() {
    let mut app = App::new(super::catalog(), 80, 24);
    let path = |id: u64| format!("/home/ada/projects/p{id}/target");
    for id in 1..=150 {
        found(&mut app, id, CARGO, "builtin.cargo.target", &path(id));
        measured(&mut app, id, GIB, safe());
    }
    for id in 151..=190 {
        found(&mut app, id, CARGO, "builtin.cargo.target", &path(id));
        measured(&mut app, id, 5 * GIB, unknown_layout());
    }
    for id in 191..=202 {
        found(&mut app, id, CARGO, "builtin.cargo.target", &path(id));
    }
    partial(&mut app);
    let shown = screen(&app, colour(), 80, 24);
    let band = shown.lines().nth(1).unwrap();
    for wanted in [
        "Safe 150.0 GiB",
        "Review none",
        "Protected 200.0 GiB",
        "Measuring 12",
        "scan partial",
    ] {
        assert!(band.contains(wanted), "{wanted}: {shown}");
    }
}

// Catches a finding's path squeezed to an ellipsis, or cut short, while columns
// beside it sit empty: every path in the tree reads whole at 120 columns, and at 160
// the rule titles that fit beside the widest state do too.
#[test]
fn the_tree_gives_a_findings_path_its_room_before_anything_else() {
    let paths = [
        "api/target",
        "tool/__pycache__",
        "web/node_modules",
        "bunapp/node_modules",
        "pacman/pkg",
    ];
    for ascii in [false, true] {
        let shown = screen(&sized(120, 40), look(true, ascii), 120, 40);
        for path in paths {
            assert!(shown.contains(&format!("{path} ")), "{path}: {shown}");
        }
        let wide = screen(&sized(160, 48), look(true, ascii), 160, 48);
        for title in [
            "api/target  Cargo build output",
            "web/node_modules  Node dependencies",
            "pacman/pkg  pacman package cache",
        ] {
            assert!(wide.contains(title), "{title}: {wide}");
        }
    }
}

// Catches a path too long for the tree cut to nothing, or cut at its end, so the line
// no longer says what it is.
#[test]
fn a_path_too_long_for_the_tree_keeps_its_last_component() {
    let mut app = App::new(super::catalog(), 80, 24);
    let long = "/home/ada/projects/a-project-whose-name-is-much-too-long-for-the-tree/node_modules";
    found(&mut app, 1, super::NODE, "builtin.node.node-modules", long);
    measured(&mut app, 1, 300, recent());
    let shown = screen(&app, colour(), 80, 24);
    let kept = |line: &&str| line.contains("/node_modules ") && line.contains("recent");
    assert!(shown.lines().any(|line| kept(&line)), "{shown}");
}

// Catches a hold's words cut short when the tree is narrow, as beside the detail pane at
// 80 columns: the state is the hold's only label (never colour alone), so the path gives
// way first, down to nothing but its ellipsis.
#[test]
fn a_findings_state_shows_whole_however_narrow_the_tree() {
    let mut app = sized(80, 24);
    app.config.ui.detail = DetailPlace::Right;
    for ascii in [false, true] {
        let shown = screen(&app, look(true, ascii), 80, 24);
        assert!(shown.contains("layout not recognised +1"), "{shown}");
    }
}

// Catches the band's card lines meeting the rule under it a few columns away from the
// line that starts there between the tree and the detail pane: wherever the detail
// pane is beside the tree, the two meet in one cross, with or without a Measuring card.
#[test]
fn the_bands_card_lines_meet_the_line_beside_the_detail_pane() {
    for width in [100, 120, 160, 200] {
        for measuring in [false, true] {
            let mut app = sized(width, 48);
            app.config.ui.detail = DetailPlace::Right;
            if measuring {
                found(
                    &mut app,
                    9,
                    CARGO,
                    "builtin.cargo.target",
                    "/home/ada/q/target",
                );
            }
            let shown = screen(&app, colour(), width, 48);
            let rule = shown.lines().nth(3).unwrap();
            assert_eq!(rule.matches('┼').count(), 1, "at {width}: {shown}");
            assert!(!rule.contains('┬'), "at {width}: {shown}");
        }
    }
}

// Catches a theme role the screen never draws, so a theme that sets it changes nothing:
// what the user kept (its state and its note), each hint's key, and the empty part of
// a bar.
#[test]
fn kept_findings_keys_and_bar_tracks_draw_in_their_own_roles() {
    let mut app = sized(160, 48);
    app.policy.keep.push(kipple_core::HeldPath {
        forms: vec![PathBuf::from("/home/ada/projects/kept")],
        lineage: kipple_core::Lineage::default(),
        note: Some("client work".to_owned()),
    });
    let path = "/home/ada/projects/kept/target";
    found(&mut app, 9, CARGO, "builtin.cargo.target", path);
    measured(&mut app, 9, 64, safe());
    super::to(&mut app, 9);
    let shown = super::styled(&super::draw(&app, super::probe(), 160, 48));

    let row = shown
        .lines()
        .find(|line| line.contains("kept/target") && line.contains("bg=cursor"))
        .unwrap();
    assert!(row.contains("fg=kept,bg=cursor| kept»"), "{row}");
    let note = shown
        .lines()
        .find(|line| line.contains("Kept note"))
        .unwrap();
    assert!(note.contains("«fg=kept|client work»"), "{note}");
    let bar = shown.lines().rev().nth(1).unwrap();
    assert!(bar.contains("«fg=key|j/k»«fg=key.label| move"), "{bar}");
    assert!(shown.contains("fg=bar.track|"), "{shown}");
}
