//! Projects and the order of the tree (`docs/03-functional-spec.md` §5 Projects, Sort):
//! inside a bucket's group, the findings of one project under a project root share a
//! collapsible line once there are two of them, and `o` changes what every group and
//! project is sorted by.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kipple_core::{Lens, Modified};

use super::super::ui;
use super::{App, Line, Node, Row, Shelf};

/// A project, by the order it was first seen in this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ProjectId(usize);

/// The top-level directory below a project root that holds findings.
#[derive(Debug)]
pub(crate) struct Project {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
}

/// Where a finding sits: in a project or not, and what its line says by default.
#[derive(Debug)]
pub(crate) struct Spot {
    pub(crate) project: Option<ProjectId>,
    /// Its path inside its project, `/` between components, empty outside every
    /// project.
    pub(crate) inside: String,
    /// Its label when it has a line of its own: `api/target` in a project, its last two
    /// components outside one.
    pub(crate) name: String,
}

/// What every group and project is sorted by. `o` cycles through them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Order {
    /// Largest first.
    #[default]
    Size,
    /// Oldest last change first; unknown last.
    Changed,
    Name,
}

impl Order {
    pub(super) const fn next(self) -> Self {
        match self {
            Self::Size => Self::Changed,
            Self::Changed => Self::Name,
            Self::Name => Self::Size,
        }
    }

    /// How the title line names it: nothing for size, the default.
    pub(crate) const fn title(self) -> Option<&'static str> {
        match self {
            Self::Size => None,
            Self::Changed => Some("by last change"),
            Self::Name => Some("by name"),
        }
    }

    /// What `o` says once it chose this order.
    pub(super) const fn said(self) -> &'static str {
        match self {
            Self::Size => "Sorted by size, largest first.",
            Self::Changed => "Sorted by last change, oldest first; unknown last.",
            Self::Name => "Sorted by name.",
        }
    }

    /// Ties break by path, so the order never depends on when a finding arrived.
    fn compare(self, a: &Key<'_>, b: &Key<'_>) -> Ordering {
        let first = match self {
            Self::Size => b.bytes.cmp(&a.bytes),
            Self::Changed => match (a.age, b.age) {
                (Some(a), Some(b)) => b.cmp(&a),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
            Self::Name => caseless(a.name).cmp(caseless(b.name)),
        };
        first.then_with(|| a.path.cmp(b.path))
    }
}

fn caseless(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().flat_map(char::to_lowercase)
}

/// What a line sorts by.
#[derive(Debug, Clone, Copy)]
struct Key<'rows> {
    bytes: u64,
    /// How long ago it last changed; for a project, its oldest finding's.
    age: Option<Duration>,
    name: &'rows str,
    path: &'rows Path,
}

impl<'a> Key<'a> {
    fn of(row: &'a Row) -> Self {
        Self {
            bytes: row.bytes(),
            age: age(row),
            name: &row.spot.name,
            path: &row.finding.path,
        }
    }
}

/// How long ago `row` last changed, when that is known.
fn age(row: &Row) -> Option<Duration> {
    match row.finding.facts.as_ref()?.modified {
        Modified::Ago(ago) => Some(ago),
        Modified::Unknown(_) => None,
    }
}

/// One line of a group: a finding, or a project with the findings under its line.
struct Entry<'rows> {
    node: Node,
    key: Key<'rows>,
    rows: Vec<&'rows Row>,
}

/// A lens within a bucket, as the tree groups findings.
#[derive(Debug, Clone, Copy)]
pub(super) struct Group {
    pub(super) shelf: Shelf,
    pub(super) lens: Lens,
}

impl App {
    /// The lines of `group` holding `rows`, sorted, below its heading; `whole` unfolds
    /// every project.
    pub(super) fn group_lines(&self, group: Group, rows: Vec<&Row>, whole: bool) -> Vec<Line> {
        let mut projects: BTreeMap<ProjectId, Vec<&Row>> = BTreeMap::new();
        let mut entries = Vec::new();
        for row in rows {
            match row.spot.project {
                Some(id) => projects.entry(id).or_default().push(row),
                None => entries.push(Self::entry(row)),
            }
        }
        for (id, held) in projects {
            // A project with one finding here keeps it on one line.
            match self.project(id).filter(|_| held.len() > 1) {
                Some(project) => entries.push(project_entry(group, id, project, held)),
                None => entries.extend(held.into_iter().map(Self::entry)),
            }
        }
        entries.sort_by(|a, b| self.order.compare(&a.key, &b.key));
        let mut lines = Vec::new();
        for mut entry in entries {
            lines.push(Line {
                node: entry.node,
                depth: 2,
            });
            if !whole && self.is_collapsed(entry.node) {
                continue;
            }
            entry
                .rows
                .sort_by(|a, b| self.order.compare(&Key::of(a), &Key::of(b)));
            lines.extend(entry.rows.iter().map(|row| Line {
                node: Node::Finding(row.finding.id),
                depth: 3,
            }));
        }
        lines
    }

    /// A finding's own line.
    fn entry(row: &Row) -> Entry<'_> {
        Entry {
            node: Node::Finding(row.finding.id),
            key: Key::of(row),
            rows: Vec::new(),
        }
    }

    /// The project `id`.
    pub(crate) fn project(&self, id: ProjectId) -> Option<&Project> {
        self.projects.get(id.0)
    }

    /// The projects shown that share their name with another one shown, so their lines
    /// show their path from home instead.
    pub(crate) fn clashing(&self) -> BTreeSet<ProjectId> {
        let mut named: BTreeMap<&str, BTreeSet<ProjectId>> = BTreeMap::new();
        for row in self.rows.iter().filter(|row| self.shown(row)) {
            if let Some((id, project)) = row
                .spot
                .project
                .and_then(|id| Some((id, self.project(id)?)))
            {
                named.entry(&project.name).or_default().insert(id);
            }
        }
        named
            .into_values()
            .filter(|ids| ids.len() > 1)
            .flatten()
            .collect()
    }

    /// A project root the scan opened. The findings already found are placed again,
    /// in case one is inside it.
    pub(super) fn open_root(&mut self, root: PathBuf) {
        if self.scanned_roots.contains(&root) {
            return;
        }
        self.scanned_roots.push(root);
        let paths: Vec<PathBuf> = self
            .rows
            .iter()
            .map(|row| row.finding.path.clone())
            .collect();
        for (index, path) in paths.iter().enumerate() {
            let spot = self.spot(path);
            if let Some(row) = self.rows.get_mut(index) {
                row.spot = spot;
            }
        }
    }

    /// Where `path` sits: in the project below the deepest project root holding it,
    /// compared as the scan reported both.
    pub(super) fn spot(&mut self, path: &Path) -> Spot {
        let outside = || Spot {
            project: None,
            inside: String::new(),
            name: ui::tail(path),
        };
        let Some((root, rest)) = self
            .scanned_roots
            .iter()
            .filter_map(|root| Some((root, path.strip_prefix(root).ok()?)))
            .max_by_key(|(root, _)| root.components().count())
        else {
            return outside();
        };
        let mut parts = rest.components();
        let (Some(first), inside) = (parts.next(), parts.as_path()) else {
            return outside();
        };
        if inside.as_os_str().is_empty() {
            // The finding is the project's own directory: nothing to group.
            return outside();
        }
        let name = first.as_os_str().to_string_lossy().into_owned();
        let inside = ui::slashed(inside);
        let label = format!("{name}/{inside}");
        let project = root.join(first);
        let id = self.intern(project, name);
        Spot {
            project: Some(id),
            inside,
            name: label,
        }
    }

    /// The ID of the project at `path`, the same for the whole session.
    fn intern(&mut self, path: PathBuf, name: String) -> ProjectId {
        if let Some(&id) = self.project_ids.get(&path) {
            return id;
        }
        let id = ProjectId(self.projects.len());
        self.project_ids.insert(path.clone(), id);
        self.projects.push(Project { path, name });
        id
    }
}

/// A project's line in the group `(bucket, lens)`: its total, its oldest finding and
/// its name.
fn project_entry<'a>(
    Group { shelf, lens }: Group,
    id: ProjectId,
    project: &'a Project,
    rows: Vec<&'a Row>,
) -> Entry<'a> {
    Entry {
        node: Node::Project(shelf, lens, id),
        key: Key {
            bytes: rows.iter().map(|row| row.bytes()).sum(),
            age: rows.iter().filter_map(|row| age(row)).max(),
            name: &project.name,
            path: &project.path,
        },
        rows,
    }
}
