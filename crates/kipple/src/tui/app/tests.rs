//! The home screen's transitions (`docs/03-functional-spec.md` §2 J1 and J2, §5) and its
//! rendered buffers at 80×24 and 160×48 (`docs/08-engineering-standards.md`), from scan
//! events built here, so no file system or clock is read. Keeping and settings, which
//! write a config file in a temp dir, are in `settings`; the first run is in `onboarding`.

mod dashboard;
mod detail;
mod keys;
mod onboarding;
mod parity;
mod settings;
mod themes;
mod tree;

use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use kipple_core::{
    Activity, Advice, Allocated, Apparent, Category, Completeness, Consequence, Cost, Evidence,
    Facts, Finding, FindingId, HeldPath, IntegrationId, IntegrationReport, Lens, Lineage, Loss,
    Method, Modified, Omission, ProbeError, Proposal, Recovery, RuleId, RuleText, ScanEvent,
    ScanOutcome, ScanReport, SpaceEstimate, SymbolicRoot, Tracked, WalkGap,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};

use super::{App, Catalog, Key, Msg, Node, Shelf};
use crate::theme::{Glyphs, Look, Role, Theme};
use crate::tui::ui;

const DAY: Duration = Duration::from_hours(24);
const MIB: u64 = 1024 * 1024;

const CARGO: IntegrationId = IntegrationId("cargo");
const NODE: IntegrationId = IntegrationId("node");
const PYTHON: IntegrationId = IntegrationId("python");
const PACMAN: IntegrationId = IntegrationId("pacman");

fn text(title: &str, why: &str, loss: &str) -> RuleText {
    RuleText {
        title: title.to_owned(),
        description: String::new(),
        why: why.to_owned(),
        loss: loss.to_owned(),
        references: Vec::new(),
    }
}

fn catalog() -> Catalog {
    let rules = [
        (
            "builtin.cargo.target",
            text(
                "Cargo build output",
                "Cargo writes compiled crates here when you build.",
                "The next build compiles everything again.",
            ),
        ),
        (
            "builtin.node.node-modules",
            text(
                "Node dependencies",
                "The package manager installs dependencies here.",
                "The next install downloads them again.",
            ),
        ),
        (
            "builtin.python.bytecode",
            text(
                "Python bytecode cache",
                "Python caches compiled modules here.",
                "Python compiles them again on the next import.",
            ),
        ),
        (
            "builtin.pacman.cache",
            text(
                "pacman package cache",
                "pacman keeps every package it downloaded.",
                "Downgrading without the network.",
            ),
        ),
    ];
    Catalog {
        rules: rules
            .into_iter()
            .map(|(id, text)| (RuleId(id.to_owned()), text))
            .collect(),
        lenses: [
            (CARGO, Lens::Dev),
            (NODE, Lens::Dev),
            (PYTHON, Lens::Dev),
            (PACMAN, Lens::System),
        ]
        .into_iter()
        .collect(),
    }
}

/// A recognised, untracked, idle build output untouched for 41 days: Safe at Balanced.
fn safe() -> Facts {
    Facts {
        proposal: Proposal::Remove(vec![Method::Delete, Method::Trash]),
        layout_verified: true,
        consequence: Some(Consequence {
            recovery: Recovery::RebuildLocal,
            loss: &[Loss::CacheWarmth],
            cost: Cost::Unknown,
        }),
        tracked: Tracked::NoRepository,
        activity: Activity::Idle,
        modified: Modified::Ago(41 * DAY),
        recent_days: None,
        lineage: Lineage::default(),
    }
}

/// The same, changed two days ago: Review.
fn recent() -> Facts {
    Facts {
        modified: Modified::Ago(2 * DAY),
        ..safe()
    }
}

/// A layout kipple didn't recognise: Protected.
fn unknown_layout() -> Facts {
    Facts {
        layout_verified: false,
        consequence: None,
        ..safe()
    }
}

/// A report-only rule with advice: Protected.
fn advice() -> Facts {
    Facts {
        proposal: Proposal::Advice,
        ..safe()
    }
}

fn size(bytes: u64, unique: u64) -> SpaceEstimate {
    SpaceEstimate {
        apparent: Apparent(bytes),
        allocated: Some(Allocated(bytes)),
        unique_reclaim: Some(Allocated(unique)),
        completeness: Completeness::Complete,
    }
}

fn found(app: &mut App, id: u64, integration: IntegrationId, rule: &str, path: &str) {
    let finding = Finding {
        id: FindingId(id),
        integration,
        rule: RuleId(rule.to_owned()),
        category: Category::BuildArtifact,
        path: PathBuf::from(path),
        evidence: vec![Evidence::Marker("Cargo.toml".to_owned())],
        size: None,
        facts: None,
    };
    app.update(Msg::Scan(Box::new(ScanEvent::FindingAdded { finding })));
}

fn measured(app: &mut App, id: u64, mib: u64, facts: Facts) {
    app.update(Msg::Scan(Box::new(ScanEvent::FindingUpdated {
        id: FindingId(id),
        size: Some(size(mib * MIB, mib * MIB)),
        facts,
    })));
}

/// The scan opened the project root at `path`.
fn opened(app: &mut App, path: &str) {
    app.update(Msg::Scan(Box::new(ScanEvent::RootOpened {
        integration: CARGO,
        root: SymbolicRoot::ProjectRoots,
        path: PathBuf::from(path),
    })));
}

fn press(app: &mut App, keys: &[Key]) {
    for &key in keys {
        app.update(Msg::Key(key));
    }
}

/// Moves the cursor down to finding `id`.
fn to(app: &mut App, id: u64) {
    let wanted = Some(Node::Finding(FindingId(id)));
    for _ in 0..app.lines().len() {
        if focus(app) == wanted {
            return;
        }
        app.update(Msg::Key(Key::Down));
    }
    assert_eq!(focus(app), wanted, "finding {id} isn't shown");
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        app.update(Msg::Key(Key::Char(c)));
    }
}

fn focus(app: &App) -> Option<Node> {
    let lines = app.lines();
    lines.get(app.cursor_index(&lines)).map(|line| line.node)
}

fn findings(app: &App) -> Vec<u64> {
    app.lines()
        .iter()
        .filter_map(|line| match line.node {
            Node::Finding(id) => Some(id.0),
            Node::Shelf(_)
            | Node::Group(..)
            | Node::Project(..)
            | Node::Gaps
            | Node::Gap(_)
            | Node::Notice(_) => None,
        })
        .collect()
}

fn selected(app: &App) -> Vec<u64> {
    app.selected.iter().map(|id| id.0).collect()
}

/// A home with one finding in each bucket, as `demo-home` has them.
fn home() -> App {
    with_findings(App::new(catalog(), 160, 48))
}

/// `app` with the findings of [`home`].
fn with_findings(mut app: App) -> App {
    found(
        &mut app,
        1,
        CARGO,
        "builtin.cargo.target",
        "/home/ada/projects/api/target",
    );
    found(
        &mut app,
        2,
        NODE,
        "builtin.node.node-modules",
        "/home/ada/projects/web/node_modules",
    );
    found(
        &mut app,
        3,
        NODE,
        "builtin.node.node-modules",
        "/home/ada/projects/bunapp/node_modules",
    );
    found(
        &mut app,
        4,
        PYTHON,
        "builtin.python.bytecode",
        "/home/ada/projects/tool/__pycache__",
    );
    found(
        &mut app,
        5,
        PACMAN,
        "builtin.pacman.cache",
        "/var/cache/pacman/pkg",
    );
    measured(&mut app, 1, 512, safe());
    measured(&mut app, 2, 300, recent());
    measured(&mut app, 3, 120, unknown_layout());
    measured(&mut app, 4, 2, safe());
    measured(&mut app, 5, 900, advice());
    app
}

#[test]
fn a_finding_waits_under_measuring_until_its_facts_arrive_and_is_never_safe_before() {
    let mut app = App::new(catalog(), 160, 48);
    found(&mut app, 1, CARGO, "builtin.cargo.target", "/p/api/target");

    let row = app.row(FindingId(1)).unwrap();
    assert_eq!(row.shelf(), Shelf::Measuring);
    assert_eq!(app.totals().eligible, 0);
    press(&mut app, &[Key::Char('G'), Key::Char('s'), Key::Char('c')]);
    assert!(
        app.selected.is_empty(),
        "an unmeasured finding was selected"
    );

    measured(&mut app, 1, 512, safe());

    assert_eq!(app.row(FindingId(1)).unwrap().shelf(), Shelf::SAFE);
    assert_eq!(app.totals().eligible, 512 * MIB);
}

#[test]
fn c_selects_the_safe_bucket_as_it_stands_at_the_keypress_and_nothing_else() {
    let mut app = home();
    found(
        &mut app,
        6,
        CARGO,
        "builtin.cargo.target",
        "/home/ada/projects/cli/target",
    );

    press(&mut app, &[Key::Char('c')]);
    // Sorted Safe only after the keypress, so not part of what c froze.
    measured(&mut app, 6, 64, safe());

    assert_eq!(selected(&app), [1, 4]);
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.contains("nothing was changed"), "{notice}");
}

#[test]
fn s_toggles_an_eligible_finding_and_refuses_a_protected_one_with_its_reason() {
    let mut app = home();
    app.update(Msg::Key(Key::Char('/')));
    typed(&mut app, "web");
    press(&mut app, &[Key::Enter]);
    to(&mut app, 2);
    press(&mut app, &[Key::Char('s')]);
    assert_eq!(selected(&app), [2], "a Review finding can be selected");
    press(&mut app, &[Key::Char('s')]);
    assert!(app.selected.is_empty(), "s unselects");

    press(&mut app, &[Key::Esc, Key::Char('/')]);
    typed(&mut app, "bunapp");
    press(&mut app, &[Key::Enter]);
    to(&mut app, 3);
    press(&mut app, &[Key::Char('s')]);

    assert!(app.selected.is_empty(), "a Protected finding was selected");
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.contains("layout not recognised"), "{notice}");
}

// Catches the home screen judging with the default policy instead of the configured one,
// which would offer a kept project's artifacts as Safe.
#[test]
fn a_finding_under_a_kept_path_is_protected_and_s_refuses_it_as_kept() {
    let mut app = App::new(catalog(), 160, 48);
    app.policy.keep.push(HeldPath {
        forms: vec![PathBuf::from("/home/ada/projects/api")],
        lineage: Lineage::default(),
        note: Some("client work".to_owned()),
    });
    found(
        &mut app,
        1,
        CARGO,
        "builtin.cargo.target",
        "/home/ada/projects/api/target",
    );
    measured(&mut app, 1, 512, safe());

    assert_eq!(app.row(FindingId(1)).unwrap().shelf(), Shelf::PROTECTED);
    to(&mut app, 1);
    press(&mut app, &[Key::Char('s')]);
    assert!(app.selected.is_empty(), "a kept finding was selected");
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.contains("kept"), "{notice}");
}

#[test]
fn a_adds_the_eligible_findings_under_the_focused_heading_but_never_protected() {
    let mut app = home();
    // Safe to clean is the first line.
    press(&mut app, &[Key::Char('a')]);
    assert_eq!(selected(&app), [1, 4]);

    let protected = app
        .lines()
        .iter()
        .position(|line| line.node == Node::Shelf(Shelf::PROTECTED))
        .unwrap();
    press(&mut app, &[Key::Char('g')]);
    press(&mut app, &vec![Key::Down; protected]);
    assert_eq!(focus(&app), Some(Node::Shelf(Shelf::PROTECTED)));
    press(&mut app, &[Key::Char('a')]);

    assert_eq!(selected(&app), [1, 4], "a Protected finding was added");
}

#[test]
fn the_cursor_stays_on_its_finding_while_sizes_stream_in_and_reorder_the_tree() {
    let mut app = App::new(catalog(), 160, 48);
    found(&mut app, 1, CARGO, "builtin.cargo.target", "/p/a/target");
    found(&mut app, 2, CARGO, "builtin.cargo.target", "/p/b/target");
    measured(&mut app, 1, 10, safe());
    measured(&mut app, 2, 5, safe());
    to(&mut app, 2);

    // A larger size moves it above the other; the cursor follows it.
    measured(&mut app, 2, 50, safe());
    assert_eq!(findings(&app), [2, 1]);
    assert_eq!(focus(&app), Some(Node::Finding(FindingId(2))));

    // When its line goes away, the cursor stays where it was rather than jumping.
    press(&mut app, &[Key::Char('/')]);
    typed(&mut app, "zzz");
    assert_eq!(findings(&app), Vec::<u64>::new());
    assert!(
        focus(&app).is_some(),
        "the cursor stays on a finding while the filter shows none"
    );
}

/// The findings `query` leaves shown, typed after `/` and cleared again with `esc`.
fn filtered(app: &mut App, query: &str) -> Vec<u64> {
    press(app, &[Key::Char('/')]);
    typed(app, query);
    let shown = findings(app);
    press(app, &[Key::Esc]);
    shown
}

// Catches the filter matching letters scattered across the title and the whole path, so
// `/target` found `tool/.venv` (F3); terms that must be typed in the path's order; and a
// term matching the home directory's own name, which every path below home shares.
#[test]
fn every_filter_term_must_appear_whole_in_the_path_as_shown_or_the_title() {
    let mut app = App::new(catalog(), 160, 48);
    app.home = Some(PathBuf::from("/home/ada"));
    let mut app = with_findings(app);

    assert_eq!(filtered(&mut app, "web node"), [2]);
    assert_eq!(filtered(&mut app, "node web"), [2], "terms in any order");
    assert_eq!(filtered(&mut app, "webnm"), Vec::<u64>::new(), "no fuzzy");
    assert_eq!(filtered(&mut app, "/target"), [1]);
    assert_eq!(filtered(&mut app, "ada"), Vec::<u64>::new(), "home is ~");
    assert_eq!(filtered(&mut app, "~/projects/tool"), [4]);
    assert_eq!(filtered(&mut app, "BYTECODE tool"), [4], "title and path");
    assert_eq!(filtered(&mut app, "  "), [1, 4, 2, 3, 5]);
}

#[test]
fn enter_keeps_the_filter_and_esc_clears_it() {
    let mut app = home();

    press(&mut app, &[Key::Char('/')]);
    typed(&mut app, "web");
    assert_eq!(findings(&app), [2]);
    // q while typing is text, not quit.
    typed(&mut app, "q");
    assert!(!app.quit, "q while typing is text, not quit");
    press(&mut app, &[Key::Backspace, Key::Enter]);
    assert_eq!(findings(&app), [2], "enter keeps the filter");

    press(&mut app, &[Key::Esc]);
    assert_eq!(findings(&app), [1, 4, 2, 3, 5]);
}

// Catches `esc` leaving the cursor on whatever line took its place while typing (F5),
// instead of on the line where `/` was pressed.
#[test]
fn esc_in_the_filter_puts_the_cursor_back_where_slash_was_pressed() {
    let mut app = home();
    to(&mut app, 4);

    press(&mut app, &[Key::Char('/')]);
    typed(&mut app, "web");
    press(&mut app, &[Key::Esc]);

    assert_eq!(focus(&app), Some(Node::Finding(FindingId(4))));
}

// Catches a line the filter hides handing the cursor to the line that takes its place
// (F5), a bucket heading, instead of the nearest finding before it, else after it.
#[test]
fn a_line_the_filter_hides_moves_the_cursor_to_the_nearest_finding() {
    let mut app = home();
    to(&mut app, 4);
    press(&mut app, &[Key::Char('/')]);
    typed(&mut app, "target");
    assert_eq!(focus(&app), Some(Node::Finding(FindingId(1))), "before");
    press(&mut app, &[Key::Enter]);
    assert_eq!(focus(&app), Some(Node::Finding(FindingId(1))), "enter");
    press(&mut app, &[Key::Esc]);
    assert_eq!(focus(&app), Some(Node::Finding(FindingId(1))), "cleared");

    // Nothing is shown before the first finding.
    press(&mut app, &[Key::Char('/')]);
    typed(&mut app, "bunapp");
    assert_eq!(focus(&app), Some(Node::Finding(FindingId(3))), "after");
}

// Catches the filter matching the native path, so on Windows `rust-app/target`, as the
// tree writes paths, matches nothing; and a query typed with `\\` there missing too.
#[test]
fn the_filter_reads_a_path_with_slashes_whatever_the_system_writes() {
    let mut app = App::new(catalog(), 160, 48);
    let native = Path::new("home").join("rust-app").join("target");
    found(
        &mut app,
        1,
        CARGO,
        "builtin.cargo.target",
        native.to_str().unwrap(),
    );

    press(&mut app, &[Key::Char('/')]);
    typed(&mut app, "rust-app/target");
    assert_eq!(findings(&app), [1]);
    press(&mut app, &[Key::Esc, Key::Char('/')]);
    typed(&mut app, &format!("rust-app{MAIN_SEPARATOR}target"));
    assert_eq!(findings(&app), [1]);
}

#[test]
fn tab_shows_one_lens_at_a_time_then_all_of_them() {
    let mut app = home();

    press(&mut app, &[Key::Tab]);
    assert!(findings(&app).is_empty(), "nothing in Agents");
    press(&mut app, &[Key::Tab]);
    assert_eq!(findings(&app), [1, 4, 2, 3]);
    press(&mut app, &[Key::Tab]);
    assert_eq!(findings(&app), [5]);
    press(&mut app, &[Key::Tab]);
    assert_eq!(findings(&app).len(), 5);
}

#[test]
fn h_folds_a_heading_and_from_a_finding_climbs_to_its_heading_and_l_unfolds() {
    let mut app = home();
    // Safe to clean, Dev, then the first Safe finding.
    press(&mut app, &[Key::Down, Key::Down]);
    assert_eq!(focus(&app), Some(Node::Finding(FindingId(1))));

    press(&mut app, &[Key::Char('h')]);
    assert_eq!(focus(&app), Some(Node::Group(Shelf::SAFE, Lens::Dev)));
    press(&mut app, &[Key::Char('h')]);
    assert_eq!(findings(&app), [2, 3, 5], "Safe's Dev group folded");
    press(&mut app, &[Key::Char('h')]);
    assert_eq!(focus(&app), Some(Node::Shelf(Shelf::SAFE)));

    press(&mut app, &[Key::Char('l'), Key::Down, Key::Char('l')]);
    assert_eq!(findings(&app), [1, 4, 2, 3, 5]);
}

#[test]
fn an_incomplete_scan_lists_what_it_could_not_cover_instead_of_looking_empty() {
    let mut app = App::new(catalog(), 160, 48);
    let error = ProbeError::io(
        &PathBuf::from("/home/ada/projects/secret"),
        std::io::ErrorKind::PermissionDenied.into(),
    );
    app.update(Msg::Scan(Box::new(ScanEvent::IntegrationCompleted {
        integration: CARGO,
        omissions: vec![Omission::Unlisted { error }],
    })));

    let gaps: Vec<Node> = app
        .lines()
        .iter()
        .map(|line| line.node)
        .filter(|node| matches!(node, Node::Gaps | Node::Gap(_)))
        .collect();
    assert_eq!(gaps, [Node::Gaps, Node::Gap(0)]);
    let screen = screen(&app, colour(), 160, 48);
    assert!(screen.contains("partial: 1 not covered"), "{screen}");
}

// Catches the D-050 and D-052 notices reaching `scan` and `doctor` but not the home
// screen, or being counted as something the scan didn't cover, which would call a
// complete scan partial.
#[test]
fn processes_left_out_of_the_activity_check_are_a_notice_not_a_gap() {
    let ended = |unchecked_system, by_command_line| {
        let mut app = App::new(catalog(), 160, 48);
        app.update(Msg::Finished {
            report: Box::new(ScanReport {
                outcome: ScanOutcome::Completed,
                integrations: Vec::new(),
                findings: Vec::new(),
                unchecked_system,
                by_command_line,
            }),
            elapsed: Duration::from_secs(1),
        });
        app
    };

    let app = ended(3, 2);

    assert_eq!(
        app.notices,
        [
            "3 processes owned by root or SYSTEM, or running with root's rights, couldn't be inspected and were not counted for items in your home.",
            "2 of your own processes couldn't be fully inspected and were checked by their command line only for items in your home."
        ]
    );
    assert_eq!(app.gaps, []);
    let screen = screen(&app, colour(), 160, 48);
    assert!(
        screen.contains("Note: 3 processes owned by root"),
        "{screen}"
    );
    assert!(!screen.contains("partial"), "{screen}");
    assert!(!screen.contains("Not covered"), "{screen}");
    assert!(screen.contains("Note: 2 of your own processes"), "{screen}");
    assert_eq!(ended(0, 0).notices, Vec::<String>::new());
}

#[test]
fn totals_count_what_is_eligible_and_selected_and_frees_only_storage_nothing_else_holds() {
    let mut app = App::new(catalog(), 160, 48);
    found(&mut app, 1, CARGO, "builtin.cargo.target", "/p/a/target");
    found(
        &mut app,
        2,
        PACMAN,
        "builtin.pacman.cache",
        "/var/cache/pacman/pkg",
    );
    app.update(Msg::Scan(Box::new(ScanEvent::FindingUpdated {
        id: FindingId(1),
        // 40 MiB of it are hard links also reachable from elsewhere.
        size: Some(size(100 * MIB, 60 * MIB)),
        facts: safe(),
    })));
    measured(&mut app, 2, 900, advice());

    press(&mut app, &[Key::Char('c')]);

    let totals = app.totals();
    assert_eq!(totals.discovered, 1000 * MIB);
    assert_eq!(totals.eligible, 100 * MIB);
    assert_eq!((totals.selected_count, totals.selected), (1, 100 * MIB));
    assert_eq!(totals.reclaim, 60 * MIB);
}

#[test]
fn q_and_ctrl_c_quit_and_any_key_closes_help() {
    let mut app = home();
    press(&mut app, &[Key::Char('?')]);
    press(&mut app, &[Key::Char('q')]);
    assert!(!app.quit, "q closes help first");
    press(&mut app, &[Key::Char('q')]);
    assert!(app.quit, "a second q quits");

    let mut typing = home();
    press(&mut typing, &[Key::Char('/'), Key::Interrupt]);
    assert!(typing.quit, "ctrl-c quits while typing");
}

/// Whether a test draws in colour.
#[derive(Clone, Copy)]
enum Paint {
    /// The default theme.
    Colour,
    /// `mono`, without colour.
    Mono,
}

/// The theme `paint` says, with `glyphs`.
fn look(paint: Paint, glyphs: Glyphs) -> Look<'static> {
    static KIPPLE: LazyLock<Theme> = LazyLock::new(|| Theme::builtin("kipple").unwrap());
    static MONO: LazyLock<Theme> = LazyLock::new(|| Theme::builtin("mono").unwrap());
    let theme = match paint {
        Paint::Colour => &KIPPLE,
        Paint::Mono => &MONO,
    };
    Look { theme, glyphs }
}

/// The probe theme, which draws each role in its own colour, with Unicode symbols.
fn probe() -> Look<'static> {
    static PROBE: LazyLock<Theme> = LazyLock::new(Theme::probe);
    Look {
        theme: &PROBE,
        glyphs: Glyphs::Unicode,
    }
}

fn colour() -> Look<'static> {
    look(Paint::Colour, Glyphs::Unicode)
}

/// `app` drawn on a `width` by `height` terminal, as text. Paths shown from `~` are
/// written with `/`, so a snapshot reads the same on Windows.
fn screen(app: &App, look: Look<'_>, width: u16, height: u16) -> String {
    let buffer = draw(app, look, width, height);
    let mut text = String::new();
    for y in 0..height {
        let line: String = (0..width)
            .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
            .collect();
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text.replace(MAIN_SEPARATOR, "/")
}

/// `buffer` as text with its style runs: each run of cells drawn in something other
/// than the terminal's own style reads `«fg=green,bold|text»`, so a snapshot of it
/// catches a colour or a modifier that moves.
fn styled(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area;
    let mut text = String::new();
    for y in area.top()..area.bottom() {
        let mut runs: Vec<(String, String)> = Vec::new();
        for x in area.left()..area.right() {
            let Some(cell) = buffer.cell((x, y)) else {
                continue;
            };
            let style = style_name(cell.fg, cell.bg, cell.modifier);
            match runs.last_mut() {
                Some((last, run)) if *last == style => run.push_str(cell.symbol()),
                _ => runs.push((style, cell.symbol().to_owned())),
            }
        }
        let line: String = runs
            .into_iter()
            .map(|(style, run)| {
                if style.is_empty() {
                    run
                } else {
                    format!("«{style}|{run}»")
                }
            })
            .collect();
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text.replace(MAIN_SEPARATOR, "/")
}

/// A cell's style as `fg=Green,bg=…,bold,reversed`, leaving out the terminal's own
/// colours: empty when it has none.
fn style_name(fg: Color, bg: Color, modifier: Modifier) -> String {
    let mut parts = Vec::new();
    for (name, colour) in [("fg", fg), ("bg", bg)] {
        if colour == Color::Reset {
            continue;
        }
        // The probe theme's colours, by the role they stand for; the text role every cell
        // starts from is left out, so the other roles read.
        let probed = if let Color::Indexed(index) = colour {
            Role::ALL.get(usize::from(index)).copied()
        } else {
            None
        };
        match probed {
            Some(Role::Text) => {}
            Some(role) => parts.push(format!("{name}={}", role.name())),
            None => parts.push(format!("{name}={colour}")),
        }
    }
    for (name, _) in modifier.iter_names() {
        parts.push(name.to_lowercase());
    }
    parts.join(",")
}

fn draw(app: &App, look: Look<'_>, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| ui::render(app, look, frame)).unwrap();
    terminal.backend().buffer().clone()
}

fn finished(app: &mut App) {
    let report = ScanReport {
        outcome: ScanOutcome::Completed,
        integrations: vec![IntegrationReport {
            id: PACMAN,
            omissions: Vec::new(),
            advice: vec![Advice {
                title: "Keep three versions of each package".to_owned(),
                text: "paccache prunes the cache.".to_owned(),
                snippets: vec!["sudo paccache -rk3".to_owned()],
                references: Vec::new(),
            }],
        }],
        findings: Vec::new(),
        unchecked_system: 0,
        by_command_line: 0,
    };
    app.update(Msg::Finished {
        report: Box::new(report),
        elapsed: Duration::from_millis(1_240),
    });
}

/// The home on a `width` by `height` terminal with the cursor on the first Safe finding
/// and one finding selected, and `~/projects` as its project root.
fn sized(width: u16, height: u16) -> App {
    let mut app = home();
    app.home = Some(PathBuf::from("/home/ada"));
    app.roots = vec![PathBuf::from("~/projects")];
    opened(&mut app, "/home/ada/projects");
    finished(&mut app);
    app.update(Msg::Resize { width, height });
    to(&mut app, 1);
    press(&mut app, &[Key::Char('s')]);
    app
}

#[test]
fn a_finding_holding_a_mount_shows_its_size_as_a_lower_bound_and_names_the_mount() {
    // A bind mount or btrfs subvolume inside a finding is never walked: its bytes are
    // missing from the size and it could hold a checkout or recent files.
    let mut app = App::new(catalog(), 160, 48);
    found(&mut app, 1, CARGO, "builtin.cargo.target", "/p/api/target");
    app.update(Msg::Scan(Box::new(ScanEvent::FindingUpdated {
        id: FindingId(1),
        size: Some(SpaceEstimate {
            completeness: Completeness::Incomplete {
                unreadable: 0,
                skipped_mounts: 1,
            },
            ..size(512 * MIB, 512 * MIB)
        }),
        facts: Facts {
            tracked: Tracked::Unwalked(WalkGap::OtherFileSystem),
            modified: Modified::Unknown(WalkGap::OtherFileSystem),
            ..safe()
        },
    })));
    to(&mut app, 1);

    let shown = screen(&app, colour(), 160, 48);

    assert_eq!(app.row(FindingId(1)).unwrap().shelf(), Shelf::PROTECTED);
    assert!(shown.contains("Size          >= 512.0 MiB"), "{shown}");
    for label in ["Git ", "Last change "] {
        let line = shown.lines().find(|line| line.contains(label)).unwrap();
        assert!(
            line.contains("a directory inside is on another file system"),
            "{shown}"
        );
    }
}

#[test]
fn the_bucket_home_at_80_by_24_stacks_the_detail_pane_below_the_tree() {
    let app = sized(80, 24);
    insta::assert_snapshot!(screen(&app, colour(), 80, 24));
}

#[test]
fn the_bucket_home_at_160_by_48_puts_the_detail_pane_beside_the_tree() {
    let app = sized(160, 48);
    insta::assert_snapshot!(screen(&app, colour(), 160, 48));
}

// Catches a colour or a modifier on the home screen changing while its drawing code
// moves: the text snapshots above see only the characters.
#[test]
fn the_bucket_home_at_160_by_48_in_colour() {
    let app = sized(160, 48);
    insta::assert_snapshot!(styled(&draw(&app, colour(), 160, 48)));
}

// Catches a part of the screen drawn in the terminal's own colours, which no theme can
// reach: under the probe every cell has some role's colour.
#[test]
fn every_cell_of_the_home_screen_and_its_help_is_drawn_in_a_role() {
    let mut app = sized(160, 48);
    for keys in [&[][..], &[Key::Char('?')], &[Key::Esc, Key::Char(',')]] {
        press(&mut app, keys);
        let buffer = draw(&app, probe(), 160, 48);
        for cell in &buffer.content {
            assert_ne!(cell.fg, Color::Reset, "{keys:?} {cell:?}");
        }
    }
}

// Catches a part of the screen asking for the wrong role, which the default theme can't
// show because several roles share a colour there: muted text drawn as a border, a bar
// as its bucket's name, the cursor no longer winning over a span's own style.
#[test]
fn the_bucket_home_and_its_help_draw_each_part_in_its_role() {
    let mut app = sized(160, 48);
    let home = styled(&draw(&app, probe(), 160, 48));
    press(&mut app, &[Key::Char('?')]);
    let help = styled(&draw(&app, probe(), 160, 48));
    insta::assert_snapshot!(format!("{home}\n{help}"));
}

// The same for the help over it.
#[test]
fn help_at_160_by_48_in_colour() {
    let mut app = sized(160, 48);
    press(&mut app, &[Key::Char('?')]);
    insta::assert_snapshot!(styled(&draw(&app, colour(), 160, 48)));
}

#[test]
fn help_at_80_by_24_in_ascii() {
    let mut app = home();
    finished(&mut app);
    app.update(Msg::Resize {
        width: 80,
        height: 24,
    });
    press(&mut app, &[Key::Char('?')]);
    let look = look(Paint::Mono, Glyphs::Ascii);
    insta::assert_snapshot!(screen(&app, look, 80, 24));
}

#[test]
fn no_color_draws_every_cell_in_the_terminals_own_colours_and_ascii_only_ascii() {
    for (width, height) in [(80, 24), (160, 48)] {
        let mut app = sized(width, height);
        press(&mut app, &[Key::Char('?')]);
        let plain = look(Paint::Mono, Glyphs::Ascii);
        let buffer = draw(&app, plain, width, height);

        for cell in &buffer.content {
            assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset), "{cell:?}");
            assert!(cell.symbol().is_ascii(), "{cell:?}");
        }
    }
}
