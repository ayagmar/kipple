//! The first run's steps, each in the same frame: a title line, the question and its
//! choices, and the keys.

use kipple_core::{Level, Method, ScanOutcome};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::settings::{describe, search_notes};
use super::{Look, level_about, title_case};
use crate::human::{self, bytes, escaped};
use crate::theme::Role;
use crate::tui::app::{App, Progress};
use crate::tui::keymap::{Action, Mode};
use crate::tui::onboarding::{Onboarding, Step, level_totals};

/// Draws the step `onboarding` is on, with `app`'s scan for the numbers and tips.
pub(crate) fn render(onboarding: &Onboarding, app: &App, look: Look<'_>, frame: &mut Frame<'_>) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    look.fill(frame);
    frame.render_widget(head(onboarding, app, look), header);
    let lines = match onboarding.step {
        Step::Level => level_lines(onboarding, app, look),
        Step::Roots => root_lines(onboarding, app, look),
        Step::Method => method_lines(onboarding, look),
        Step::Tips => tip_lines(app, look),
    };
    let scroll = if onboarding.step == Step::Tips {
        onboarding.scroll
    } else {
        0
    };
    let block = Block::new()
        .borders(Borders::TOP)
        .border_set(look.glyphs.border())
        .border_style(look.style(Role::Border));
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, body);
    frame.render_widget(keys(onboarding, look), footer);
}

fn head(onboarding: &Onboarding, app: &App, look: Look<'_>) -> Paragraph<'static> {
    let state = match app.progress {
        Progress::Scanning => look.glyphs.ellipsis("scanning"),
        Progress::Done { outcome, .. } if outcome != ScanOutcome::Completed => {
            "scan stopped early".to_owned()
        }
        Progress::Done { .. } => "scan finished".to_owned(),
    };
    Paragraph::new(Line::from(vec![
        Span::styled(" Welcome to kipple", look.style(Role::Title)),
        Span::raw(format!(
            "{}step {} of 4{}",
            look.glyphs.dot(),
            onboarding.step.number(),
            look.glyphs.dot()
        )),
        Span::styled(state, look.style(Role::Muted)),
    ]))
}

fn question(text: &str, about: &str, look: Look<'_>) -> Vec<Line<'static>> {
    vec![
        Line::styled(text.to_owned(), look.style(Role::Title)),
        Line::styled(about.to_owned(), look.style(Role::Muted)),
        Line::from(""),
    ]
}

/// A choice line: a marker, and the cursor's style when the cursor is on it.
fn choice(look: Look<'_>, focused: bool, text: &str) -> Line<'static> {
    let line = Line::from(format!("{}{text}", if focused { "> " } else { "  " }));
    if focused { look.cursor(line) } else { line }
}

fn level_lines(onboarding: &Onboarding, app: &App, look: Look<'_>) -> Vec<Line<'static>> {
    let mut lines = question(
        "How careful should kipple be?",
        "Each level shows what it would offer on this machine. It changes nothing.",
        look,
    );
    let findings = app.rows.iter().map(|row| &row.finding);
    for &level in Level::ALL {
        let totals = level_totals(findings.clone(), &app.policy, level);
        let mut name = title_case(level.name());
        if level == Level::Balanced {
            name.push_str(" (the default)");
        }
        lines.push(choice(look, level == onboarding.level, &name));
        lines.push(Line::styled(
            format!("    {}", level_about(level)),
            look.style(Role::Muted),
        ));
        lines.push(Line::from(format!(
            "    {} safe to clean, {} to review",
            bytes(totals.safe),
            bytes(totals.review)
        )));
    }
    lines.push(Line::from(""));
    let note = match app.progress {
        Progress::Scanning => "The numbers grow while the scan runs.",
        Progress::Done { .. } => "The scan has finished: these are the totals.",
    };
    lines.push(Line::styled(note, look.style(Role::Muted)));
    lines
}

fn root_lines(onboarding: &Onboarding, app: &App, look: Look<'_>) -> Vec<Line<'static>> {
    let mut lines = question(
        "Where are your projects?",
        "kipple looks for build output and dependencies below these folders.",
        look,
    );
    if onboarding.roots.is_empty() {
        lines.push(Line::styled(
            "None of the usual project folders exist (~/projects, ~/code, ~/src and others).",
            look.style(Role::Muted),
        ));
    }
    for (index, root) in onboarding.roots.iter().enumerate() {
        let mark = if root.checked { "[x]" } else { "[ ]" };
        let path = escaped(&root.path.display().to_string());
        lines.push(choice(
            look,
            index == onboarding.root_cursor,
            &format!("{mark} {path}"),
        ));
    }
    let add = onboarding.typing.as_ref().map_or_else(
        || "+ add another".to_owned(),
        |typed| format!("+ {}_", escaped(typed)),
    );
    lines.push(choice(look, onboarding.on_add(), &add));
    suggestion_lines(onboarding, app, look, &mut lines);
    if let Some(notice) = &onboarding.notice {
        lines.push(Line::from(""));
        lines.push(Line::from(notice.clone()));
    }
    lines
}

/// The roots kipple suggests, below "add another", with what the search says.
fn suggestion_lines(onboarding: &Onboarding, app: &App, look: Look<'_>, lines: &mut Vec<Line<'_>>) {
    let notes = search_notes(&app.search, look);
    if onboarding.offers.is_empty() && notes.is_empty() {
        return;
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        "Suggested: projects found elsewhere in your home folder",
        look.style(Role::Title),
    ));
    let first = onboarding.roots.len() + 1;
    for (index, offer) in onboarding.offers.iter().enumerate() {
        let mark = if offer.checked { "[x]" } else { "[ ]" };
        let text = format!("{mark} {}", describe(&offer.suggestion, look));
        lines.push(choice(look, onboarding.root_cursor == first + index, &text));
    }
    lines.extend(
        notes
            .into_iter()
            .map(|note| Line::styled(note, look.style(Role::Muted))),
    );
}

fn method_lines(onboarding: &Onboarding, look: Look<'_>) -> Vec<Line<'static>> {
    let mut lines = question(
        "Delete or move to Trash by default?",
        "This version only stores the answer: nothing is removed yet.",
        look,
    );
    for &method in Method::ALL {
        let about = match method {
            Method::Delete => "frees the space at once",
            Method::Trash => "can be put back, but frees nothing until the Trash is emptied",
        };
        lines.push(choice(
            look,
            method == onboarding.method,
            &format!("{:<8}{about}", title_case(method.name())),
        ));
    }
    lines
}

fn tip_lines(app: &App, look: Look<'_>) -> Vec<Line<'static>> {
    let mut lines = question(
        "Prevention tips",
        "Settings of your tools that keep less around. kipple shows them and never changes them.",
        look,
    );
    let tips: Vec<_> = app.advice.values().flatten().collect();
    if tips.is_empty() {
        let none = match app.progress {
            Progress::Scanning => "Still scanning: tips appear once the tools are found.",
            Progress::Done { .. } => "kipple knows no retention setting for the tools it found.",
        };
        lines.push(Line::from(none));
    }
    for tip in tips {
        lines.push(Line::styled(escaped(&tip.title), look.style(Role::Title)));
        lines.push(Line::from(escaped(&tip.text)));
        for line in tip.snippets.iter().flat_map(|s| human::snippet_lines(s)) {
            lines.push(Line::styled(format!("  {line}"), look.style(Role::Muted)));
        }
        lines.push(Line::from(""));
    }
    lines
}

fn keys(onboarding: &Onboarding, look: Look<'_>) -> Paragraph<'static> {
    let mut text = String::from(" ");
    if onboarding.typing.is_some() {
        text.push_str("type a path, enter adds it, esc cancels");
    } else {
        let hint = |actions: &[Action], between: &str, what: &str| {
            onboarding
                .keymap
                .hint(Mode::Onboarding, actions, between, what)
        };
        let choose = match onboarding.step {
            Step::Tips => "finish",
            Step::Level | Step::Roots | Step::Method => "choose",
        };
        let mut hints = vec![hint(&[Action::Choose], "", choose)];
        if onboarding.step == Step::Roots {
            hints.push(hint(&[Action::Check], "", "check"));
        }
        if onboarding.step == Step::Tips {
            hints.push(hint(&[Action::MoveDown, Action::MoveUp], " ", "scroll"));
        }
        hints.push(hint(&[Action::Skip], "", "skip"));
        if onboarding.step != Step::Level {
            hints.push(Some("esc back".to_owned()));
        }
        hints.push(hint(&[Action::Quit], "", "quit, saving nothing"));
        let hints: Vec<String> = hints.into_iter().flatten().collect();
        text.push_str(&hints.join(look.glyphs.dot()));
    }
    Paragraph::new(Line::styled(
        look.glyphs.keys(&text),
        look.style(Role::KeyLabel),
    ))
}
