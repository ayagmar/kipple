//! Keeping findings and changing settings (`docs/03-functional-spec.md` §2 J2): `p`
//! keeps the focused finding, `P` its rule, and the settings panel changes the rest. Each
//! change is an [`Effect`] the loop carries out by writing the config file; the buckets
//! change only when the loop answers that the write succeeded, and then from what was
//! written.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use kipple_core::{FindingId, Lineage, Policy, SymbolicRoot};

use super::{App, Key, Mode, Node, Progress, Row, Shelf};
use crate::config::{self, Config, Edit, Settings};
use crate::human::{escaped, findings};

/// What the home screen asks of the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Effect {
    /// Write these edits to the config file in one go, then answer with
    /// [`Msg::Saved`](super::Msg::Saved) or [`Msg::NotSaved`](super::Msg::NotSaved).
    Write(Vec<Edit>),
    /// Scan again, with the project roots the config now names.
    Rescan,
}

/// What the config file says, with the policy and project roots it gives.
#[derive(Debug, Clone)]
pub(crate) struct Saved {
    /// The config file, when its directory is known.
    pub(crate) file: Option<PathBuf>,
    pub(crate) config: Config,
    pub(crate) policy: Policy,
    pub(crate) roots: Vec<PathBuf>,
    /// The home directory `~` stands for, when it is known.
    pub(crate) home: Option<PathBuf>,
}

impl Saved {
    /// What `settings` gives: the project roots as the config writes them, else the
    /// default ones that are scanned.
    pub(crate) fn of(settings: &Settings) -> Self {
        let config = settings.loaded.config.clone();
        let roots = config.project_roots.as_ref().map_or_else(
            || {
                settings
                    .roots
                    .all(SymbolicRoot::ProjectRoots)
                    .iter()
                    .filter_map(|root| root.as_ref().ok())
                    .map(|root| root.configured().clone())
                    .collect()
            },
            |written| written.iter().map(PathBuf::from).collect(),
        );
        Self {
            file: settings.loaded.location.path.clone(),
            config,
            policy: settings.policy.clone(),
            roots,
            home: config::home(&settings.roots).map(Path::to_owned),
        }
    }
}

impl App {
    /// Takes on what the config says, before any finding arrives.
    pub(crate) fn configure(&mut self, saved: Saved) {
        self.file = saved.file;
        self.config = saved.config;
        self.policy = saved.policy;
        self.roots = saved.roots;
        self.home = saved.home;
    }

    /// What the loop should do next, if anything.
    pub(crate) const fn take_effect(&mut self) -> Option<Effect> {
        self.effect.take()
    }

    /// `p`: asks for a note, then keeps the focused finding.
    pub(super) fn keep_finding(&mut self) {
        let Some(Node::Finding(id)) = self.focus() else {
            self.notice = Some("Move to a finding to keep it.".to_owned());
            return;
        };
        let Some(row) = self.row(id) else {
            return;
        };
        let lineage = row
            .finding
            .facts
            .as_ref()
            .map_or_else(Lineage::default, |facts| facts.lineage.clone());
        if self.policy.kept(&row.finding.path, &lineage).is_some() {
            self.notice = Some("Already kept. Settings (,) lists what is kept.".to_owned());
            return;
        }
        self.note.clear();
        self.mode = Mode::Note(id);
    }

    pub(super) fn note_key(&mut self, id: FindingId, key: Key) {
        match key {
            Key::Char(c) => self.note.push(c),
            Key::Backspace => {
                self.note.pop();
            }
            Key::Esc => {
                self.mode = Mode::Browse;
                self.notice = Some("Not kept.".to_owned());
            }
            Key::Enter => {
                self.mode = Mode::Browse;
                self.keep(id);
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
    }

    /// Takes finding `id` out of the selection and asks for its path to be kept.
    fn keep(&mut self, id: FindingId) {
        let Some(path) = self.row(id).map(|row| row.finding.path.clone()) else {
            return;
        };
        let note = Some(self.note.trim().to_owned()).filter(|note| !note.is_empty());
        let left = self.unselect(|row| row.finding.id == id);
        let mut done = format!(
            "Kept {}. Settings (,) undoes it.",
            escaped(&path.display().to_string())
        );
        if left > 0 {
            done.push_str(" It left the selection.");
        }
        self.save(vec![Edit::Keep { path, note }], done);
    }

    /// `P`: turns the focused finding's rule off, which keeps all its findings.
    pub(super) fn keep_rule(&mut self) {
        let Some(Node::Finding(id)) = self.focus() else {
            self.notice = Some("Move to a finding to keep its rule.".to_owned());
            return;
        };
        let Some(rule) = self.row(id).map(|row| row.finding.rule.clone()) else {
            return;
        };
        let title = self.catalog.title(&rule).to_owned();
        if self.policy.rules.get(&rule).is_some_and(|own| own.disabled) {
            self.notice = Some(format!("{title} is already turned off."));
            return;
        }
        let left = self.unselect(|row| row.finding.rule == rule);
        let mut done =
            format!("Turned off {title} ({rule}): its findings are kept. Settings (,) undoes it.");
        if left > 0 {
            write!(done, " {} left the selection.", findings(left)).ok();
        }
        let edit = Edit::RuleEnabled {
            rule,
            enabled: false,
        };
        self.save(vec![edit], done);
    }

    /// Asks the loop to write `edits`, and to say `done` once they are written.
    pub(crate) fn save(&mut self, edits: Vec<Edit>, done: String) {
        self.effect = Some(Effect::Write(edits));
        self.pending = Some(done);
    }

    /// The config was written: judge every finding again with what it says now, in
    /// place, or scan again when the project roots changed.
    pub(super) fn saved(&mut self, saved: Saved) {
        let done = self.pending.take().unwrap_or_default();
        let rescan = saved.roots != self.roots;
        self.configure(saved);
        if rescan {
            self.rescan(&done);
            return;
        }
        for row in &mut self.rows {
            row.verdict = row.finding.verdict(&self.policy);
        }
        let left = self.reconcile();
        let mut notice = done;
        if left > 0 {
            write!(
                notice,
                " {} left the selection: no longer eligible, or added by a shortcut and no longer Safe or now recent.",
                findings(left)
            )
            .ok();
        }
        self.notice = Some(notice);
    }

    pub(super) fn not_saved(&mut self, error: &str) {
        self.pending = None;
        self.notice = Some(format!("Nothing was changed: {}", escaped(error)));
    }

    /// Forgets this scan's findings and asks for a new scan. Finding IDs only hold within
    /// one scan, so the selection goes too.
    fn rescan(&mut self, done: &str) {
        let cleared = self.selected.len();
        self.rows.clear();
        self.by_id.clear();
        self.gaps.clear();
        self.notices.clear();
        self.advice.clear();
        self.selected.clear();
        self.shortcut.clear();
        self.progress = Progress::Scanning;
        self.cursor = None;
        self.hint = 0;
        self.scroll = 0;
        self.effect = Some(Effect::Rescan);
        let mut notice = format!("{done} Scanning again");
        if cleared > 0 {
            write!(
                notice,
                ", so the selection ({}) was cleared",
                findings(cleared)
            )
            .ok();
        }
        notice.push('.');
        self.notice = Some(notice);
    }

    /// Takes the findings `which` matches out of the selection. How many it took.
    fn unselect(&mut self, which: impl Fn(&Row) -> bool) -> usize {
        self.unselect_ids(|app, id| app.row(id).is_some_and(&which))
    }

    /// Takes the selected findings `which` matches out of the selection. How many.
    fn unselect_ids(&mut self, which: impl Fn(&Self, FindingId) -> bool) -> usize {
        let gone: Vec<FindingId> = self
            .selected
            .iter()
            .copied()
            .filter(|&id| which(self, id))
            .collect();
        for id in &gone {
            self.selected.remove(id);
            self.shortcut.remove(id);
        }
        gone.len()
    }

    /// After the buckets changed: every selected finding no longer eligible, and every
    /// one a shortcut added that is no longer Safe or is now recent, leaves the
    /// selection. Nothing joins it. How many left.
    fn reconcile(&mut self) -> usize {
        self.unselect_ids(|app, id| !app.stays(id))
    }

    fn stays(&self, id: FindingId) -> bool {
        let Some(row) = self.row(id) else {
            return false;
        };
        if !row.eligible() {
            return false;
        }
        self.shortcut.get(&id).is_none_or(|&added| {
            !row.recent() && (added != Shelf::Safe || row.shelf() == Shelf::Safe)
        })
    }
}
