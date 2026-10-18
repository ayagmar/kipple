//! Which key does what (D-056). Every key the home screen, settings and the first run
//! act on maps to an [`Action`] in a [`Mode`]; `[keys]` in the config rebinds an action
//! by its name. Help and the key bars are written from the keymap, so they can't
//! disagree with it. Typed text (the filter, a note, a root) is never rebound, Ctrl-C
//! always quits, and esc always goes back or closes.

use super::app::Key;

/// Where a key is read. Help has none: any key closes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The tree.
    Browse,
    /// The settings panel.
    Settings,
    /// The first run's questions.
    Onboarding,
    /// The filter being typed: only the keys that type nothing move the cursor.
    FilterNav,
}

impl Mode {
    const fn name(self) -> &'static str {
        match self {
            Self::Browse => "the tree",
            Self::Settings => "settings",
            Self::Onboarding => "the first run",
            Self::FilterNav => "the filter",
        }
    }

    /// Whether `key` keeps its fixed meaning here, whatever `[keys]` says: esc always
    /// goes back, and in the filter every key that types or edits the text types.
    const fn fixed(self, key: Key) -> bool {
        matches!(
            (self, key),
            (_, Key::Esc) | (Self::FilterNav, Key::Char(_) | Key::Enter | Key::Backspace)
        )
    }
}

const MOVE: &[Mode] = &[
    Mode::Browse,
    Mode::Settings,
    Mode::Onboarding,
    Mode::FilterNav,
];
const BROWSE: &[Mode] = &[Mode::Browse];
const BROWSE_SETTINGS: &[Mode] = &[Mode::Browse, Mode::Settings];
const EVERY_SCREEN: &[Mode] = &[Mode::Browse, Mode::Settings, Mode::Onboarding];
const SETTINGS: &[Mode] = &[Mode::Settings];
const SETTINGS_ONBOARDING: &[Mode] = &[Mode::Settings, Mode::Onboarding];
const ONBOARDING: &[Mode] = &[Mode::Onboarding];

/// Something a key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    MoveDown,
    MoveUp,
    PageDown,
    PageUp,
    First,
    Last,
    Fold,
    Unfold,
    Filter,
    Lens,
    Select,
    SelectGroup,
    SelectSafe,
    Keep,
    KeepRule,
    Sort,
    DetailDown,
    DetailUp,
    Copy,
    Settings,
    Review,
    Help,
    Quit,
    /// Settings: the previous level or method.
    ChangeBack,
    /// Settings: the next level or method, or type a root on "add another".
    Change,
    /// Settings and the first run: change or add; answer the step.
    Choose,
    Remove,
    /// The first run: check or uncheck a root.
    Check,
    /// The first run: skip the step.
    Skip,
}

impl Action {
    pub(crate) const ALL: [Self; 29] = [
        Self::MoveDown,
        Self::MoveUp,
        Self::PageDown,
        Self::PageUp,
        Self::First,
        Self::Last,
        Self::Fold,
        Self::Unfold,
        Self::Filter,
        Self::Lens,
        Self::Select,
        Self::SelectGroup,
        Self::SelectSafe,
        Self::Keep,
        Self::KeepRule,
        Self::Sort,
        Self::DetailDown,
        Self::DetailUp,
        Self::Copy,
        Self::Settings,
        Self::Review,
        Self::Help,
        Self::Quit,
        Self::ChangeBack,
        Self::Change,
        Self::Choose,
        Self::Remove,
        Self::Check,
        Self::Skip,
    ];

    /// Its name in `[keys]`.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::MoveDown => "move-down",
            Self::MoveUp => "move-up",
            Self::PageDown => "page-down",
            Self::PageUp => "page-up",
            Self::First => "first",
            Self::Last => "last",
            Self::Fold => "fold",
            Self::Unfold => "unfold",
            Self::Filter => "filter",
            Self::Lens => "lens",
            Self::Select => "select",
            Self::SelectGroup => "select-group",
            Self::SelectSafe => "select-safe",
            Self::Keep => "keep",
            Self::KeepRule => "keep-rule",
            Self::Sort => "sort",
            Self::DetailDown => "detail-down",
            Self::DetailUp => "detail-up",
            Self::Copy => "copy",
            Self::Settings => "settings",
            Self::Review => "review",
            Self::Help => "help",
            Self::Quit => "quit",
            Self::ChangeBack => "change-back",
            Self::Change => "change",
            Self::Choose => "choose",
            Self::Remove => "remove",
            Self::Check => "check",
            Self::Skip => "skip",
        }
    }

    /// The action named `name` in `[keys]`.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.name() == name)
    }

    /// Where it is read.
    const fn modes(self) -> &'static [Mode] {
        match self {
            Self::MoveDown | Self::MoveUp => MOVE,
            Self::First | Self::Last | Self::Settings => BROWSE_SETTINGS,
            Self::Quit => EVERY_SCREEN,
            Self::ChangeBack | Self::Change | Self::Remove => SETTINGS,
            Self::Choose => SETTINGS_ONBOARDING,
            Self::Check | Self::Skip => ONBOARDING,
            Self::PageDown
            | Self::PageUp
            | Self::Fold
            | Self::Unfold
            | Self::Filter
            | Self::Lens
            | Self::Select
            | Self::SelectGroup
            | Self::SelectSafe
            | Self::Keep
            | Self::KeepRule
            | Self::Sort
            | Self::DetailDown
            | Self::DetailUp
            | Self::Copy
            | Self::Review
            | Self::Help => BROWSE,
        }
    }

    /// Its keys when `[keys]` doesn't name it.
    const fn defaults(self) -> &'static [Key] {
        match self {
            Self::MoveDown => &[Key::Char('j'), Key::Down],
            Self::MoveUp => &[Key::Char('k'), Key::Up],
            Self::PageDown => &[Key::PageDown],
            Self::PageUp => &[Key::PageUp],
            Self::First => &[Key::Char('g'), Key::Home],
            Self::Last => &[Key::Char('G'), Key::End],
            Self::Fold | Self::ChangeBack => &[Key::Char('h'), Key::Left],
            Self::Unfold => &[Key::Char('l'), Key::Right],
            Self::Filter => &[Key::Char('/')],
            Self::Lens | Self::Skip => &[Key::Tab],
            Self::Select => &[Key::Char('s')],
            Self::SelectGroup => &[Key::Char('a')],
            Self::SelectSafe => &[Key::Char('c')],
            Self::Keep => &[Key::Char('p')],
            Self::KeepRule => &[Key::Char('P')],
            Self::Sort => &[Key::Char('o')],
            Self::DetailDown => &[Key::Char('J')],
            Self::DetailUp => &[Key::Char('K')],
            Self::Copy => &[Key::Char('y')],
            Self::Settings => &[Key::Char(',')],
            Self::Review | Self::Choose => &[Key::Enter],
            Self::Help => &[Key::Char('?')],
            Self::Quit => &[Key::Char('q')],
            Self::Change => &[Key::Char('l'), Key::Right, Key::Char(' ')],
            Self::Remove => &[Key::Char('d'), Key::Char('x'), Key::Backspace],
            Self::Check => &[Key::Char(' ')],
        }
    }

    /// Whether it may be left with no key: without the others, part of the screen
    /// couldn't be reached.
    const fn optional(self) -> bool {
        !matches!(
            self,
            Self::MoveDown
                | Self::MoveUp
                | Self::Filter
                | Self::Select
                | Self::Settings
                | Self::Help
                | Self::Quit
                | Self::Change
                | Self::Choose
                | Self::Remove
        )
    }
}

/// The key named `name` in `[keys]`: one printable character, case counting (`S` is
/// shift-s), or a key's name. There are no modifiers, so Ctrl-C can't be named.
pub(crate) fn key_named(name: &str) -> Option<Key> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && !c.is_control()
        && !c.is_whitespace()
    {
        return Some(Key::Char(c));
    }
    let key = match name {
        "space" => Key::Char(' '),
        "enter" => Key::Enter,
        "esc" => Key::Esc,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        _ => return None,
    };
    Some(key)
}

/// `key` as help and the key bars write it, arrows as arrows.
pub(crate) fn shown(key: Key) -> String {
    let name = match key {
        Key::Char(' ') => "space",
        Key::Char(c) => return c.to_string(),
        Key::Up => "↑",
        Key::Down => "↓",
        Key::Left => "←",
        Key::Right => "→",
        Key::Home => "home",
        Key::End => "end",
        Key::PageUp => "pageup",
        Key::PageDown => "pagedown",
        Key::Enter => "enter",
        Key::Esc => "esc",
        Key::Backspace => "backspace",
        Key::Tab => "tab",
        Key::Interrupt => "ctrl-c",
    };
    name.to_owned()
}

/// Why `[keys]` can't be used: which entry, which of its keys if one, and what is wrong.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Conflict {
    pub(crate) entry: usize,
    pub(crate) key: Option<usize>,
    pub(crate) message: String,
}

/// An action's keys in one mode.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Binding {
    mode: Mode,
    action: Action,
    keys: Vec<Key>,
}

/// Every action's keys in every mode it is read in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Keymap {
    bindings: Vec<Binding>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::build(&[])
    }
}

impl Keymap {
    /// The keymap `user` gives, each entry an action and the keys that replace its
    /// defaults. A key a user entry takes is removed from the defaults of every other
    /// action in the same mode; a fixed key is left out. [`Keymap::check`] tells whether
    /// `user` is usable.
    pub(crate) fn build(user: &[(Action, Vec<Key>)]) -> Self {
        let mut bindings = Vec::new();
        for action in Action::ALL {
            let mine = user.iter().find(|(a, _)| *a == action);
            for &mode in action.modes() {
                let keys = match mine {
                    Some((_, keys)) => keys.clone(),
                    None => action
                        .defaults()
                        .iter()
                        .copied()
                        .filter(|&key| taker(user, mode, key).is_none())
                        .collect(),
                };
                let keys = keys.into_iter().filter(|&key| !mode.fixed(key)).collect();
                bindings.push(Binding { mode, action, keys });
            }
        }
        Self { bindings }
    }

    /// Whether `user` is usable: no entry binds esc, no two entries take one key in one
    /// mode, and every action that needs a key keeps one.
    pub(crate) fn check(user: &[(Action, Vec<Key>)]) -> Result<(), Conflict> {
        let keyed = user.iter().enumerate().flat_map(|(entry, (action, keys))| {
            keys.iter()
                .enumerate()
                .map(move |(at, &key)| (entry, *action, at, key))
        });
        for (entry, action, at, key) in keyed {
            if let Some(message) = refused(user.get(..entry).unwrap_or(&[]), action, key) {
                return Err(Conflict {
                    entry,
                    key: Some(at),
                    message,
                });
            }
        }
        Self::build(user).every_needed_key(user)
    }

    /// An action that needs a key and has none, blamed on the entry that left it so.
    fn every_needed_key(&self, user: &[(Action, Vec<Key>)]) -> Result<(), Conflict> {
        let bare = self.bindings.iter().find(|binding| {
            !binding.action.optional() && binding.mode != Mode::FilterNav && binding.keys.is_empty()
        });
        let Some(&Binding { mode, action, .. }) = bare else {
            return Ok(());
        };
        let own = user.iter().position(|(a, _)| *a == action);
        let took = || {
            action
                .defaults()
                .iter()
                .filter_map(|&key| taker(user, mode, key))
                .max()
        };
        let entry = own.or_else(took).unwrap_or(0);
        let message = if own.is_some() {
            format!("{} needs a key", action.name())
        } else {
            format!(
                "this leaves {} with no key in {}",
                action.name(),
                mode.name()
            )
        };
        Err(Conflict {
            entry,
            key: None,
            message,
        })
    }

    /// The action `key` does in `mode`, if any.
    pub(crate) fn action(&self, mode: Mode, key: Key) -> Option<Action> {
        self.bindings
            .iter()
            .find(|binding| binding.mode == mode && binding.keys.contains(&key))
            .map(|binding| binding.action)
    }

    /// The keys of `action` in `mode`, in the order they were given.
    pub(crate) fn keys(&self, mode: Mode, action: Action) -> &[Key] {
        self.bindings
            .iter()
            .find(|binding| binding.mode == mode && binding.action == action)
            .map_or(&[], |binding| &binding.keys)
    }

    /// The first key of `action` in `mode`, as shown, if it has one.
    pub(crate) fn first(&self, mode: Mode, action: Action) -> Option<String> {
        self.keys(mode, action).first().map(|&key| shown(key))
    }

    /// `what`, after the first key of each of `actions` that has one, joined by `between`
    /// (`j/k move`); nothing when none has a key.
    pub(crate) fn hint(
        &self,
        mode: Mode,
        actions: &[Action],
        between: &str,
        what: &str,
    ) -> Option<String> {
        let keys: Vec<String> = actions
            .iter()
            .filter_map(|&action| self.first(mode, action))
            .collect();
        (!keys.is_empty()).then(|| format!("{} {what}", keys.join(between)))
    }

    /// The tree's key bar.
    pub(crate) fn browse_bar(&self) -> String {
        self.bar(Mode::Browse, BROWSE_BAR).join("  ")
    }

    /// The settings panel's key bar.
    pub(crate) fn settings_bar(&self) -> String {
        let mut hints = self.bar(Mode::Settings, SETTINGS_BAR);
        hints.push("esc back".to_owned());
        hints.join("  ")
    }

    fn bar(&self, mode: Mode, hints: &[(&[Action], &str)]) -> Vec<String> {
        hints
            .iter()
            .filter_map(|(actions, what)| self.hint(mode, actions, "/", what))
            .collect()
    }

    /// Help's lines: the keys of each, then what they do. The keyboard's own home, end
    /// and page keys are listed only for an action that has no other key.
    pub(crate) fn help(&self) -> Vec<(String, &'static str)> {
        HELP.iter()
            .filter_map(|(actions, what)| {
                let keys: Vec<Vec<Key>> = actions
                    .iter()
                    .map(|&action| listed(self.keys(Mode::Browse, action)))
                    .collect();
                let widest = keys.iter().map(Vec::len).max().unwrap_or(0);
                let columns: Vec<String> = (0..widest)
                    .map(|at| {
                        let column: Vec<String> = keys
                            .iter()
                            .filter_map(|keys| keys.get(at).map(|&key| shown(key)))
                            .collect();
                        column.join(" ")
                    })
                    .collect();
                (!columns.is_empty()).then(|| (columns.join("  "), *what))
            })
            .collect()
    }
}

/// The tree's key bar: the first key of each, then what it does.
const BROWSE_BAR: &[(&[Action], &str)] = &[
    (&[Action::MoveDown, Action::MoveUp], "move"),
    (&[Action::Filter], "filter"),
    (&[Action::Select], "select"),
    (&[Action::SelectSafe], "safe"),
    (&[Action::Keep], "keep"),
    (&[Action::Settings], "settings"),
    (&[Action::Help], "help"),
    (&[Action::Quit], "quit"),
];

/// The settings panel's key bar, before `esc back`.
const SETTINGS_BAR: &[(&[Action], &str)] = &[
    (&[Action::MoveDown, Action::MoveUp], "move"),
    (&[Action::ChangeBack, Action::Change], "change"),
    (&[Action::Choose], "change or add"),
    (&[Action::Remove], "remove"),
];

/// Help, line by line.
const HELP: &[(&[Action], &str)] = &[
    (&[Action::MoveDown, Action::MoveUp], "move"),
    (
        &[Action::Fold, Action::Unfold],
        "fold and unfold a group or project",
    ),
    (&[Action::First, Action::Last], "first and last line"),
    (
        &[Action::Filter],
        "filter by words of the path or rule; enter keeps it, esc clears it",
    ),
    (
        &[Action::Lens],
        "show one lens: Agents, Dev, System, then all",
    ),
    (&[Action::Select], "select or unselect the finding"),
    (
        &[Action::SelectGroup],
        "add the group's or project's Safe and Review findings",
    ),
    (&[Action::SelectSafe], "select the whole Safe bucket"),
    (
        &[Action::Keep],
        "keep the finding for good, with an optional note",
    ),
    (
        &[Action::KeepRule],
        "keep every finding of its rule: turn the rule off",
    ),
    (&[Action::Sort], "sort by size, last change or name"),
    (
        &[Action::DetailDown, Action::DetailUp],
        "scroll the detail pane",
    ),
    (&[Action::Copy], "send the path to the terminal clipboard"),
    (
        &[Action::Settings],
        "settings: level, project roots, method, what is kept",
    ),
    (&[Action::Review], "show what is selected"),
    (&[Action::Help], "this help"),
    (&[Action::Quit], "quit"),
];

/// `keys` without the home, end and page keys, unless they are all there is.
fn listed(keys: &[Key]) -> Vec<Key> {
    let own = |key: &&Key| !matches!(key, Key::Home | Key::End | Key::PageUp | Key::PageDown);
    let others: Vec<Key> = keys.iter().filter(own).copied().collect();
    if others.is_empty() {
        keys.to_vec()
    } else {
        others
    }
}

/// Why `action` can't have `key`, if it can't: esc is fixed, or an entry of `earlier`
/// takes it where `action` is read too.
fn refused(earlier: &[(Action, Vec<Key>)], action: Action, key: Key) -> Option<String> {
    if key == Key::Esc {
        return Some("esc always goes back or closes; it can't be bound".to_owned());
    }
    let (other, mode) = claimed(earlier, action, key)?;
    Some(format!(
        "{} is bound to {} as well in {}",
        quoted(key),
        other.name(),
        mode.name()
    ))
}

/// The first user entry that takes `key` in `mode`, by its index.
fn taker(user: &[(Action, Vec<Key>)], mode: Mode, key: Key) -> Option<usize> {
    user.iter()
        .position(|(action, keys)| action.modes().contains(&mode) && keys.contains(&key))
}

/// An entry of `earlier` that takes `key` in a mode `action` is read in too, and that
/// mode. In the filter, keys that type don't count: they type there.
fn claimed(earlier: &[(Action, Vec<Key>)], action: Action, key: Key) -> Option<(Action, Mode)> {
    earlier.iter().find_map(|(other, keys)| {
        let mode = action.modes().iter().find(|&&mode| {
            !mode.fixed(key) && other.modes().contains(&mode) && keys.contains(&key)
        })?;
        Some((*other, *mode))
    })
}

fn quoted(key: Key) -> String {
    match key {
        Key::Char(' ') => "space".to_owned(),
        Key::Char(c) => format!("\"{c}\""),
        Key::Up => "up".to_owned(),
        Key::Down => "down".to_owned(),
        Key::Left => "left".to_owned(),
        Key::Right => "right".to_owned(),
        Key::Home
        | Key::End
        | Key::PageUp
        | Key::PageDown
        | Key::Enter
        | Key::Esc
        | Key::Backspace
        | Key::Tab
        | Key::Interrupt => shown(key),
    }
}
