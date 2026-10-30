//! Placing git's answer on disk. git is asked about the item's path only as far as git
//! matches each name the way the file system does; past the first name it might not, it
//! is asked about that name's directory. Each path it returns is resolved handle-relative
//! from the work tree, one component at a time, never through a link, so the file system
//! folds case and normalization itself; a path is then matched to the item, and below it
//! to a frozen entry, by its file identity, never by its name.

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
    /// How many of them git is asked about.
    asked: usize,
    /// What the item is.
    item: Seen,
    /// The item, open, when it is a directory.
    item_dir: Option<Dir>,
    /// How git must match the path it is asked about: ignoring case when a directory on
    /// the way folds it.
    pub(super) matching: Matching,
}

impl Located {
    /// The path git is asked about: the item's, or the nearest directory above it whose
    /// every name git matches as the file system does. Empty for the whole work tree.
    pub(super) fn asked(&self) -> &[Component] {
        self.relative.get(..self.asked).unwrap_or_default()
    }

    /// Which file the item is.
    pub(super) const fn identity(&self) -> FileIdentity {
        self.item.snapshot.identity
    }
}

/// Opens `work_tree`, a path without links, and walks `relative` from it without following
/// one, noting how much of it git can be asked about and whether any directory on that
/// part folds the case of the name looked up in it.
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
    let mut question = Question::default();
    let mut below: Option<Dir> = None;
    for component in way {
        let holder = below.as_ref().unwrap_or(&root);
        let (dir, seen) = open_dir(holder, component).map_err(|_| GitGap::OtherWorkTree)?;
        question.add(holder, component, &seen, Some(&dir))?;
        below = Some(dir);
    }
    let holder = below.as_ref().unwrap_or(&root);
    let name = sys::name(last.as_os_str()).map_err(|_| GitGap::OtherWorkTree)?;
    let item = holder.stat(&name).map_err(|_| GitGap::OtherWorkTree)?;
    let item_dir = if item.snapshot.kind == EntryKind::Dir {
        let (dir, seen) = open_dir(holder, last).map_err(|_| GitGap::OtherWorkTree)?;
        if seen.snapshot.identity != item.snapshot.identity {
            return Err(GitGap::OtherWorkTree);
        }
        Some(dir)
    } else {
        None
    };
    question.add(holder, last, &item, item_dir.as_ref())?;
    drop(below);
    Ok(Located {
        work_tree: root,
        work_tree_identity: root_seen.snapshot.identity,
        relative,
        asked: question.asked,
        item,
        item_dir,
        matching: if question.folds {
            Matching::FoldCase
        } else {
            Matching::Exact
        },
    })
}

/// How much of the item's path git is asked about, and how.
#[derive(Debug, Default)]
struct Question {
    /// How many names, from the work tree down.
    asked: usize,
    /// Whether a name was left out, and with it every one below.
    cut: bool,
    /// Whether a directory on the asked part folds case.
    folds: bool,
}

impl Question {
    /// Adds `name`, found as `seen` in `holder` and open as `opened` when a directory,
    /// unless git can't match it as the file system does or a name above it was left out.
    fn add(
        &mut self,
        holder: &Dir,
        name: &Component,
        seen: &Seen,
        opened: Option<&Dir>,
    ) -> Result<(), GitGap> {
        if self.cut || !matched_by_git(name, opened) {
            self.cut = true;
            return Ok(());
        }
        self.asked += 1;
        self.folds |= folds_case(holder, name, seen)?;
        Ok(())
    }
}

/// Whether git, matching `name` byte for byte or ignoring ASCII case, finds every name the
/// file system takes for it. git folds neither Unicode case nor normalization, which a
/// file system may (APFS, NTFS, Linux casefold directories), so a name with a byte past
/// ASCII could be in the index under another spelling git wouldn't match.
#[cfg(unix)]
fn matched_by_git(name: &Component, _opened: Option<&Dir>) -> bool {
    name.as_os_str().as_encoded_bytes().is_ascii()
}

/// On Windows a directory also answers to its 8.3 short name (`MYPROJ~1`), so `name` must
/// also be the name it is stored under, but for ASCII case, read from its handle. A file's
/// isn't read: it is asked about by its directory.
#[cfg(windows)]
fn matched_by_git(name: &Component, opened: Option<&Dir>) -> bool {
    let given = name.as_os_str().as_encoded_bytes();
    given.is_ascii()
        && opened
            .and_then(|dir| dir.stored_name().ok())
            .is_some_and(|stored| stored.as_encoded_bytes().eq_ignore_ascii_case(given))
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
    /// Not in the item: another path below the directory git was asked about.
    Outside,
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
    /// The last start found to be another path. git lists paths sorted, so those below
    /// one directory follow each other.
    elsewhere: Option<Vec<Component>>,
    open: Vec<Opened>,
}

impl<'located> Placer<'located> {
    pub(super) const fn new(located: &'located Located) -> Self {
        Self {
            located,
            spellings: Vec::new(),
            elsewhere: None,
            open: Vec::new(),
        }
    }

    /// Where `record` is.
    ///
    /// # Errors
    /// [`GitGap::Unplaced`] when its start can't be told to be the item or not, when it
    /// is the item and the item is a directory (a submodule), when it is a directory, or
    /// when a directory on its way in the item is a link or a file.
    pub(super) fn place(&mut self, record: &[Component]) -> Result<Place, GitGap> {
        let located = self.located;
        let Some((start, below)) = record.split_at_checked(located.relative.len()) else {
            return Ok(Place::Outside);
        };
        if !self.reaches_item(start)? {
            return Ok(Place::Outside);
        }
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

    /// Whether `start`, a record's first components, is the item: its own path, or
    /// another spelling the file system resolves to the same file.
    fn reaches_item(&mut self, start: &[Component]) -> Result<bool, GitGap> {
        let located = self.located;
        if start == located.relative.as_slice() || self.spellings.iter().any(|s| s == start) {
            return Ok(true);
        }
        if self.elsewhere.as_deref() == Some(start) {
            return Ok(false);
        }
        let reaches = self.resolves_to_item(start)?;
        if reaches {
            self.spellings.push(start.to_vec());
        } else {
            self.elsewhere = Some(start.to_vec());
        }
        Ok(reaches)
    }

    /// Resolves `start` from the work tree: the item when it ends on the item's file. A
    /// path that names nothing, or passes through a file, isn't the item.
    ///
    /// # Errors
    /// [`GitGap::Unplaced`] when a directory on the way is a link, a junction, a mount
    /// point or a placeholder, which could lead anywhere, or can't be read.
    fn resolves_to_item(&self, start: &[Component]) -> Result<bool, GitGap> {
        let located = self.located;
        let (last, way) = start.split_last().ok_or(GitGap::Unplaced)?;
        let mut below: Option<Dir> = None;
        for component in way {
            let holder = below.as_ref().unwrap_or(&located.work_tree);
            match step(holder, component)? {
                Some(dir) => below = Some(dir),
                None => return Ok(false),
            }
        }
        let holder = below.as_ref().unwrap_or(&located.work_tree);
        let name = sys::name(last.as_os_str()).map_err(|_| GitGap::Unplaced)?;
        match holder.stat(&name) {
            Ok(seen) => Ok(seen.snapshot.identity == located.identity()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(GitGap::Unplaced),
        }
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

/// The directory `name` in `holder`, opened on the way to a record's start. `None` when
/// nothing has that name or it is a file, neither of which can hold the item.
fn step(holder: &Dir, name: &Component) -> Result<Option<Dir>, GitGap> {
    let os_name = sys::name(name.as_os_str()).map_err(|_| GitGap::Unplaced)?;
    let seen = match holder.stat(&os_name) {
        Ok(seen) => seen,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(GitGap::Unplaced),
    };
    if seen.form == Form::Plain && matches!(seen.snapshot.kind, EntryKind::File | EntryKind::Other)
    {
        return Ok(None);
    }
    if !is_dir(&seen) {
        return Err(GitGap::Unplaced);
    }
    let (dir, opened) = open_dir(holder, name).map_err(|_| GitGap::Unplaced)?;
    if opened.snapshot.identity != seen.snapshot.identity {
        return Err(GitGap::Unplaced);
    }
    Ok(Some(dir))
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
                Place::Outside | Place::Absent => {}
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn components(path: &str) -> Vec<Component> {
        path.split('/')
            .map(|name| Component::new(name).unwrap())
            .collect()
    }

    fn asked(dirs: &str) -> Vec<String> {
        let temp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(dirs)).unwrap();
        let root = crate::os::physical(temp.path()).unwrap();
        let located = locate(&root, components(dirs)).unwrap();
        located
            .asked()
            .iter()
            .map(|name| name.as_os_str().to_str().unwrap().to_owned())
            .collect()
    }

    /// Catches a name git can't match as the file system does put in the pathspec: a
    /// folding volume finds `Été` under `été` while git's string match doesn't, so git
    /// answers nothing and the item's tracked files are deleted. git is asked about the
    /// directory above that name instead.
    #[test]
    fn git_is_asked_about_the_item_only_up_to_a_name_past_ascii() {
        assert_eq!(asked("p/target"), ["p", "target"]);
        assert_eq!(asked("p/\u{c9}t\u{e9}/target"), ["p"]);
        assert_eq!(asked("\u{fc}ber/target"), Vec::<String>::new());
    }
}
