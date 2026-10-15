//! The home screen's transitions (`docs/03-functional-spec.md` §2 J1 and J2, §5) and its
//! rendered buffers at 80×24 and 160×48 (`docs/08-engineering-standards.md`), from scan
//! events built here, so no file system or clock is read. Keeping and settings, which
//! write a config file in a temp dir, are in `settings`.

mod settings;

use std::path::PathBuf;
use std::time::Duration;

use kipple_core::{
    Activity, Advice, Allocated, Apparent, Category, Completeness, Consequence, Cost, Evidence,
    Facts, Finding, FindingId, HeldPath, IntegrationId, IntegrationReport, Lens, Lineage, Loss,
    Method, Modified, Omission, ProbeError, Proposal, Recovery, RuleId, RuleText, ScanEvent,
    ScanOutcome, ScanReport, SpaceEstimate, Tracked, WalkGap,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

use super::{App, Catalog, Key, Msg, Node, Shelf};
use crate::tui::view::{self, Look};

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
            Node::Shelf(_) | Node::Group(..) | Node::Gaps | Node::Gap(_) => None,
        })
        .collect()
}

fn selected(app: &App) -> Vec<u64> {
    app.selected.iter().map(|id| id.0).collect()
}

/// A home with one finding in each bucket, as `demo-home` has them.
fn home() -> App {
    let mut app = App::new(catalog(), 160, 48);
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

    assert_eq!(app.row(FindingId(1)).unwrap().shelf(), Shelf::Safe);
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
    assert!(notice.contains("unknown: layout"), "{notice}");
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

    assert_eq!(app.row(FindingId(1)).unwrap().shelf(), Shelf::Protected);
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
        .position(|line| line.node == Node::Shelf(Shelf::Protected))
        .unwrap();
    press(&mut app, &[Key::Char('g')]);
    press(&mut app, &vec![Key::Down; protected]);
    assert_eq!(focus(&app), Some(Node::Shelf(Shelf::Protected)));
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
    assert!(focus(&app).is_some());
}

#[test]
fn the_fuzzy_filter_keeps_findings_whose_name_or_path_match_in_order_and_esc_clears_it() {
    let mut app = home();

    press(&mut app, &[Key::Char('/')]);
    typed(&mut app, "webnm");
    assert_eq!(findings(&app), [2]);
    // q while typing is text, not quit.
    typed(&mut app, "q");
    assert!(!app.quit);
    press(&mut app, &[Key::Backspace, Key::Enter]);
    assert_eq!(findings(&app), [2], "enter keeps the filter");

    press(&mut app, &[Key::Esc]);
    assert_eq!(findings(&app), [1, 4, 2, 3, 5]);
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
    assert_eq!(focus(&app), Some(Node::Group(Shelf::Safe, Lens::Dev)));
    press(&mut app, &[Key::Char('h')]);
    assert_eq!(findings(&app), [2, 3, 5], "Safe's Dev group folded");
    press(&mut app, &[Key::Char('h')]);
    assert_eq!(focus(&app), Some(Node::Shelf(Shelf::Safe)));

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
    assert!(app.quit);

    let mut typing = home();
    press(&mut typing, &[Key::Char('/'), Key::Interrupt]);
    assert!(typing.quit);
}

const fn colour() -> Look {
    Look {
        color: true,
        ascii: false,
    }
}

/// `app` drawn on a `width` by `height` terminal, as text.
fn screen(app: &App, look: Look, width: u16, height: u16) -> String {
    let buffer = draw(app, look, width, height);
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

fn draw(app: &App, look: Look, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| view::render(app, look, frame))
        .unwrap();
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
    };
    app.update(Msg::Finished {
        report: Box::new(report),
        elapsed: Duration::from_millis(1_240),
    });
}

/// The home on a `width` by `height` terminal with the cursor on the first Safe finding
/// and one finding selected.
fn sized(width: u16, height: u16) -> App {
    let mut app = home();
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

    assert_eq!(app.row(FindingId(1)).unwrap().shelf(), Shelf::Protected);
    assert!(shown.contains("Size              >= 512.0 MiB"), "{shown}");
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

#[test]
fn help_at_80_by_24_in_ascii() {
    let mut app = home();
    finished(&mut app);
    app.update(Msg::Resize {
        width: 80,
        height: 24,
    });
    press(&mut app, &[Key::Char('?')]);
    let look = Look {
        color: false,
        ascii: true,
    };
    insta::assert_snapshot!(screen(&app, look, 80, 24));
}

#[test]
fn no_color_draws_every_cell_in_the_terminals_own_colours_and_ascii_only_ascii() {
    for (width, height) in [(80, 24), (160, 48)] {
        let mut app = sized(width, height);
        press(&mut app, &[Key::Char('?')]);
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
