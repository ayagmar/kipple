//! The detail pane: what the focused line is, why it is in its bucket, and what removing
//! it would cost, as a list of facts under aligned labels.

use kipple_core::{Lens, Level};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::Paragraph;

use super::layout::{Screen, layout};
use super::tree::sum;
use super::wrap::wrap;
use super::{Look, tint};
use crate::human::{self, bytes, escaped, homed};
use crate::theme::{Glyphs, Role, Theme};
use crate::tui::app::{App, Node, ProjectId, Row, Shelf};
use crate::tui::keymap::{Action, Mode};

/// The labels' column: as wide as the longest, `Why it's here`, and a space.
const LABEL: usize = 14;

/// A line of the pane and where its wrapped rows start.
type Entry = (Text<'static>, usize);

/// The detail pane for `focus` in `area`, from the line `App::detail_first` says. When
/// more follows than fits, its last line says so.
pub(super) fn render(
    app: &App,
    look: Look<'_>,
    focus: Option<Node>,
    area: Rect,
    frame: &mut Frame<'_>,
) {
    let rows = focus.map_or_else(Vec::new, |node| wrap(detail(app, look, node), area.width));
    let height = usize::from(area.height);
    let scrolled = focus.map_or(0, |node| app.detail_first(node));
    let first = scrolled.min(rows.len().saturating_sub(height));
    let more = rows.len() > first + height;
    let take = if more {
        height.saturating_sub(1)
    } else {
        height
    };
    let mut shown: Vec<Text<'static>> = rows.into_iter().skip(first).take(take).collect();
    if more {
        let said = look.glyphs.ellipsis("more");
        let scroll = app.keymap().hint(
            Mode::Browse,
            &[Action::DetailDown, Action::DetailUp],
            "/",
            "scroll",
        );
        let text = scroll.map_or_else(|| said.clone(), |scroll| format!("{said} {scroll}"));
        shown.push(Text::styled(text, look.style(Role::Muted)));
    }
    frame.render_widget(Paragraph::new(shown), area);
}

/// The last line the detail pane for `node` can scroll to on `app`'s screen: none
/// when it all fits or there is no pane.
pub(crate) fn detail_end(app: &App, node: Node) -> usize {
    let areas = layout(
        Rect::new(0, 0, app.width, app.height),
        &app.config.ui,
        Screen::Home,
    );
    let Some(area) = areas.detail else {
        return 0;
    };
    // Styles don't change where a line wraps, and both glyph sets draw symbols of the
    // same width, so any look wraps the same.
    let look = Look {
        theme: &Theme::default(),
        glyphs: Glyphs::Ascii,
    };
    let rows = wrap(detail(app, look, node), area.width).len();
    rows.saturating_sub(usize::from(area.height))
}

fn detail(app: &App, look: Look<'_>, node: Node) -> Vec<Entry> {
    match node {
        Node::Finding(id) => app
            .row(id)
            .map_or_else(Vec::new, |row| finding_detail(app, look, row)),
        Node::Project(_, _, id) => project_detail(app, look, id),
        Node::Shelf(shelf) | Node::Group(shelf, _) => shelf_detail(app, look, node, shelf),
        Node::Gaps => vec![
            heading(look, human::NOT_COVERED),
            plain(Text::from(
                "The scan couldn't read everything, so this report is partial. An empty bucket doesn't mean there is nothing to clean.",
            )),
        ],
        Node::Gap(index) => app.gaps.get(index).map_or_else(Vec::new, |gap| {
            vec![
                heading(look, human::NOT_COVERED),
                blank(),
                labelled(look, "Integration", gap.integration.clone()),
                labelled(look, "What happened", escaped(&gap.message)),
            ]
        }),
        Node::Notice(index) => app.notices.get(index).map_or_else(Vec::new, |notice| {
            vec![
                heading(look, "Note"),
                plain(Text::from(escaped(notice))),
                plain(Text::styled(
                    "It doesn't make the scan partial: it says how far the check for running programs went.",
                    look.style(Role::Muted),
                )),
            ]
        }),
    }
}

const fn plain(line: Text<'static>) -> Entry {
    (line, 0)
}

fn blank() -> Entry {
    plain(Text::from(""))
}

fn heading(look: Look<'_>, text: &str) -> Entry {
    plain(Text::styled(text.to_owned(), look.style(Role::Title)))
}

/// `text` under `label`, its wrapped rows under its first.
fn labelled(look: Look<'_>, label: &str, text: String) -> Entry {
    let line = Text::from(vec![
        Span::styled(format!("{label:<LABEL$}"), look.style(Role::Label)),
        Span::raw(text),
    ]);
    (line, LABEL)
}

/// A path as the screen shows it: from `~` below home.
fn shown(app: &App, path: &std::path::Path) -> String {
    escaped(&homed(path, app.home.as_deref()))
}

fn finding_detail(app: &App, look: Look<'_>, row: &Row) -> Vec<Entry> {
    let finding = &row.finding;
    let text = app.catalog.rules.get(&finding.rule);
    let level = app.policy.level_of(&finding.rule);
    let mut facts = human::facts(finding, &app.policy);
    let mut lines = vec![
        heading(look, &escaped(&row.spot.name)),
        plain(Text::from(vec![
            Span::styled(
                app.catalog.title(&finding.rule).to_owned(),
                look.style(Role::Muted),
            ),
            Span::raw(look.glyphs.dot()),
            verdict_span(look, row, level),
        ])),
        blank(),
    ];
    if let Some(text) = text {
        let what = if text.description.is_empty() {
            &text.title
        } else {
            &text.description
        };
        lines.push(labelled(look, "What it is", escaped(what)));
        lines.push(labelled(look, "Why it's here", escaped(&text.why)));
    }
    lines.push(labelled(
        look,
        human::standing(row.shelf().bucket()).why,
        human::why(row.verdict.as_ref(), level),
    ));
    let mut take = |label: &str| {
        let at = facts.iter().position(|(name, _)| *name == label)?;
        Some(facts.remove(at).1)
    };
    // The loss in the rule's own words, which say more than the verified one.
    let verified = take("You lose");
    if let Some(lose) = text.map(|text| escaped(&text.loss)).or(verified) {
        lines.push(labelled(look, "You lose", lose));
    }
    let kept = take("Kept");
    for label in [
        "Comes back",
        "Restore cost",
        "Proposes",
        "In use",
        "Git",
        "Last change",
    ] {
        if let Some(value) = take(label) {
            lines.push(labelled(look, label, value));
        }
    }
    // Any other fact, so none is ever left out.
    for (label, value) in facts {
        lines.push(labelled(look, label, value));
    }
    if let Some(size) = &row.size {
        lines.push(labelled(look, "Size", human::size(Some(size))));
    }
    lines.push(labelled(look, "Path", shown(app, &finding.path)));
    lines.push(labelled(look, "Rule", finding.rule.0.clone()));
    if let Some(kept) = kept {
        let mut line = labelled(look, "Kept note", String::new());
        line.0
            .spans
            .push(Span::styled(kept, look.style(Role::Kept)));
        lines.push(line);
    }
    for advice in app.advice.get(&finding.integration).into_iter().flatten() {
        lines.push(labelled(look, "Advice", escaped(&advice.title)));
        let indent = " ".repeat(LABEL);
        lines.extend(
            advice
                .snippets
                .iter()
                .flat_map(|s| human::snippet_lines(s))
                .map(|line| (Text::raw(format!("{indent}{line}")), LABEL)),
        );
    }
    lines
}

/// A project's line: its path, then its findings in each bucket with their totals,
/// as the filter and lens show them.
fn project_detail(app: &App, look: Look<'_>, id: ProjectId) -> Vec<Entry> {
    let Some(project) = app.project(id) else {
        return Vec::new();
    };
    let mut lines = vec![
        heading(look, &format!("Project {}", escaped(&project.name))),
        plain(Text::raw(shown(app, &project.path))),
    ];
    for shelf in [
        Shelf::SAFE,
        Shelf::REVIEW,
        Shelf::PROTECTED,
        Shelf::Measuring,
    ] {
        let mut rows: Vec<&Row> = [Lens::Agents, Lens::Dev, Lens::System]
            .into_iter()
            .flat_map(|lens| app.under(Node::Project(shelf, lens, id)))
            .collect();
        if rows.is_empty() {
            continue;
        }
        rows.sort_by(|a, b| a.spot.inside.cmp(&b.spot.inside));
        let total: u64 = rows.iter().map(|row| row.bytes()).sum();
        lines.push(blank());
        lines.push(plain(Text::styled(
            format!(
                "{}: {}, {}",
                human::standing(shelf.bucket()).title,
                human::findings(rows.len()),
                bytes(total)
            ),
            look.style(tint(shelf)).add_modifier(Modifier::BOLD),
        )));
        lines.extend(rows.iter().map(|row| {
            plain(Text::raw(format!(
                "  {}  {}",
                escaped(&row.spot.inside),
                human::size(row.size.as_ref())
            )))
        }));
    }
    lines
}

fn verdict_span(look: Look<'_>, row: &Row, level: Level) -> Span<'static> {
    Span::styled(
        human::placed(row.verdict.as_ref(), level),
        look.style(tint(row.shelf())).add_modifier(Modifier::BOLD),
    )
}

fn shelf_detail(app: &App, look: Look<'_>, node: Node, shelf: Shelf) -> Vec<Entry> {
    let (count, total) = sum(app, node);
    let standing = human::standing(shelf.bucket());
    let safe = app
        .keymap()
        .first(Mode::Browse, Action::SelectSafe)
        .filter(|_| shelf == Shelf::SAFE)
        .map_or_else(String::new, |key| format!(" {key} selects all of it."));
    let about = format!("{}{safe}", standing.about);
    vec![
        plain(Text::styled(
            standing.title,
            look.style(tint(shelf)).add_modifier(Modifier::BOLD),
        )),
        plain(Text::raw(format!(
            "{}, {}",
            human::findings(count),
            bytes(total)
        ))),
        blank(),
        plain(Text::raw(about)),
    ]
}
