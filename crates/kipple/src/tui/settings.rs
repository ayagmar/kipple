//! The settings panel (`,`, `docs/03-functional-spec.md` §2 J2 and §5): the level, the
//! project roots, the default method, the kept items and the rules turned off. Each change
//! is a config edit the loop writes; nothing here changes the screen before that write
//! succeeded.

use std::path::PathBuf;

use kipple_core::{Level, Method, RuleId};

use super::app::{App, Key};
use super::view::title_case;
use crate::config::Edit;

/// One line of the panel the cursor can stand on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    Level,
    /// A project root, by its index in [`App::roots`].
    Root(usize),
    /// Where another root is typed.
    AddRoot,
    Method,
    /// A kept path, by its index in the config's `keep.paths`.
    Kept(usize),
    /// A rule turned off.
    Disabled(RuleId),
}

/// What a key in the panel asks of the home screen.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Nothing,
    Close,
    /// Write these edits; once they are written, say `done`.
    Save {
        edits: Vec<Edit>,
        done: String,
    },
    Say(String),
}

/// Where the cursor is in the panel, and the root being typed, if any.
#[derive(Debug, Default)]
pub(crate) struct Panel {
    pub(crate) cursor: usize,
    pub(crate) typing: Option<String>,
}

/// The panel's lines, in order: the level, each root and "add another", the method, each
/// kept path and each rule turned off.
pub(crate) fn items(app: &App) -> Vec<Item> {
    let mut items = vec![Item::Level];
    items.extend((0..app.roots.len()).map(Item::Root));
    items.push(Item::AddRoot);
    items.push(Item::Method);
    items.extend((0..app.config.keep.len()).map(Item::Kept));
    items.extend(disabled(app).into_iter().map(Item::Disabled));
    items
}

/// The rules turned off, by `exclude.rules` or `rules.<id>.enabled = false`.
pub(crate) fn disabled(app: &App) -> Vec<RuleId> {
    app.policy
        .rules
        .iter()
        .filter(|(_, settings)| settings.disabled)
        .map(|(rule, _)| rule.clone())
        .collect()
}

/// The method stored in config, or the default.
pub(crate) fn method(app: &App) -> Method {
    app.config.method.unwrap_or(Method::Delete)
}

impl Panel {
    /// The item the cursor is on.
    pub(crate) fn focus(&self, items: &[Item]) -> Option<Item> {
        items
            .get(self.cursor.min(items.len().saturating_sub(1)))
            .cloned()
    }

    /// Applies `key` to the panel over `app`'s settings.
    pub(crate) fn key(&mut self, key: Key, app: &App) -> Action {
        if self.typing.is_some() {
            return self.type_root(key, app);
        }
        let items = items(app);
        self.cursor = self.cursor.min(items.len().saturating_sub(1));
        match key {
            Key::Esc | Key::Char(',' | 'q') => Action::Close,
            Key::Char('j') | Key::Down => self.step(1, items.len()),
            Key::Char('k') | Key::Up => self.step(-1, items.len()),
            Key::Char('g') | Key::Home => self.step(isize::MIN, items.len()),
            Key::Char('G') | Key::End => self.step(isize::MAX, items.len()),
            Key::Char('h') | Key::Left => self.change(app, &items, -1),
            Key::Char('l' | ' ') | Key::Right | Key::Enter => self.change(app, &items, 1),
            Key::Char('d' | 'x') | Key::Backspace => self.remove(app, &items),
            Key::Char(_) | Key::PageUp | Key::PageDown | Key::Tab | Key::Interrupt => {
                Action::Nothing
            }
        }
    }

    fn step(&mut self, by: isize, len: usize) -> Action {
        let last = len.saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(by).min(last);
        Action::Nothing
    }

    /// Changes the focused value: the level and the method step through their values,
    /// and "add another" starts typing a root.
    fn change(&mut self, app: &App, items: &[Item], by: isize) -> Action {
        match self.focus(items) {
            Some(Item::Level) => {
                let level = step_level(app.policy.level, by);
                if level == app.policy.level {
                    return Action::Nothing;
                }
                Action::Save {
                    edits: vec![Edit::Level(level)],
                    done: format!("Level set to {}.", title_case(level.name())),
                }
            }
            Some(Item::Method) => {
                let method = match method(app) {
                    Method::Delete => Method::Trash,
                    Method::Trash => Method::Delete,
                };
                Action::Save {
                    edits: vec![Edit::Method(method)],
                    done: format!(
                        "Default method set to {}. It is only stored: nothing is removed yet.",
                        method.name()
                    ),
                }
            }
            Some(Item::AddRoot) => {
                self.typing = Some(String::new());
                Action::Nothing
            }
            Some(Item::Root(_) | Item::Kept(_) | Item::Disabled(_)) => {
                Action::Say("d removes it.".to_owned())
            }
            None => Action::Nothing,
        }
    }

    /// Removes the focused root, kept path or turned-off rule.
    fn remove(&self, app: &App, items: &[Item]) -> Action {
        match self.focus(items) {
            Some(Item::Root(index)) => {
                let mut roots = app.roots.clone();
                if index >= roots.len() {
                    return Action::Nothing;
                }
                let gone = roots.remove(index);
                Action::Save {
                    edits: vec![Edit::ProjectRoots(roots)],
                    done: format!("Removed {} from the project roots.", gone.display()),
                }
            }
            Some(Item::Kept(index)) => unkeep(app, index),
            Some(Item::Disabled(rule)) => Action::Save {
                done: format!("Turned {} back on.", app.catalog.title(&rule)),
                edits: vec![Edit::RuleEnabled {
                    rule,
                    enabled: true,
                }],
            },
            Some(Item::Level | Item::Method | Item::AddRoot) | None => Action::Nothing,
        }
    }

    fn type_root(&mut self, key: Key, app: &App) -> Action {
        let Some(typed) = &mut self.typing else {
            return Action::Nothing;
        };
        match key {
            Key::Char(c) => typed.push(c),
            Key::Backspace => {
                typed.pop();
            }
            Key::Esc => self.typing = None,
            Key::Enter => {
                let path = PathBuf::from(typed.trim());
                self.typing = None;
                if path.as_os_str().is_empty() {
                    return Action::Nothing;
                }
                let mut roots = app.roots.clone();
                roots.push(path.clone());
                return Action::Save {
                    edits: vec![Edit::ProjectRoots(roots)],
                    done: format!("Added {} to the project roots.", path.display()),
                };
            }
            Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::Tab
            | Key::Interrupt => {}
        }
        Action::Nothing
    }
}

/// Undoes the kept path at `index`, by the path it expands to.
fn unkeep(app: &App, index: usize) -> Action {
    let written = app.config.keep.get(index).map(|(path, _)| path.clone());
    let expanded = app
        .policy
        .keep
        .get(index)
        .and_then(|held| held.forms.first());
    match (written, expanded) {
        (Some(written), Some(path)) => Action::Save {
            edits: vec![Edit::Unkeep(path.clone())],
            done: format!("{written} is no longer kept."),
        },
        (Some(written), None) => Action::Say(format!(
            "{written} starts with ~ and the home directory is unknown, so it can only be removed in the config file."
        )),
        (None, _) => Action::Nothing,
    }
}

/// The level `by` steps from `level`, stopping at either end.
fn step_level(level: Level, by: isize) -> Level {
    let at = Level::ALL.iter().position(|&l| l == level).unwrap_or(0);
    let to = at.saturating_add_signed(by).min(Level::ALL.len() - 1);
    Level::ALL.get(to).copied().unwrap_or(level)
}
