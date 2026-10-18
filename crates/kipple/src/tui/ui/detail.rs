//! The detail pane: what the focused line is, why it is in its bucket, and what removing
//! it would cost.

use kipple_core::{Lens, Level, Verdict};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::layout::{Areas, layout};
use super::tree::sum;
use super::wrap::wrap;
use super::{Look, shelf_title, title_case};
use crate::human::{self, bytes, escaped};
use crate::tui::app::{App, Node, ProjectId, Row, Shelf};

/// The detail pane's border: on its left beside the tree, on top below it.
fn detail_block(look: Look, wide: bool) -> Block<'static> {
    Block::new()
        .borders(if wide { Borders::LEFT } else { Borders::TOP })
        .border_set(look.border())
        .border_style(look.dim())
}

/// The detail pane for `focus`, from the line `App::detail_first` says. When more
/// follows than fits, its last line says so.
pub(super) fn render(
    app: &App,
    look: Look,
    focus: Option<Node>,
    areas: &Areas,
    frame: &mut Frame<'_>,
) {
    let block = detail_block(look, areas.wide);
    let inner = block.inner(areas.detail);
    frame.render_widget(block, areas.detail);
    let rows = focus.map_or_else(Vec::new, |node| wrap(detail(app, look, node), inner.width));
    let height = usize::from(inner.height);
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
        shown.push(Text::styled(
            format!("{} J/K scroll", look.ellipsis("more")),
            look.dim(),
        ));
    }
    frame.render_widget(Paragraph::new(shown), inner);
}

/// The last line the detail pane for `node` can scroll to on `app`'s screen: none
/// when it all fits.
pub(crate) fn detail_end(app: &App, node: Node) -> usize {
    let areas = layout(Rect::new(0, 0, app.width, app.height));
    // Both looks draw symbols of the same width, so either wraps the same.
    let look = Look {
        color: false,
        ascii: true,
    };
    let inner = detail_block(look, areas.wide).inner(areas.detail);
    let rows = wrap(detail(app, look, node), inner.width).len();
    rows.saturating_sub(usize::from(inner.height))
}

fn detail(app: &App, look: Look, node: Node) -> Vec<Text<'static>> {
    match node {
        Node::Finding(id) => app
            .row(id)
            .map_or_else(Vec::new, |row| finding_detail(app, look, row)),
        Node::Project(_, _, id) => project_detail(app, look, id),
        Node::Shelf(shelf) | Node::Group(shelf, _) => shelf_detail(app, look, node, shelf),
        Node::Gaps => vec![
            heading("Not covered"),
            Text::from(
                "The scan couldn't read everything, so this report is partial. An empty bucket doesn't mean there is nothing to clean.",
            ),
        ],
        Node::Gap(index) => app.gaps.get(index).map_or_else(Vec::new, |gap| {
            vec![
                heading("Not covered"),
                labelled("Integration", gap.integration.clone()),
                labelled("What happened", escaped(&gap.message)),
            ]
        }),
        Node::Notice(index) => app.notices.get(index).map_or_else(Vec::new, |notice| {
            vec![
                heading("Note"),
                Text::from(escaped(notice)),
                Text::styled(
                    "It doesn't make the scan partial: it says how far the check for running programs went.",
                    look.dim(),
                ),
            ]
        }),
    }
}

fn heading(text: &str) -> Text<'static> {
    Text::styled(text.to_owned(), Style::new().add_modifier(Modifier::BOLD))
}

fn labelled(label: &str, text: String) -> Text<'static> {
    Text::from(vec![
        Span::styled(
            format!("{label:<17} "),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::raw(text),
    ])
}

fn finding_detail(app: &App, look: Look, row: &Row) -> Vec<Text<'static>> {
    let finding = &row.finding;
    let text = app.catalog.rules.get(&finding.rule);
    let facts = human::facts(finding, &app.policy);
    let mut lines = vec![
        Text::from(vec![
            Span::styled(
                app.catalog.title(&finding.rule).to_owned(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw(look.dot()),
            verdict_span(look, row, app.policy.level_of(&row.finding.rule)),
        ]),
        Text::raw(escaped(&finding.path.display().to_string())),
    ];
    if let Some(text) = text {
        let what = if text.description.is_empty() {
            &text.title
        } else {
            &text.description
        };
        lines.push(labelled("What it is", escaped(what)));
        lines.push(labelled("Why it's here", escaped(&text.why)));
    }
    let level = app.policy.level_of(&row.finding.rule);
    lines.push(labelled(why_label(row.shelf()), why(row, level)));
    if let Some(text) = text {
        lines.push(labelled("What you lose", escaped(&text.loss)));
    }
    // How it comes back first, as the spec orders the pane. The verified loss is said
    // in the rule's own words above.
    let (back, rest): (Vec<_>, Vec<_>) = facts
        .into_iter()
        .filter(|(label, _)| *label != "You lose")
        .partition(|(label, _)| *label == "Comes back");
    for (_, value) in back {
        lines.push(labelled("How it comes back", value));
    }
    for (label, value) in rest {
        lines.push(labelled(label, value));
    }
    if let Some(size) = &row.size {
        lines.push(labelled("Size", human::size(Some(size))));
    }
    lines.push(labelled("Rule", finding.rule.0.clone()));
    for advice in app.advice.get(&finding.integration).into_iter().flatten() {
        lines.push(labelled("Advice", escaped(&advice.title)));
        lines.extend(
            advice
                .snippets
                .iter()
                .flat_map(|s| human::snippet_lines(s))
                .map(|line| Text::raw(format!("  {line}"))),
        );
    }
    lines
}

/// A project's line: its path, then its findings in each bucket with their totals,
/// as the filter and lens show them.
fn project_detail(app: &App, look: Look, id: ProjectId) -> Vec<Text<'static>> {
    let Some(project) = app.project(id) else {
        return Vec::new();
    };
    let mut lines = vec![
        heading(&format!("Project {}", escaped(&project.name))),
        Text::raw(escaped(&project.path.display().to_string())),
    ];
    for shelf in [
        Shelf::Safe,
        Shelf::Review,
        Shelf::Protected,
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
        lines.push(Text::from(""));
        lines.push(Text::styled(
            format!(
                "{}: {}, {}",
                shelf_title(shelf),
                human::findings(rows.len()),
                bytes(total)
            ),
            look.tint(shelf).add_modifier(Modifier::BOLD),
        ));
        lines.extend(rows.iter().map(|row| {
            Text::raw(format!(
                "  {}  {}",
                escaped(&row.spot.inside),
                human::size(row.size.as_ref())
            ))
        }));
    }
    lines
}

fn verdict_span(look: Look, row: &Row, level: Level) -> Span<'static> {
    let text = row.verdict.as_ref().map_or_else(
        || "measuring".to_owned(),
        |verdict| {
            format!(
                "{} at {}",
                human::bucket(verdict.bucket()),
                title_case(level.name())
            )
        },
    );
    Span::styled(text, look.tint(row.shelf()).add_modifier(Modifier::BOLD))
}

const fn why_label(shelf: Shelf) -> &'static str {
    match shelf {
        Shelf::Safe => "Why it's safe",
        Shelf::Review => "Why review it",
        Shelf::Protected => "Why it's held",
        Shelf::Measuring => "Status",
    }
}

fn why(row: &Row, level: Level) -> String {
    match &row.verdict {
        None => "still being measured and checked".to_owned(),
        Some(Verdict::Safe) => format!(
            "passes every check at {}: recognised, comes back, untracked, no running program shows it in use, not recent",
            title_case(level.name())
        ),
        Some(verdict) => human::reasons_in_full(verdict),
    }
}

fn shelf_detail(app: &App, look: Look, node: Node, shelf: Shelf) -> Vec<Text<'static>> {
    let (count, total) = sum(app, node);
    let about = match shelf {
        Shelf::Safe => {
            "Passes every check at this level: kipple recognised it, knows how it comes back, nothing inside is tracked by git, no running program shows it in use, and it hasn't changed recently. c selects all of it."
        }
        Shelf::Review => {
            "Eligible, but it loses more than this level allows, costs a lot to bring back, or changed recently. Each row says which."
        }
        Shelf::Protected => {
            "Not eligible: blocked, unknown, report only or advice only. Each row says why. kipple never selects these."
        }
        Shelf::Measuring => {
            "Still being measured and checked. Nothing counts as Safe until kipple knows."
        }
    };
    vec![
        Text::styled(
            shelf_title(shelf),
            look.tint(shelf).add_modifier(Modifier::BOLD),
        ),
        Text::raw(format!("{}, {}", human::findings(count), bytes(total))),
        Text::from(""),
        Text::raw(about),
    ]
}
