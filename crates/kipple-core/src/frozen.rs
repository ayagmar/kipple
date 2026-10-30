//! Frozen trees: what a plan authorizes, entry by entry (`docs/04-safety-model.md` §3.5).
//!
//! A [`FrozenTree`] is the target of one plan item and everything below it, as the
//! [`Freezer`](crate::Freezer) found it: each file, directory and link with its identity
//! and change fingerprint. It authorizes exactly those entries. A child that appears
//! later isn't in it, so nothing authorizes it. Names are kept exactly as the OS gave
//! them ([`Component`] holds an `OsString`), so a non-UTF-8 name on Unix or an ill-formed
//! UTF-16 one on Windows is removed by its own name, never by a lossy copy.
//!
//! The tree is compact: names are interned, and the nodes sit in one array in canonical
//! order (breadth first, each directory's children together and sorted by name), so the
//! same entries always give the same tree, [`encode`](FrozenTree::encode) and
//! [`digest`](FrozenTree::digest), whatever order the freezer read them in.
//!
//! What may be removed is a [`Mark`] per node. The tree's own marks say what the plan
//! authorized; a [`Marks`] overlay of them is what revalidation leaves. Marks only ever
//! move from [`Mark::Remove`] to something else: nothing in this module turns an entry
//! back on, so revalidation can only subtract (04 §3.6).

mod codec;
mod recheck;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{self, Path, PathBuf};

use crate::outcome::LeftReason;
use crate::probe::{EntryKind, FileIdentity};

pub use codec::{DecodeError, NameCodec};

/// One name inside a directory, exactly as the OS gave it.
///
/// Never empty, `.` or `..`, and holds no separator and no NUL, so joining components
/// can't climb out of a directory or name two levels at once.
///
/// It may hold `:`, a valid name on Unix. On Windows an NT-relative open of `a:b` names
/// the stream `b` of `a`, not an entry, so the platform keeps such names out: its
/// [`NameCodec`] refuses one when decoding, and its freezer never yields one (NTFS and
/// `ReFS` don't allow `:` in a name).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Component(OsString);

/// Why a name can't be a [`Component`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BadName {
    /// It is empty.
    #[error("the name is empty")]
    Empty,
    /// It is `.` or `..`.
    #[error("the name is `.` or `..`")]
    Dot,
    /// It holds a path separator.
    #[error("the name holds a path separator")]
    Separator,
    /// It holds a NUL.
    #[error("the name holds a NUL")]
    Nul,
}

impl Component {
    /// `name`, if it is one name.
    ///
    /// # Errors
    /// When it is empty, `.` or `..`, or holds a separator or a NUL.
    pub fn new(name: impl Into<OsString>) -> Result<Self, BadName> {
        let name = name.into();
        let bytes = name.as_encoded_bytes();
        if bytes.is_empty() {
            return Err(BadName::Empty);
        }
        if bytes == b"." || bytes == b".." {
            return Err(BadName::Dot);
        }
        if bytes.contains(&0) {
            return Err(BadName::Nul);
        }
        // Separators are ASCII, and an ASCII byte is never part of a longer character in
        // the OS string encoding.
        if bytes
            .iter()
            .any(|byte| byte.is_ascii() && path::is_separator(char::from(*byte)))
        {
            return Err(BadName::Separator);
        }
        Ok(Self(name))
    }

    /// The name.
    #[must_use]
    pub fn as_os_str(&self) -> &OsStr {
        &self.0
    }
}

/// A path below a root or a target, one [`Component`] at a time. Empty means the root or
/// target itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelPath(Vec<Component>);

impl RelPath {
    /// These components, in order.
    #[must_use]
    pub const fn new(components: Vec<Component>) -> Self {
        Self(components)
    }

    /// The relative path `path`, which must name only normal components.
    ///
    /// # Errors
    /// When `path` is absolute or holds `.`, `..` or a Windows prefix.
    pub fn of(path: &Path) -> Result<Self, BadName> {
        path.components()
            .map(|component| match component {
                path::Component::Normal(name) => Component::new(name),
                path::Component::CurDir | path::Component::ParentDir => Err(BadName::Dot),
                path::Component::RootDir | path::Component::Prefix(_) => Err(BadName::Separator),
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }

    /// Its components.
    #[must_use]
    pub fn components(&self) -> &[Component] {
        &self.0
    }

    /// Whether it is the root or target itself.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `base` with these components below it, for showing people. Mutation never goes
    /// through the joined path: it walks the components from a verified handle.
    #[must_use]
    pub fn below(&self, base: &Path) -> PathBuf {
        let mut path = base.to_owned();
        path.extend(self.0.iter().map(Component::as_os_str));
        path
    }
}

/// A point in time as a file system records it, exactly: seconds since the Unix epoch
/// and nanoseconds within that second. Compared only for equality with itself later, so
/// it is never rounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp {
    secs: i64,
    nanos: u32,
}

/// Nanoseconds in a second, the bound of [`Stamp`]'s fraction.
const NANOS: u32 = 1_000_000_000;

impl Stamp {
    /// `secs` seconds and `nanos` nanoseconds after the Unix epoch, when `nanos` is less
    /// than a second.
    #[must_use]
    pub const fn new(secs: i64, nanos: u32) -> Option<Self> {
        if nanos < NANOS {
            Some(Self { secs, nanos })
        } else {
            None
        }
    }

    /// A Windows time: `ticks` of 100 nanoseconds since 1601-01-01 UTC. Exact and
    /// one-to-one, so two different times never become one stamp.
    #[must_use]
    pub const fn from_windows_ticks(ticks: i64) -> Self {
        const TICKS_PER_SEC: i64 = 10_000_000;
        const EPOCH_GAP_SECS: i64 = 11_644_473_600;
        Self {
            secs: ticks.div_euclid(TICKS_PER_SEC) - EPOCH_GAP_SECS,
            // Below 10 000 000, so times 100 it fits in a u32 below a second.
            #[expect(
                clippy::cast_possible_truncation,
                reason = "rem_euclid is in 0..10_000_000"
            )]
            nanos: (ticks.rem_euclid(TICKS_PER_SEC) * 100) as u32,
        }
    }

    /// Whole seconds since the Unix epoch.
    #[must_use]
    pub const fn secs(self) -> i64 {
        self.secs
    }

    /// Nanoseconds within the second.
    #[must_use]
    pub const fn nanos(self) -> u32 {
        self.nanos
    }
}

/// What changes when an entry is changed in place, read from its open handle.
///
/// Unix: `changed` is the status change time (ctime), `modified` the modification time,
/// `born` the birth time where the OS gives one. Windows: the change, last write and
/// creation times. `size` is the length in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint {
    /// The status change time (Unix ctime, Windows change time).
    pub changed: Stamp,
    /// The modification time (Windows last write time).
    pub modified: Stamp,
    /// The birth or creation time, where the OS reports one.
    pub born: Option<Stamp>,
    /// The length in bytes.
    pub size: u64,
}

/// One entry as read from its open handle: what it is, which file it is and its
/// fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntrySnapshot {
    /// What it is, never a link's target.
    pub kind: EntryKind,
    /// Which file it is.
    pub identity: FileIdentity,
    /// What changes when it is changed in place.
    pub fingerprint: Fingerprint,
    /// How many names the file has. A directory's count says nothing about hard links.
    pub links: u64,
}

named! {
    /// How an entry differs from the one frozen.
    Drift {
        /// Another file is there now: a different identity or kind.
        Replaced = "replaced",
        /// The same file, changed in place: a different fingerprint.
        Changed = "changed",
    }
}

impl EntrySnapshot {
    /// How `now`, read later at the same place, differs from this entry, if it does.
    ///
    /// A different kind or identity is [`Drift::Replaced`]. Otherwise the fingerprints
    /// are compared: all of it for a directory, or a file with one name. A file with more
    /// than one name, then or now, compares only its modification time and size: removing
    /// one of its names, which kipple itself does when two frozen names share an inode
    /// (Cargo hard-links `target/<profile>/x` and `deps/x-<hash>`), changes its status
    /// change time but not its contents.
    ///
    /// The executor calls this on the open handle before this run touches the entry, and
    /// revalidation on a fresh freeze before the run starts.
    #[must_use]
    pub fn drift(&self, now: &Self) -> Option<Drift> {
        if self.kind != now.kind || self.identity != now.identity {
            return Some(Drift::Replaced);
        }
        let (then, now_fp) = (&self.fingerprint, &now.fingerprint);
        let linked = self.kind != EntryKind::Dir && (self.links > 1 || now.links > 1);
        let same = if linked {
            then.modified == now_fp.modified && then.size == now_fp.size
        } else {
            then == now_fp
        };
        (!same).then_some(Drift::Changed)
    }
}

/// The file system a tree lives on. Every entry of a tree is on it: a freezer never
/// crosses into another one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Volume {
    /// The device (Unix `st_dev`) or volume serial (Windows), which every entry's
    /// [`FileIdentity::volume`] equals.
    pub device: u64,
    /// The mount's own identity where the OS has one (Linux `statx` mount ID), which tells
    /// a bind mount of the same device apart.
    pub mount: Option<u64>,
}

named! {
    /// What a tree's top entry is.
    Scope {
        /// The target itself, and everything below it.
        Whole = "whole",
        /// Only what is inside the target, which is a verified root: the root directory
        /// itself is never removed.
        Contents = "contents",
    }
}

named! {
    /// Why an entry in a plan was never authorized.
    Unauthorized {
        /// It is the root directory of a [`Scope::Contents`] tree.
        Top = "top",
        /// It was left out, with its directories kept, such as a file git tracks.
        Excluded = "excluded",
    }
}

/// Whether an entry may be removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mark {
    /// Authorized: remove it once it is checked again.
    Remove,
    /// Never authorized, and shown so in review.
    Unauthorized(Unauthorized),
    /// Authorized in review, then dropped before the run touched it.
    Dropped(LeftReason),
}

/// An entry's place in its tree. Only meaningful with the tree it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(u32);

impl NodeId {
    /// The tree's top entry.
    pub const TOP: Self = Self(0);

    pub(crate) const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A node with no parent or name: the top.
const NONE: u32 = u32::MAX;

/// A birth time that wasn't reported, in a node's nanosecond field.
const NO_BIRTH: u32 = u32::MAX;

/// One entry, packed. Its children are the nodes from the previous node's `end` (1 for
/// the top) up to its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Node {
    parent: u32,
    name: u32,
    end: u32,
    links: u32,
    kind: EntryKind,
    file: [u64; 2],
    size: u64,
    /// Changed, modified and born, in that order.
    secs: [i64; 3],
    nanos: [u32; 3],
}

impl Node {
    fn new(parent: u32, name: u32, entry: &EntrySnapshot) -> Self {
        let fp = &entry.fingerprint;
        let birth = fp.born.unwrap_or(Stamp { secs: 0, nanos: 0 });
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the file ID is split into its two halves"
        )]
        let file = [
            entry.identity.file as u64,
            (entry.identity.file >> 64) as u64,
        ];
        Self {
            parent,
            name,
            end: 0,
            links: u32::try_from(entry.links).unwrap_or(u32::MAX),
            kind: entry.kind,
            file,
            size: fp.size,
            secs: [fp.changed.secs, fp.modified.secs, birth.secs],
            nanos: [
                fp.changed.nanos,
                fp.modified.nanos,
                fp.born.map_or(NO_BIRTH, |born| born.nanos),
            ],
        }
    }

    fn snapshot(&self, volume: Volume) -> EntrySnapshot {
        let [changed, modified, born] = self.secs;
        let [changed_nanos, modified_nanos, born_nanos] = self.nanos;
        EntrySnapshot {
            kind: self.kind,
            identity: FileIdentity {
                volume: volume.device,
                file: u128::from(self.file[0]) | (u128::from(self.file[1]) << 64),
            },
            fingerprint: Fingerprint {
                changed: Stamp {
                    secs: changed,
                    nanos: changed_nanos,
                },
                modified: Stamp {
                    secs: modified,
                    nanos: modified_nanos,
                },
                born: (born_nanos != NO_BIRTH).then_some(Stamp {
                    secs: born,
                    nanos: born_nanos,
                }),
                size: self.size,
            },
            links: u64::from(self.links),
        }
    }
}

/// A plan item's target and everything below it, frozen. See the module documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenTree {
    volume: Volume,
    scope: Scope,
    names: Vec<Component>,
    nodes: Vec<Node>,
    marks: Vec<Mark>,
}

impl FrozenTree {
    /// The file system every entry is on.
    #[must_use]
    pub const fn volume(&self) -> Volume {
        self.volume
    }

    /// Whether the top entry itself may be removed.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// How many entries it holds, the top included.
    #[must_use]
    pub const fn entry_count(&self) -> usize {
        self.nodes.len()
    }

    /// The target.
    #[must_use]
    pub const fn top(&self) -> Entry<'_> {
        Entry {
            tree: self,
            id: NodeId::TOP,
        }
    }

    /// The entry `id`, if this tree has it.
    #[must_use]
    pub fn entry(&self, id: NodeId) -> Option<Entry<'_>> {
        (id.index() < self.nodes.len()).then_some(Entry { tree: self, id })
    }

    /// Every entry in canonical order: breadth first, each directory's children by name.
    /// A directory comes before everything inside it.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = Entry<'_>> {
        (0..self.nodes.len()).map(|index| Entry {
            tree: self,
            id: NodeId(u32::try_from(index).unwrap_or(NONE)),
        })
    }

    /// The entry at `path` below the target, if the tree has one.
    #[must_use]
    pub fn find(&self, path: &RelPath) -> Option<NodeId> {
        let mut entry = self.top();
        for component in path.components() {
            entry = entry.child(component)?;
        }
        Some(entry.id)
    }

    /// What the plan authorized for `id`. [`Mark::Dropped`] never appears here.
    #[must_use]
    pub fn mark(&self, id: NodeId) -> Mark {
        self.marks
            .get(id.index())
            .copied()
            .unwrap_or(Mark::Unauthorized(Unauthorized::Excluded))
    }

    /// Leaves `id` and everything below it out of the plan, keeping the directories above
    /// it: they won't be empty, so they stay. For files the plan must not touch, such as
    /// those git tracks. Only takes authority away.
    pub fn exclude(&mut self, id: NodeId) {
        for below in self.subtree(id) {
            if let Some(mark @ Mark::Remove) = self.marks.get_mut(below.index()) {
                *mark = Mark::Unauthorized(Unauthorized::Excluded);
            }
        }
    }

    /// `id` and every entry below it.
    fn subtree(&self, id: NodeId) -> Vec<NodeId> {
        let mut found = Vec::new();
        let mut stack = vec![id];
        while let Some(next) = stack.pop() {
            let Some(entry) = self.entry(next) else {
                continue;
            };
            found.push(next);
            stack.extend(entry.children().map(|child| child.id));
        }
        found
    }

    fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index())
    }

    fn name(&self, index: u32) -> Option<&Component> {
        self.names.get(index as usize)
    }

    /// The range of `id`'s children.
    fn children_of(&self, id: NodeId) -> std::ops::Range<u32> {
        let start = match id.0 {
            0 => 1,
            index => self
                .nodes
                .get(index as usize - 1)
                .map_or(1, |node| node.end),
        };
        let end = self.node(id).map_or(start, |node| node.end);
        start..end.max(start)
    }
}

/// One entry of a [`FrozenTree`].
#[derive(Debug, Clone, Copy)]
pub struct Entry<'tree> {
    tree: &'tree FrozenTree,
    id: NodeId,
}

impl<'tree> Entry<'tree> {
    /// Its place in the tree.
    #[must_use]
    pub const fn id(&self) -> NodeId {
        self.id
    }

    fn node(&self) -> Option<&'tree Node> {
        self.tree.node(self.id)
    }

    /// Its name in its directory. The top has none: it is reached through the anchor.
    #[must_use]
    pub fn name(&self) -> Option<&'tree Component> {
        self.node().and_then(|node| self.tree.name(node.name))
    }

    /// What it is.
    #[must_use]
    pub fn kind(&self) -> EntryKind {
        self.node().map_or(EntryKind::Other, |node| node.kind)
    }

    /// What it was when it was frozen.
    #[must_use]
    pub fn snapshot(&self) -> Option<EntrySnapshot> {
        self.node().map(|node| node.snapshot(self.tree.volume))
    }

    /// What the plan authorized for it.
    #[must_use]
    pub fn mark(&self) -> Mark {
        self.tree.mark(self.id)
    }

    /// The directory holding it, unless it is the top.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        let parent = self.node()?.parent;
        (parent != NONE).then_some(Self {
            tree: self.tree,
            id: NodeId(parent),
        })
    }

    /// The entries directly inside it, by name.
    pub fn children(&self) -> impl ExactSizeIterator<Item = Entry<'tree>> + use<'tree> {
        let tree = self.tree;
        tree.children_of(self.id).map(move |index| Entry {
            tree,
            id: NodeId(index),
        })
    }

    /// The entry directly inside it called `name`.
    #[must_use]
    pub fn child(&self, name: &Component) -> Option<Self> {
        let range = self.tree.children_of(self.id);
        let nodes = self
            .tree
            .nodes
            .get(range.start as usize..range.end as usize)?;
        let found = nodes
            .binary_search_by(|node| {
                self.tree
                    .name(node.name)
                    .map_or(std::cmp::Ordering::Less, |own| own.cmp(name))
            })
            .ok()?;
        let index = range.start + u32::try_from(found).ok()?;
        Some(Self {
            tree: self.tree,
            id: NodeId(index),
        })
    }

    /// Its path below the target.
    #[must_use]
    pub fn path(&self) -> RelPath {
        let mut components = Vec::new();
        let mut entry = Some(*self);
        while let Some(current) = entry {
            components.extend(current.name().cloned());
            // A directory always comes before what it holds, so the walk up ends.
            entry = current.parent().filter(|parent| parent.id < current.id);
        }
        components.reverse();
        RelPath(components)
    }
}

/// Which entries of a tree may still be removed: the tree's own marks, less what was
/// dropped since. Marks only ever leave [`Mark::Remove`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marks(Vec<Mark>);

impl Marks {
    /// What `tree`'s plan authorized.
    #[must_use]
    pub fn of(tree: &FrozenTree) -> Self {
        Self(tree.marks.clone())
    }

    /// The mark of `id`. An entry these marks don't cover is never authorized.
    #[must_use]
    pub fn get(&self, id: NodeId) -> Mark {
        self.0
            .get(id.index())
            .copied()
            .unwrap_or(Mark::Unauthorized(Unauthorized::Excluded))
    }

    /// Whether `id` may still be removed.
    #[must_use]
    pub fn is_authorized(&self, id: NodeId) -> bool {
        self.get(id) == Mark::Remove
    }

    /// How many entries may still be removed.
    #[must_use]
    pub fn authorized(&self) -> u64 {
        self.0.iter().filter(|mark| **mark == Mark::Remove).count() as u64
    }

    /// Drops `id` and everything below it in `tree` for `reason`. Entries not authorized
    /// any more keep the mark they have.
    pub fn drop_subtree(&mut self, tree: &FrozenTree, id: NodeId, reason: LeftReason) {
        for below in tree.subtree(id) {
            if let Some(mark @ Mark::Remove) = self.0.get_mut(below.index()) {
                *mark = Mark::Dropped(reason);
            }
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = Mark> + '_ {
        self.0.iter().copied()
    }
}

/// Why a [`TreeBuilder`] refused an entry or a tree.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShapeError {
    /// The top is a link. A link is removed by its own name inside a frozen directory,
    /// never frozen as a target.
    #[error("the target is a link")]
    TopIsLink,
    /// A tree of a root's contents must have a directory at the top.
    #[error("a root's contents need a directory at the top")]
    ContentsOfFile,
    /// An entry was added inside something that isn't a directory.
    #[error("an entry was added inside something that isn't a directory")]
    NotADirectory,
    /// An entry is on another file system than the tree.
    #[error("an entry is on another file system")]
    OtherVolume,
    /// A directory holds two entries with one name.
    #[error("a directory holds {0:?} twice")]
    Duplicate(OsString),
    /// More entries than a tree can hold.
    #[error("too many entries")]
    TooLarge,
    /// A parent that this builder didn't hand out.
    #[error("an unknown parent")]
    UnknownParent,
}

/// A directory added to a [`TreeBuilder`], to add entries inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot(u32);

/// Builds a [`FrozenTree`] from entries added in any order, each inside a directory
/// added before it.
#[derive(Debug)]
pub struct TreeBuilder {
    volume: Volume,
    scope: Scope,
    names: HashMap<Component, u32>,
    nodes: Vec<Node>,
}

impl TreeBuilder {
    /// A tree whose top is `top`, on `volume`.
    ///
    /// # Errors
    /// When `top` is a link, is on another volume, or isn't a directory for
    /// [`Scope::Contents`].
    pub fn new(scope: Scope, volume: Volume, top: &EntrySnapshot) -> Result<Self, ShapeError> {
        if top.kind == EntryKind::Symlink {
            return Err(ShapeError::TopIsLink);
        }
        if scope == Scope::Contents && top.kind != EntryKind::Dir {
            return Err(ShapeError::ContentsOfFile);
        }
        if top.identity.volume != volume.device {
            return Err(ShapeError::OtherVolume);
        }
        Ok(Self {
            volume,
            scope,
            names: HashMap::new(),
            nodes: vec![Node::new(NONE, NONE, top)],
        })
    }

    /// The top, to add its entries.
    #[must_use]
    pub const fn top(&self) -> Slot {
        Slot(0)
    }

    /// Adds `entry`, called `name`, inside the directory `parent`. Returns its slot, to
    /// add entries inside it when it is a directory.
    ///
    /// # Errors
    /// When `parent` isn't a directory of this builder, `entry` is on another volume, or
    /// the tree is full.
    pub fn add(
        &mut self,
        parent: Slot,
        name: Component,
        entry: &EntrySnapshot,
    ) -> Result<Slot, ShapeError> {
        let holder = self
            .nodes
            .get(parent.0 as usize)
            .ok_or(ShapeError::UnknownParent)?;
        if holder.kind != EntryKind::Dir {
            return Err(ShapeError::NotADirectory);
        }
        if entry.identity.volume != self.volume.device {
            return Err(ShapeError::OtherVolume);
        }
        let index = u32::try_from(self.nodes.len())
            .ok()
            .filter(|index| *index < NONE)
            .ok_or(ShapeError::TooLarge)?;
        let next = u32::try_from(self.names.len()).map_err(|_| ShapeError::TooLarge)?;
        let name = *self.names.entry(name).or_insert(next);
        self.nodes.push(Node::new(parent.0, name, entry));
        Ok(Slot(index))
    }

    /// The tree, in canonical order.
    ///
    /// # Errors
    /// When a directory holds two entries with one name.
    pub fn finish(self) -> Result<FrozenTree, ShapeError> {
        let mut names: Vec<Option<Component>> = vec![None; self.names.len()];
        for (name, index) in self.names {
            if let Some(slot) = names.get_mut(index as usize) {
                *slot = Some(name);
            }
        }
        let names: Vec<Component> = names.into_iter().flatten().collect();
        canonical(self.volume, self.scope, &names, &self.nodes)
    }
}

/// Lays `built` out in canonical order: breadth first, siblings by name, and names
/// numbered in the order they first appear.
fn canonical(
    volume: Volume,
    scope: Scope,
    names: &[Component],
    built: &[Node],
) -> Result<FrozenTree, ShapeError> {
    let children = siblings(names, built)?;
    let mut order: Vec<u32> = Vec::with_capacity(built.len());
    order.push(0);
    let mut cursor = 0;
    while let Some(&old) = order.get(cursor) {
        order.extend(children.get(old as usize).into_iter().flatten());
        cursor += 1;
    }
    let mut renumber = vec![NONE; built.len()];
    for (new, old) in order.iter().enumerate() {
        if let Some(slot) = renumber.get_mut(*old as usize) {
            *slot = u32::try_from(new).map_err(|_| ShapeError::TooLarge)?;
        }
    }
    let mut renamed: HashMap<u32, u32> = HashMap::new();
    let mut tree = FrozenTree {
        volume,
        scope,
        names: Vec::new(),
        nodes: Vec::with_capacity(built.len()),
        marks: Vec::with_capacity(built.len()),
    };
    let mut end = 1;
    for old in &order {
        let Some(mut node) = built.get(*old as usize).copied() else {
            continue;
        };
        end += children.get(*old as usize).map_or(0, Vec::len);
        node.end = u32::try_from(end).map_err(|_| ShapeError::TooLarge)?;
        node.parent = renumber.get(node.parent as usize).copied().unwrap_or(NONE);
        if node.name != NONE {
            let fresh = u32::try_from(renamed.len()).map_err(|_| ShapeError::TooLarge)?;
            let name = *renamed.entry(node.name).or_insert_with(|| {
                tree.names.extend(names.get(node.name as usize).cloned());
                fresh
            });
            node.name = name;
        }
        tree.nodes.push(node);
        tree.marks.push(Mark::Remove);
    }
    if let (Scope::Contents, Some(top)) = (scope, tree.marks.first_mut()) {
        *top = Mark::Unauthorized(Unauthorized::Top);
    }
    Ok(tree)
}

/// Each built node's children, sorted by name. Fails on a name given twice in one
/// directory.
fn siblings(names: &[Component], built: &[Node]) -> Result<Vec<Vec<u32>>, ShapeError> {
    let mut children: Vec<Vec<u32>> = vec![Vec::new(); built.len()];
    for (index, node) in built.iter().enumerate().skip(1) {
        if let Some(list) = children.get_mut(node.parent as usize) {
            list.push(u32::try_from(index).map_err(|_| ShapeError::TooLarge)?);
        }
    }
    let name_of = |index: &u32| {
        built
            .get(*index as usize)
            .and_then(|node| names.get(node.name as usize))
    };
    for list in &mut children {
        list.sort_unstable_by(|a, b| name_of(a).cmp(&name_of(b)));
        for pair in list.windows(2) {
            if let [a, b] = pair
                && name_of(a) == name_of(b)
            {
                let name = name_of(a).map(|name| name.0.clone()).unwrap_or_default();
                return Err(ShapeError::Duplicate(name));
            }
        }
    }
    Ok(children)
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.below(Path::new("")).display())
    }
}
