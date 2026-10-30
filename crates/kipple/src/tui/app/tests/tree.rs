//! The tree's projects, sort order, size bars and header (`docs/03-functional-spec.md`
//! §5 Layout, Projects, Sort, Size bars; D-054).

use std::path::PathBuf;

use kipple_core::{Consequence, Cost, Facts, FindingId, Level, Modified, WalkGap};

use super::{
    CARGO, DAY, NODE, PACMAN, PYTHON, Paint, colour, finished, focus, found, look, measured,
    opened, press, recent, safe, screen, selected, to, typed, unknown_layout,
};
use crate::theme::{Glyphs, Look};
use crate::tui::app::{App, Effect, Key, Msg, Node, Order, Shelf};

fn ascii() -> Look<'static> {
    look(Paint::Mono, Glyphs::Ascii)
}

/// Eligible, not recent, but costly to bring back: Review at every level.
fn costly() -> Facts {
    let facts = safe();
    Facts {
        consequence: facts.consequence.map(|consequence| Consequence {
            cost: Cost::High,
            ..consequence
        }),
        ..facts
    }
}

/// Measured, untouched for `days` days: Safe.
fn aged(days: u32) -> Facts {
    Facts {
        modified: Modified::Ago(DAY * days),
        ..safe()
    }
}

/// An App with `~/projects` as its one project root, opened by the scan.
fn rooted() -> App {
    let mut app = App::new(super::catalog(), 160, 48);
    app.home = Some(PathBuf::from("/home/ada"));
    app.roots = vec![PathBuf::from("~/projects")];
    opened(&mut app, "/home/ada/projects");
    app
}

/// `api` has two Safe findings, one recent and one costly in Review, and two Protected;
/// `web` has one in Safe and one in Review; pacman's cache is outside every root.
fn projects() -> App {
    let mut app = rooted();
    let cargo = "builtin.cargo.target";
    let node = "builtin.node.node-modules";
    let python = "builtin.python.bytecode";
    found(&mut app, 1, CARGO, cargo, "/home/ada/projects/api/target");
    found(
        &mut app,
        2,
        NODE,
        node,
        "/home/ada/projects/api/node_modules",
    );
    found(
        &mut app,
        3,
        PYTHON,
        python,
        "/home/ada/projects/api/tool/__pycache__",
    );
    found(
        &mut app,
        4,
        CARGO,
        cargo,
        "/home/ada/projects/api/cli/target",
    );
    found(
        &mut app,
        5,
        NODE,
        node,
        "/home/ada/projects/api/docs/node_modules",
    );
    found(
        &mut app,
        6,
        PYTHON,
        python,
        "/home/ada/projects/api/old/__pycache__",
    );
    found(&mut app, 7, CARGO, cargo, "/home/ada/projects/web/target");
    found(
        &mut app,
        8,
        NODE,
        node,
        "/home/ada/projects/web/node_modules",
    );
    found(
        &mut app,
        9,
        PACMAN,
        "builtin.pacman.cache",
        "/var/cache/pacman/pkg",
    );
    measured(&mut app, 1, 512, safe());
    measured(&mut app, 2, 100, safe());
    measured(&mut app, 3, 2, recent());
    measured(&mut app, 4, 64, costly());
    measured(&mut app, 5, 120, unknown_layout());
    measured(&mut app, 6, 8, unknown_layout());
    measured(&mut app, 7, 50, safe());
    measured(&mut app, 8, 300, recent());
    measured(&mut app, 9, 900, safe());
    app
}

/// Each line as its kind, a finding's or project's name, and its depth.
fn outline(app: &App) -> Vec<String> {
    app.lines()
        .iter()
        .map(|line| {
            let what = match line.node {
                Node::Shelf(shelf) => shelf
                    .bucket()
                    .map_or_else(|| "Measuring".to_owned(), |bucket| format!("{bucket:?}")),
                Node::Group(_, lens) => format!("{lens:?}"),
                Node::Project(_, _, id) => format!("project {}", app.project(id).unwrap().name),
                Node::Finding(id) => format!("finding {}", id.0),
                Node::Gaps | Node::Gap(_) | Node::Notice(_) => "other".to_owned(),
            };
            format!("{}{what}", "  ".repeat(usize::from(line.depth)))
        })
        .collect()
}

fn project_line(app: &App, bucket: Shelf, name: &str) -> Node {
    app.lines()
        .iter()
        .map(|line| line.node)
        .find(|node| {
            matches!(node, Node::Project(shelf, _, id)
                if *shelf == bucket && app.project(*id).unwrap().name == name)
        })
        .unwrap_or_else(|| panic!("no {name} line in {bucket:?}: {:?}", outline(app)))
}

fn go(app: &mut App, node: Node) {
    press(app, &[Key::Char('g')]);
    for _ in 0..app.lines().len() {
        if focus(app) == Some(node) {
            return;
        }
        press(app, &[Key::Down]);
    }
    panic!("{node:?} isn't shown");
}

// Catches a project line for a single finding, or one project line gathering a
// project's findings across buckets or lenses.
#[test]
fn a_project_gets_a_line_only_in_a_group_where_it_has_two_findings() {
    let mut app = projects();
    // Only Dev, so the lens groups don't split the projects.
    press(&mut app, &[Key::Tab, Key::Tab]);

    assert_eq!(
        outline(&app),
        [
            "Safe",
            "  Dev",
            "    project api",
            "      finding 1",
            "      finding 2",
            "    finding 7",
            "Review",
            "  Dev",
            "    finding 8",
            "    project api",
            "      finding 4",
            "      finding 3",
            "Protected",
            "  Dev",
            "    project api",
            "      finding 5",
            "      finding 6",
        ]
    );

    // With the other Review finding of api filtered out, one is left: a plain line.
    press(&mut app, &[Key::Char('/')]);
    typed(&mut app, "/cli/");
    press(&mut app, &[Key::Enter]);
    assert_eq!(
        outline(&app),
        ["Safe", "Review", "  Dev", "    finding 4", "Protected"]
    );
}

// Catches `h` on a project line not folding it, and `a` on it adding the project's
// findings from another bucket, a recent one or a Protected one.
#[test]
fn a_folded_project_hides_its_findings_and_a_on_it_adds_only_what_a_group_would() {
    let mut app = projects();
    let review = project_line(&app, Shelf::REVIEW, "api");
    go(&mut app, review);

    press(&mut app, &[Key::Char('h')]);
    let shown = super::findings(&app);
    assert!(!shown.contains(&3) && !shown.contains(&4), "{shown:?}");
    assert_eq!(focus(&app), Some(review));

    press(&mut app, &[Key::Char('a')]);
    assert_eq!(selected(&app), [4], "only the costly one: 3 is recent");
    let notice = app.notice.as_deref().unwrap();
    assert!(
        notice.contains("1 of 1 findings in this project"),
        "{notice}"
    );

    let protected = project_line(&app, Shelf::PROTECTED, "api");
    go(&mut app, protected);
    press(&mut app, &[Key::Char('a')]);
    assert_eq!(selected(&app), [4], "a Protected finding was added");

    go(&mut app, review);
    press(&mut app, &[Key::Char('l')]);
    to(&mut app, 3);
    press(&mut app, &[Key::Char('h')]);
    assert_eq!(
        focus(&app),
        Some(review),
        "h from a finding climbs to its project"
    );
}

// Catches `s` on a project line selecting something: like a group heading, it only
// says how to select.
#[test]
fn s_on_a_project_line_selects_nothing_as_on_a_group() {
    let mut app = projects();
    let safe = project_line(&app, Shelf::SAFE, "api");
    go(&mut app, safe);

    press(&mut app, &[Key::Char('s')]);

    assert!(app.selected.is_empty());
    assert!(app.notice.as_deref().unwrap().contains("press a"));
}

/// Four Safe Dev findings in four projects, each a different size, age and name.
fn sortable() -> App {
    let mut app = rooted();
    let cargo = "builtin.cargo.target";
    found(&mut app, 1, CARGO, cargo, "/home/ada/projects/beta/target");
    found(&mut app, 2, CARGO, cargo, "/home/ada/projects/Alpha/target");
    found(&mut app, 3, CARGO, cargo, "/home/ada/projects/delta/target");
    found(&mut app, 4, CARGO, cargo, "/home/ada/projects/gamma/target");
    measured(&mut app, 1, 30, aged(10));
    measured(&mut app, 2, 10, aged(90));
    measured(&mut app, 3, 20, aged(40));
    // Same size as 3: the path breaks the tie.
    measured(&mut app, 4, 20, aged(400));
    app
}

// Catches the cursor following the row index instead of its finding when `o`
// reorders the tree, and a cycle that doesn't come back to size.
#[test]
fn o_cycles_size_last_change_and_name_and_the_cursor_stays_on_its_finding() {
    let mut app = sortable();
    to(&mut app, 3);
    assert_eq!(super::findings(&app), [1, 3, 4, 2]);

    press(&mut app, &[Key::Char('o')]);
    assert_eq!(app.order, Order::Changed);
    assert_eq!(super::findings(&app), [4, 2, 3, 1], "oldest first");
    assert_eq!(focus(&app), Some(Node::Finding(FindingId(3))));

    press(&mut app, &[Key::Char('o')]);
    assert_eq!(
        super::findings(&app),
        [2, 1, 3, 4],
        "by name, whatever the case"
    );
    assert_eq!(focus(&app), Some(Node::Finding(FindingId(3))));

    press(&mut app, &[Key::Char('o')]);
    assert_eq!(app.order, Order::Size);
    assert_eq!(super::findings(&app), [1, 3, 4, 2]);
    assert_eq!(focus(&app), Some(Node::Finding(FindingId(3))));
}

// Catches an unknown last change sorting as the newest or the oldest, which would put
// a finding whose age nobody knows at either end of "oldest first".
#[test]
fn by_last_change_an_unknown_one_sorts_last() {
    let mut app = rooted();
    let cargo = "builtin.cargo.target";
    found(&mut app, 1, CARGO, cargo, "/home/ada/projects/a/target");
    found(&mut app, 2, CARGO, cargo, "/home/ada/projects/b/target");
    found(&mut app, 3, CARGO, cargo, "/home/ada/projects/c/target");
    let unknown = Facts {
        layout_verified: false,
        modified: Modified::Unknown(WalkGap::OtherFileSystem),
        ..safe()
    };
    measured(&mut app, 1, 10, unknown);
    measured(&mut app, 2, 10, unknown_layout());
    measured(
        &mut app,
        3,
        10,
        Facts {
            modified: Modified::Ago(DAY * 5),
            ..unknown_layout()
        },
    );

    press(&mut app, &[Key::Char('o')]);

    assert_eq!(super::findings(&app), [2, 3, 1]);
}

// Catches a project line sorting by its first finding rather than its total, or the
// findings inside it not following the order.
#[test]
fn a_project_sorts_by_its_total_and_its_findings_inside_it_too() {
    let mut app = rooted();
    let cargo = "builtin.cargo.target";
    found(&mut app, 1, CARGO, cargo, "/home/ada/projects/big/target");
    found(
        &mut app,
        2,
        CARGO,
        cargo,
        "/home/ada/projects/many/a/target",
    );
    found(
        &mut app,
        3,
        CARGO,
        cargo,
        "/home/ada/projects/many/b/target",
    );
    measured(&mut app, 1, 30, safe());
    measured(&mut app, 2, 20, safe());
    measured(&mut app, 3, 25, safe());

    assert_eq!(
        outline(&app),
        [
            "Safe",
            "  Dev",
            "    project many",
            "      finding 3",
            "      finding 2",
            "    finding 1",
            "Review",
            "Protected",
        ]
    );
}

// Catches the shallowest root winning, which would make `clients` one project holding
// every client's findings.
#[test]
fn a_finding_belongs_to_the_project_below_the_deepest_root_holding_it() {
    let mut app = rooted();
    opened(&mut app, "/home/ada/projects/clients");
    let cargo = "builtin.cargo.target";
    found(
        &mut app,
        1,
        CARGO,
        cargo,
        "/home/ada/projects/clients/acme/target",
    );
    found(
        &mut app,
        2,
        CARGO,
        cargo,
        "/home/ada/projects/clients/zeta/target",
    );
    measured(&mut app, 1, 10, safe());
    measured(&mut app, 2, 20, safe());

    assert!(!outline(&app).iter().any(|line| line.contains("project")));
    let shown = screen(&app, colour(), 160, 48);
    assert!(shown.contains("acme/target"), "{shown}");
    assert!(shown.contains("zeta/target"), "{shown}");
}

// Catches two projects of the same name under different roots reading as one, which
// would hide where each is.
#[test]
fn projects_that_share_a_name_show_their_path_from_home() {
    let mut app = rooted();
    app.roots.push(PathBuf::from("~/work"));
    opened(&mut app, "/home/ada/work");
    let cargo = "builtin.cargo.target";
    let node = "builtin.node.node-modules";
    found(&mut app, 1, CARGO, cargo, "/home/ada/projects/api/target");
    found(
        &mut app,
        2,
        NODE,
        node,
        "/home/ada/projects/api/node_modules",
    );
    found(&mut app, 3, CARGO, cargo, "/home/ada/work/api/target");
    found(&mut app, 4, CARGO, cargo, "/home/ada/work/web/target");
    for id in 1..=4 {
        measured(&mut app, id, 10 * id, safe());
    }

    let shown = screen(&app, colour(), 160, 48);

    assert!(shown.contains("▾ ~/projects/api (2)"), "{shown}");
    assert!(shown.contains("~/work/api/target"), "{shown}");
    assert!(shown.contains("  web/target"), "{shown}");
}

// Catches a finding still being measured getting a bar, or the scale coming from
// something other than the largest measured finding shown.
#[test]
fn bars_are_scaled_to_the_largest_measured_finding_and_a_measuring_one_has_none() {
    let mut app = rooted();
    let cargo = "builtin.cargo.target";
    found(&mut app, 1, CARGO, cargo, "/home/ada/projects/a/target");
    found(&mut app, 2, CARGO, cargo, "/home/ada/projects/b/target");
    found(&mut app, 3, CARGO, cargo, "/home/ada/projects/c/target");
    measured(&mut app, 1, 40, safe());
    measured(&mut app, 2, 20, safe());

    let shown = screen(&app, colour(), 160, 48);
    let line = |name: &str| {
        shown
            .lines()
            .find(|line| line.contains(name))
            .unwrap()
            .to_owned()
    };
    assert!(line("a/target").contains(" ██████ "), "{shown}");
    assert!(line("b/target").contains(" ███ "), "{shown}");
    assert!(!line("c/target").contains('█'), "{shown}");
    assert!(line("c/target").contains("measuring"), "{shown}");
    let ascii = screen(&app, ascii(), 160, 48);
    assert!(ascii.contains(" ###### "), "{ascii}");

    // Below 100 columns there is no bar.
    let narrow = screen(&app, colour(), 99, 30);
    assert!(!narrow.contains('█'), "{narrow}");
}

/// The title line of `app` on a `width` wide terminal, without the frame round it.
fn header(app: &App, look: Look<'_>, width: u16) -> String {
    let line = screen(app, look, width, 24)
        .lines()
        .next()
        .unwrap()
        .to_owned();
    line.trim_start_matches(['╭', '+'])
        .trim_end_matches(['─', '┬', '╮', '-', '+'])
        .trim()
        .to_owned()
}

// Catches the roots missing from the header, or squeezed off it at 80 columns, and the
// current level losing its mark without colour.
#[test]
fn the_header_names_the_project_roots_shortened_to_fit() {
    let mut app = rooted();
    finished(&mut app);
    let mut shown = Vec::new();
    for (look, width) in [
        (colour(), 80),
        (ascii(), 80),
        (colour(), 160),
        (ascii(), 160),
    ] {
        shown.push(header(&app, look, width));
    }
    app.roots = vec![
        PathBuf::from("~/src/clients/acme/projects/current"),
        PathBuf::from("~/work"),
        PathBuf::from("/srv/code"),
    ];
    shown.push(header(&app, colour(), 80));
    shown.push(header(&app, ascii(), 80));
    shown.push(header(&app, colour(), 160));
    press(&mut app, &[Key::Char('o')]);
    shown.push(header(&app, colour(), 80));
    shown.push(header(&app, colour(), 160));
    app.roots.clear();
    shown.push(header(&app, colour(), 80));

    insta::assert_snapshot!(shown.join("\n"));

    // Back to size, which the header doesn't name.
    press(&mut app, &[Key::Char('o'), Key::Char('o')]);
    app.roots = vec![PathBuf::from("/home/ada/code")];
    let line = header(&app, colour(), 80);
    // The screen helper writes `/` for the separator, as on every system.
    assert!(line.ends_with(" ~/code"), "{line}");
}

// Catches the roots pushing the sort order's name off the header at 80 columns: the
// name is the only lasting sign that the list isn't by size, and settings lists the
// roots anyway.
#[test]
fn the_sort_order_keeps_its_place_in_the_header_before_the_roots() {
    let mut app = rooted();
    finished(&mut app);
    app.roots = vec![PathBuf::from("~/src/clients/acme"), PathBuf::from("~/work")];
    press(&mut app, &[Key::Char('o')]);

    for look in [colour(), ascii()] {
        let narrow = header(&app, look, 80);
        assert!(narrow.ends_with("by last change"), "{narrow}");
        let wide = header(&app, look, 160);
        assert!(wide.contains("acme +1"), "{wide}");
        assert!(wide.ends_with("by last change"), "{wide}");
    }
}

// Catches the current level showing only by its colour once `NO_COLOR` is set: the
// title line names it, and only it.
#[test]
fn without_colour_the_current_level_is_still_named() {
    let mut app = rooted();
    app.policy.level = Level::Thorough;
    let plain = look(Paint::Mono, Glyphs::Unicode);

    let line = header(&app, plain, 80);

    assert!(line.starts_with("kipple ─ Thorough ─ "), "{line}");
    assert!(
        !line.contains("Careful") && !line.contains("Balanced"),
        "{line}"
    );
}

#[test]
fn projects_at_160_by_48_with_bars() {
    let mut app = projects();
    finished(&mut app);
    to(&mut app, 2);
    insta::assert_snapshot!(screen(&app, colour(), 160, 48));
}

#[test]
fn projects_at_160_by_48_in_ascii() {
    let mut app = projects();
    finished(&mut app);
    to(&mut app, 2);
    insta::assert_snapshot!(screen(&app, ascii(), 160, 48));
}

#[test]
fn projects_by_last_change_at_80_by_24() {
    let mut app = projects();
    finished(&mut app);
    app.update(Msg::Resize {
        width: 80,
        height: 24,
    });
    press(&mut app, &[Key::Char('o')]);
    insta::assert_snapshot!(screen(&app, colour(), 80, 24));
}

// Catches a project's line showing the detail of its bucket instead of the project, or
// only the findings of the bucket its line sits in: `api` also has findings to review
// and protected ones.
#[test]
fn a_project_line_details_its_path_and_its_findings_in_every_bucket() {
    let mut app = projects();
    finished(&mut app);
    let api = project_line(&app, Shelf::SAFE, "api");
    go(&mut app, api);

    // It shows from `~` below home (F1).
    let shown = screen(&app, colour(), 160, 48);

    for line in [
        "Project api",
        "~/projects/api",
        "Safe to clean: 2 findings, 612.0 MiB",
        "Review: 2 findings, 66.0 MiB",
        "Protected: 2 findings, 128.0 MiB",
        "tool/__pycache__  2.0 MiB",
    ] {
        assert!(shown.contains(line), "{line} in {shown}");
    }
    insta::assert_snapshot!(shown);
}

// Catches `y` on a project line sending nothing, or the path of a finding inside it.
#[test]
fn y_on_a_project_line_sends_the_projects_path() {
    let mut app = projects();
    finished(&mut app);
    let api = project_line(&app, Shelf::SAFE, "api");
    go(&mut app, api);

    press(&mut app, &[Key::Char('y')]);

    let Node::Project(_, _, id) = api else {
        panic!("{api:?} isn't a project line");
    };
    let path = app.project(id).unwrap().path.to_str().unwrap().to_owned();
    assert!(path.ends_with("api"), "{path}");
    assert_eq!(app.take_effect(), Some(Effect::Clipboard(path)));
}
