//! The settings panel: every line says its value in words, so the cursor and the
//! current level read without colour.

use std::path::Path;

use kipple_core::{Level, Suggestion};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::text::{Line as Text, Span};
use ratatui::widgets::Paragraph;

use super::tree::offset;
use super::{Look, level_about, title_case};
use crate::human::{escaped, homed};
use crate::theme::{Colours, Role};
use crate::tui::app::App;
use crate::tui::keymap::{Action, Mode};
use crate::tui::settings::{self, Item, Search};

/// The settings panel in place of the tree and the detail pane. Every line says its
/// value in words, so the cursor and the current level read without colour.
pub(super) fn render(app: &App, look: Look<'_>, area: Rect, frame: &mut Frame<'_>) {
    let items = settings::items(app);
    let mut panel = PanelLines {
        look,
        focus: app.panel.focus(&items),
        lines: Vec::new(),
        at: 0,
    };
    settings_head(app, look, &mut panel);
    settings_lists(app, look, &mut panel);
    let area = area.inner(Margin::new(1, 0));
    let [head, list] = Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(Text::styled("Settings", look.style(Role::Title))),
        head,
    );
    let rows = usize::from(list.height);
    let first = offset(0, panel.at, rows, panel.lines.len());
    let shown: Vec<Text<'static>> = panel.lines.into_iter().skip(first).collect();
    frame.render_widget(Paragraph::new(shown), list);
}

/// The settings panel's lines as they are built, and which one the cursor is on.
struct PanelLines<'a> {
    look: Look<'a>,
    focus: Option<Item>,
    lines: Vec<Text<'static>>,
    at: usize,
}

impl PanelLines<'_> {
    /// A line with `label` in the first column; the cursor's when it is `item` and
    /// focused.
    fn push(&mut self, item: Option<&Item>, label: &str, value: Vec<Span<'static>>) {
        let mut spans = vec![Span::styled(
            format!("{label:<17} "),
            self.look.style(Role::Label),
        )];
        spans.extend(value);
        let mut line = Text::from(spans);
        if item.is_some() && item == self.focus.as_ref() {
            line = self.look.cursor(line);
            self.at = self.lines.len();
        }
        self.lines.push(line);
    }
}

/// The config file, the level, the project roots and the method.
fn settings_head(app: &App, look: Look<'_>, panel: &mut PanelLines<'_>) {
    let file = app.file.as_ref().map_or_else(
        || "unknown, so nothing can be saved".to_owned(),
        |file| escaped(&homed(file, app.home.as_deref())),
    );
    panel.push(
        None,
        "Config file",
        vec![Span::styled(file, look.style(Role::Muted))],
    );
    panel.push(None, "", Vec::new());
    panel.push(Some(&Item::Level), "Level", level_spans(app, look));
    let about = Span::styled(level_about(app.policy.level), look.style(Role::Muted));
    panel.push(None, "", vec![about]);
    panel.push(Some(&Item::Theme), "Theme", theme_spans(app, look));
    for (index, root) in app.roots.iter().enumerate() {
        let label = if index == 0 { "Project roots" } else { "" };
        let root = escaped(&homed(root, app.home.as_deref()));
        panel.push(Some(&Item::Root(index)), label, vec![Span::raw(root)]);
    }
    let add = if app.roots.is_empty() {
        "Project roots"
    } else {
        ""
    };
    panel.push(Some(&Item::AddRoot), add, vec![Span::raw("+ add another")]);
    for (index, suggestion) in app.search.suggestions().iter().enumerate() {
        let label = if index == 0 { "Suggested roots" } else { "" };
        let line = vec![Span::raw(describe(suggestion, app.home.as_deref(), look))];
        panel.push(Some(&Item::Suggested(index)), label, line);
    }
    for (index, note) in search_notes(&app.search, look).into_iter().enumerate() {
        let label = if index == 0 && app.search.suggestions().is_empty() {
            "Suggested roots"
        } else {
            ""
        };
        panel.push(
            None,
            label,
            vec![Span::styled(note, look.style(Role::Muted))],
        );
    }
    let method = vec![
        Span::raw(settings::method(app).name()),
        Span::styled(
            "  stored only: nothing is removed yet",
            look.style(Role::Muted),
        ),
    ];
    panel.push(Some(&Item::Method), "Default method", method);
}

/// The kept paths and the rules turned off, each undone with `d`.
fn settings_lists(app: &App, look: Look<'_>, panel: &mut PanelLines<'_>) {
    for (index, (path, note)) in app.config.keep.iter().enumerate() {
        let label = if index == 0 { "Kept" } else { "" };
        let mut value = vec![Span::raw(escaped(path))];
        if let Some(note) = note {
            value.push(Span::styled(
                format!("  {}", escaped(note)),
                look.style(Role::Muted),
            ));
        }
        panel.push(Some(&Item::Kept(index)), label, value);
    }
    if app.config.keep.is_empty() {
        let none = app.keymap().first(Mode::Browse, Action::Keep).map_or_else(
            || "nothing yet".to_owned(),
            |key| format!("nothing yet: {key} keeps a finding"),
        );
        let none = Span::styled(none, look.style(Role::Muted));
        panel.push(None, "Kept", vec![none]);
    }
    let disabled = settings::disabled(app);
    for (index, rule) in disabled.iter().enumerate() {
        let label = if index == 0 { "Rules turned off" } else { "" };
        let value = vec![
            Span::raw(app.catalog.title(rule).to_owned()),
            Span::styled(format!("  {rule}"), look.style(Role::Muted)),
        ];
        panel.push(Some(&Item::Disabled(rule.clone())), label, value);
    }
    if disabled.is_empty() {
        let none = app
            .keymap()
            .first(Mode::Browse, Action::KeepRule)
            .map_or_else(
                || "none".to_owned(),
                |key| format!("none: {key} turns a finding's rule off"),
            );
        let none = Span::styled(none, look.style(Role::Muted));
        panel.push(None, "Rules turned off", vec![none]);
    }
}

/// The theme shown, which of how many, and where it comes from: a preview says so in
/// words, and a theme that can't be used says why.
fn theme_spans(app: &App, look: Look<'_>) -> Vec<Span<'static>> {
    let Some(choice) = app.themes.shown() else {
        return Vec::new();
    };
    let (at, count) = app.themes.place();
    let about = match &choice.drawn {
        Err(error) => format!("{at} of {count}, can't be used: {}", escaped(error)),
        Ok(_) if app.themes.previewing() => {
            let keep = app
                .keymap()
                .first(Mode::Settings, Action::Choose)
                .map_or_else(String::new, |key| format!("{key} keeps it, "));
            format!("previewing {at} of {count}: {keep}esc goes back")
        }
        Ok(_) => format!("{at} of {count}, {}", escaped(&choice.origin.to_string())),
    };
    let off = if app.themes.colours == Colours::Off {
        "; colour is off, so every theme draws without it"
    } else {
        ""
    };
    vec![
        Span::raw(choice.name.clone()),
        Span::styled(format!("  {about}{off}"), look.style(Role::Muted)),
    ]
}

/// The three levels, the current one in brackets.
fn level_spans(app: &App, look: Look<'_>) -> Vec<Span<'static>> {
    Level::ALL
        .iter()
        .map(|&level| {
            let name = title_case(level.name());
            if level == app.policy.level {
                Span::styled(format!("[{name}] "), look.style(Role::LevelCurrent))
            } else {
                Span::styled(format!(" {name}  "), look.style(Role::LevelOther))
            }
        })
        .collect()
}

/// A suggestion as a line: its path, how many projects it holds and the first names.
pub(crate) fn describe(suggestion: &Suggestion, home: Option<&Path>, look: Look<'_>) -> String {
    const SHOWN: usize = 3;
    let names: Vec<String> = suggestion
        .projects
        .iter()
        .take(SHOWN)
        .map(|project| {
            // A project suggested as itself is named by its last component.
            let name = match project.strip_prefix(&suggestion.path) {
                Ok(inside) if !inside.as_os_str().is_empty() => inside,
                Ok(_) | Err(_) => project.file_name().map_or(project.as_path(), Path::new),
            };
            escaped(&name.display().to_string())
        })
        .collect();
    let count = suggestion.projects.len();
    let more = if count > SHOWN {
        look.glyphs.ellipsis(", ")
    } else {
        String::new()
    };
    let projects = if count == 1 { "project" } else { "projects" };
    format!(
        "{} ({count} {projects}: {}{more})",
        escaped(&homed(&suggestion.path, home)),
        names.join(", ")
    )
}

/// What to say about the search beside its suggestions: that it runs, that it stopped
/// early, what it couldn't read, or that it found nothing.
pub(crate) fn search_notes(search: &Search, look: Look<'_>) -> Vec<String> {
    let found = match search {
        Search::NotStarted => return Vec::new(),
        Search::Running => return vec![look.glyphs.ellipsis("looking for projects")],
        Search::Done(found) => found,
    };
    let mut notes = Vec::new();
    if found.suggestions.is_empty() {
        notes.push("no projects found outside these folders".to_owned());
    }
    if !found.complete {
        notes.push("stopped early: the list may be incomplete".to_owned());
    }
    if found.unreadable > 0 {
        let folders = if found.unreadable == 1 {
            "folder"
        } else {
            "folders"
        };
        notes.push(format!("{} {folders} couldn't be read", found.unreadable));
    }
    notes
}
