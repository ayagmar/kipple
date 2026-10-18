//! The dashboard (`docs/03-functional-spec.md` §5 Layout, D-057): the frame, the summary
//! band, the tree beside or above the detail pane, and the key bar, at every size and
//! as `[ui]` sets them out.

use std::path::PathBuf;

use kipple_core::{Omission, ProbeError, ScanEvent};

use super::{CARGO, colour, found, home, look, press, screen, sized};
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
        // Each card says it on its second line; the one-line band says it once.
        let band: String = shown.lines().skip(1).take(2).collect();
        let cards = if width >= 100 { 3 } else { 0 };
        let said = band.matches("none found, scan partial").count();
        assert_eq!(said, cards, "{shown}");
        assert!(band.contains("scan partial"), "{shown}");
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
    assert!(right.contains('█'), "{right}");

    app.config.ui.bars = false;
    let plain = screen(&app, colour(), 100, 40);
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
}
