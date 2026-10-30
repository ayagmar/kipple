//! Placing git's answer on disk. Each path is resolved handle-relative from the work tree
//! git was asked about, one component at a time, never through a link, so the file
//! system folds case and normalization itself; a path is then matched to a frozen entry
//! by its file identity, never by its name.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use kipple_core::{Component, EntryKind, FileIdentity, FrozenTree, GitGap, NodeId};

use super::command::Matching;
use crate::executor::sys::{self, Dir};
use crate::executor::{Form, Seen};

/// The item git is asked about, reached from its work tree.
#[derive(Debug)]
pub(super) struct Located {
    work_tree: Dir,
    /// Which directory the work tree is.
    pub(super) work_tree_identity: FileIdentity,
    /// The item's components below the work tree.
    relative: Vec<Component>,
    /// What the item is.
    item: Seen,
    /// The item, open, when it is a directory.
    item_dir: Option<Dir>,
    /// How git must match the item's path: ignoring case when a directory on the way
    /// folds it.
    pub(super) matching: Matching,
}

impl Located {
    /// Which file the item is.
    pub(super) const fn identity(&self) -> FileIdentity {
        self.item.snapshot.identity
    }
}

/// Opens `work_tree`, a path without links, and walks `relative` from it without following
/// one, noting whether any directory on the way folds the case of the name looked up in
/// it.
///
/// # Errors
/// [`GitGap::Failed`] when the work tree can't be opened, and
/// [`GitGap::OtherWorkTree`] when the item can't be reached from it as a real path.
pub(super) fn locate(work_tree: &Path, relative: Vec<Component>) -> Result<Located, GitGap> {
    let root = Dir::open_root(work_tree).map_err(|_| GitGap::Failed)?;
    let root_seen = root.seen().map_err(|_| GitGap::Failed)?;
    if !is_dir(&root_seen) {
        return Err(GitGap::Failed);
    }
    let (last, way) = relative.split_last().ok_or(GitGap::NestedRepository)?;
    let mut folds = false;
    let mut below: Option<Dir> = None;
    for component in way {
        let holder = below.as_ref().unwrap_or(&root);
        let (dir, seen) = open_dir(holder, component).map_err(|_| GitGap::OtherWorkTree)?;
        folds |= folds_case(holder, component, &seen)?;
        below = Some(dir);
    }
    let holder = below.as_ref().unwrap_or(&root);
    let name = sys::name(last.as_os_str()).map_err(|_| GitGap::OtherWorkTree)?;
    let item = holder.stat(&name).map_err(|_| GitGap::OtherWorkTree)?;
    folds |= folds_case(holder, last, &item)?;
    let item_dir = if item.snapshot.kind == EntryKind::Dir {
        let (dir, seen) = open_dir(holder, last).map_err(|_| GitGap::OtherWorkTree)?;
        if seen.snapshot.identity != item.snapshot.identity {
            return Err(GitGap::OtherWorkTree);
        }
        Some(dir)
    } else {
        None
    };
    drop(below);
    Ok(Located {
        work_tree: root,
        work_tree_identity: root_seen.snapshot.identity,
        relative,
        item,
        item_dir,
        matching: if folds {
            Matching::FoldCase
        } else {
            Matching::Exact
        },
    })
}

/// Whether `dir` finds `name`, seen as `seen`, under another ASCII case too. git folds
/// only ASCII case, so other letters need no check.
fn folds_case(dir: &Dir, name: &Component, seen: &Seen) -> Result<bool, GitGap> {
    let upper = name.as_os_str().to_ascii_uppercase();
    let other = if upper == name.as_os_str() {
        name.as_os_str().to_ascii_lowercase()
    } else {
        upper
    };
    if other == name.as_os_str() {
        return Ok(false);
    }
    let other = sys::name(&other).map_err(|_| GitGap::Failed)?;
    match dir.stat(&other) {
        Ok(found) => Ok(found.snapshot.identity == seen.snapshot.identity),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(GitGap::Failed),
    }
}

/// The directory `name` in `dir`, opened without following a link, with what it is.
fn open_dir(dir: &Dir, name: &Component) -> io::Result<(Dir, Seen)> {
    let opened = dir.open_dir(&sys::name(name.as_os_str())?)?;
    let seen = opened.seen()?;
    if !is_dir(&seen) {
        return Err(io::ErrorKind::NotADirectory.into());
    }
    Ok((opened, seen))
}

/// Whether `seen` is a plain directory: not a link, junction, mount point or placeholder.
fn is_dir(seen: &Seen) -> bool {
    seen.snapshot.kind == EntryKind::Dir && seen.form == Form::Plain
}

/// Where one tracked path is.
#[derive(Debug)]
pub(super) enum Place {
    /// Not on disk (a skip-worktree or sparse entry): nothing of the item is tracked by it.
    Absent,
    /// The item itself, a file.
    Item,
    /// A file or link below the item, in the directory with identity `parent`.
    Below { parent: FileIdentity, seen: Seen },
}

/// A directory below the item, kept open for the paths that follow it.
#[derive(Debug)]
struct Opened {
    name: Component,
    dir: Dir,
    identity: FileIdentity,
}

/// Places git's paths one after another. git lists them sorted, so the directories of
/// one are mostly those of the one before, and stay open between them.
#[derive(Debug)]
pub(super) struct Placer<'located> {
    located: &'located Located,
    /// Other spellings of the item's path already found to reach it.
    spellings: Vec<Vec<Component>>,
    open: Vec<Opened>,
}

impl<'located> Placer<'located> {
    pub(super) const fn new(located: &'located Located) -> Self {
        Self {
            located,
            spellings: Vec::new(),
            open: Vec::new(),
        }
    }

    /// Where `record` is.
    ///
    /// # Errors
    /// [`GitGap::Unplaced`] when its start doesn't reach the item, when it is the item
    /// and the item is a directory (a submodule), when it is a directory, or when a
    /// directory on its way is a link or a file.
    pub(super) fn place(&mut self, record: &[Component]) -> Result<Place, GitGap> {
        let located = self.located;
        let (start, below) = record
            .split_at_checked(located.relative.len())
            .ok_or(GitGap::Unplaced)?;
        self.reach_item(start)?;
        let Some((last, way)) = below.split_last() else {
            return if located.item.snapshot.kind == EntryKind::Dir {
                Err(GitGap::Unplaced)
            } else {
                Ok(Place::Item)
            };
        };
        let item = located.item_dir.as_ref().ok_or(GitGap::Unplaced)?;
        if !self.descend(item, way)? {
            return Ok(Place::Absent);
        }
        let (holder, parent) = self.open.last().map_or_else(
            || (item, located.identity()),
            |opened| (&opened.dir, opened.identity),
        );
        let name = sys::name(last.as_os_str()).map_err(|_| GitGap::Unplaced)?;
        match holder.stat(&name) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Place::Absent),
            Ok(seen) if seen.snapshot.kind != EntryKind::Dir && seen.form == Form::Plain => {
                Ok(Place::Below { parent, seen })
            }
            Ok(_) | Err(_) => Err(GitGap::Unplaced),
        }
    }

    /// Checks that `start`, a record's first components, reaches the item: they are the
    /// item's own, or another spelling that resolves to the same file.
    fn reach_item(&mut self, start: &[Component]) -> Result<(), GitGap> {
        let located = self.located;
        if start == located.relative.as_slice() || self.spellings.iter().any(|s| s == start) {
            return Ok(());
        }
        let (last, way) = start.split_last().ok_or(GitGap::Unplaced)?;
        let mut below: Option<Dir> = None;
        for component in way {
            let holder = below.as_ref().unwrap_or(&located.work_tree);
            let (dir, _) = open_dir(holder, component).map_err(|_| GitGap::Unplaced)?;
            below = Some(dir);
        }
        let holder = below.as_ref().unwrap_or(&located.work_tree);
        let name = sys::name(last.as_os_str()).map_err(|_| GitGap::Unplaced)?;
        let seen = holder.stat(&name).map_err(|_| GitGap::Unplaced)?;
        if seen.snapshot.identity != located.identity() {
            return Err(GitGap::Unplaced);
        }
        self.spellings.push(start.to_vec());
        Ok(())
    }

    /// Opens the directories `way` below `item`, keeping those already open. `false`
    /// when one isn't on disk.
    fn descend(&mut self, item: &Dir, way: &[Component]) -> Result<bool, GitGap> {
        let kept = self
            .open
            .iter()
            .zip(way)
            .take_while(|(opened, name)| opened.name == **name)
            .count();
        self.open.truncate(kept);
        for component in way.iter().skip(kept) {
            let holder = self.open.last().map_or(item, |opened| &opened.dir);
            match open_dir(holder, component) {
                Ok((dir, seen)) => self.open.push(Opened {
                    name: component.clone(),
                    dir,
                    identity: seen.snapshot.identity,
                }),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(_) => return Err(GitGap::Unplaced),
            }
        }
        Ok(true)
    }
}

/// The entries of a frozen tree that tracked paths are.
#[derive(Debug)]
pub(super) struct Entries;

impl Entries {
    /// The entries of `tree`, which must be the item's, that `places` are: for each, every
    /// name that file has in that directory.
    ///
    /// # Errors
    /// [`GitGap::Unplaced`] when the tree's top isn't the item, or a place is on disk but
    /// no frozen entry.
    pub(super) fn of(
        tree: &FrozenTree,
        located: &Located,
        places: &[Place],
    ) -> Result<Vec<NodeId>, GitGap> {
        let top = tree.top().snapshot().map(|top| top.identity);
        if top != Some(located.identity()) {
            return Err(GitGap::Unplaced);
        }
        if places.is_empty() {
            return Ok(Vec::new());
        }
        let mut by_identity: HashMap<FileIdentity, Vec<NodeId>> = HashMap::new();
        for entry in tree.entries().skip(1) {
            if let Some(snapshot) = entry.snapshot() {
                by_identity
                    .entry(snapshot.identity)
                    .or_default()
                    .push(entry.id());
            }
        }
        let mut found = Vec::new();
        for place in places {
            match place {
                Place::Absent => {}
                Place::Item => found.push(NodeId::TOP),
                Place::Below { parent, seen } => {
                    found.extend(below(tree, &by_identity, *parent, seen)?);
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        Ok(found)
    }
}

/// The entries of `tree` that are the file `seen` in the directory `parent`.
fn below(
    tree: &FrozenTree,
    by_identity: &HashMap<FileIdentity, Vec<NodeId>>,
    parent: FileIdentity,
    seen: &Seen,
) -> Result<Vec<NodeId>, GitGap> {
    let found: Vec<NodeId> = by_identity
        .get(&seen.snapshot.identity)
        .into_iter()
        .flatten()
        .copied()
        .filter(|id| {
            tree.entry(*id)
                .and_then(|entry| entry.parent()?.snapshot())
                .is_some_and(|dir| dir.identity == parent)
        })
        .collect();
    if found.is_empty() {
        return Err(GitGap::Unplaced);
    }
    Ok(found)
}
