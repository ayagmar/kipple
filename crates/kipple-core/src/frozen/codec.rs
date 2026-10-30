//! The stored form of a [`FrozenTree`] (`kipple.frozen-tree`, version 1) and its digest.
//!
//! The plan is stored with its trees before the first intent (D-063), so a crash can be
//! reconciled against what was authorized. The form is compact: little-endian, unsigned
//! LEB128 numbers, signed ones zigzagged first, and no parent pointers, which the
//! breadth-first order implies.
//!
//! ```text
//! magic "kipple-tree\0", version u16
//! scope u8, device, mount flag u8 [mount]
//! name count, node count
//! each name:  length, the codec's bytes
//! each node:  child count, name + 1 (0 for the top), flags u8, links, file ID, size,
//!             changed secs, nanos, modified secs, nanos [born secs, nanos]
//! flags:      kind (bits 0-1), mark (bits 2-3), born present (bit 4)
//! ```
//!
//! Names go through a [`NameCodec`] that the platform supplies, because the OS's own
//! form of a name (bytes on Unix, UTF-16 on Windows) is what must survive, and the core
//! can't reach it. The [digest](FrozenTree::digest) hashes the same layout with each
//! name's in-memory bytes instead, so it needs no codec.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use super::{
    BadName, Component, FrozenTree, Mark, NO_BIRTH, NONE, Node, Scope, Unauthorized, Volume,
};
use crate::digest::{Digest, Hasher};
use crate::probe::EntryKind;

const MAGIC: &[u8] = b"kipple-tree\0";
const VERSION: u16 = 1;

/// Turns a name into the bytes stored for it and back, in the OS's own form. The
/// platform implements it.
pub trait NameCodec: std::fmt::Debug {
    /// The bytes that store `name`.
    fn encode(&self, name: &OsStr) -> Vec<u8>;

    /// The name `bytes` stores, if they store one name on this OS. On Windows that
    /// refuses a name holding `:`, which an NT-relative open reads as a stream of
    /// another entry ([`Component`]).
    fn decode(&self, bytes: &[u8]) -> Option<OsString>;

    /// The path `bytes` store, as [`encode`](Self::encode) gave them for a whole path.
    /// Unlike a name it holds separators, and on Windows its prefix holds a `:`.
    fn decode_path(&self, bytes: &[u8]) -> Option<PathBuf>;
}

/// Why stored bytes aren't a frozen tree.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// They don't start like one.
    #[error("not a stored frozen tree")]
    Magic,
    /// A version this kipple doesn't read.
    #[error("frozen tree version {0} isn't one this kipple reads")]
    Version(u16),
    /// They end early.
    #[error("the stored tree ends early")]
    Truncated,
    /// Bytes follow the tree.
    #[error("bytes follow the stored tree")]
    Trailing,
    /// A stored name isn't one the codec reads, or isn't one name.
    #[error("a stored name is invalid")]
    Name(Option<BadName>),
    /// Something else doesn't hold together: what.
    #[error("the stored tree is malformed: {0}")]
    Malformed(&'static str),
}

/// Where the layout is written: stored bytes, or a digest.
trait Out {
    fn put(&mut self, bytes: &[u8]);
    fn name(&mut self, name: &OsStr);

    fn unsigned(&mut self, mut value: u128) {
        loop {
            let byte = u8::try_from(value & 0x7f).unwrap_or(0x7f);
            value >>= 7;
            if value == 0 {
                self.put(&[byte]);
                return;
            }
            self.put(&[byte | 0x80]);
        }
    }

    fn signed(&mut self, value: i64) {
        #[expect(clippy::cast_sign_loss, reason = "zigzag encoding")]
        let zigzag = ((value << 1) ^ (value >> 63)) as u64;
        self.unsigned(u128::from(zigzag));
    }
}

struct Stored<'codec> {
    bytes: Vec<u8>,
    codec: &'codec dyn NameCodec,
}

impl Out for Stored<'_> {
    fn put(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn name(&mut self, name: &OsStr) {
        let bytes = self.codec.encode(name);
        self.unsigned(bytes.len() as u128);
        self.bytes.extend_from_slice(&bytes);
    }
}

impl Out for Hasher {
    fn put(&mut self, bytes: &[u8]) {
        self.bytes(bytes);
    }

    fn name(&mut self, name: &OsStr) {
        let bytes = name.as_encoded_bytes();
        self.unsigned(bytes.len() as u128);
        self.bytes(bytes);
    }
}

const fn kind_bits(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::Dir => 0,
        EntryKind::File => 1,
        EntryKind::Symlink => 2,
        EntryKind::Other => 3,
    }
}

const fn mark_bits(mark: Mark) -> u8 {
    match mark {
        Mark::Remove | Mark::Dropped(_) => 0,
        Mark::Unauthorized(Unauthorized::Top) => 1,
        Mark::Unauthorized(Unauthorized::Excluded) => 2,
    }
}

impl FrozenTree {
    /// The stored form, its names in `codec`'s form.
    #[must_use]
    pub fn encode(&self, codec: &dyn NameCodec) -> Vec<u8> {
        let mut out = Stored {
            bytes: Vec::new(),
            codec,
        };
        self.write(&mut out);
        out.bytes
    }

    /// The digest of the tree and the plan's marks: equal for equal trees on one
    /// platform, whatever order they were frozen in.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let mut hasher = Hasher::new();
        self.write(&mut hasher);
        hasher.finish()
    }

    /// The digest of the tree with `marks` in place of the plan's: what an intent names
    /// as the set it may remove.
    #[must_use]
    pub fn digest_with(&self, marks: &super::Marks) -> Digest {
        let mut hasher = Hasher::new();
        self.write(&mut hasher);
        for mark in marks.iter() {
            let reason = match mark {
                Mark::Dropped(reason) => reason.name(),
                Mark::Remove | Mark::Unauthorized(_) => "",
            };
            hasher.unsigned(reason.len() as u128);
            hasher.bytes(reason.as_bytes());
        }
        hasher.finish()
    }

    fn write(&self, out: &mut impl Out) {
        out.put(MAGIC);
        out.put(&VERSION.to_le_bytes());
        out.put(&[match self.scope {
            Scope::Whole => 0,
            Scope::Contents => 1,
        }]);
        out.unsigned(u128::from(self.volume.device));
        match self.volume.mount {
            Some(mount) => {
                out.put(&[1]);
                out.unsigned(u128::from(mount));
            }
            None => out.put(&[0]),
        }
        out.unsigned(self.names.len() as u128);
        out.unsigned(self.nodes.len() as u128);
        for name in &self.names {
            out.name(name.as_os_str());
        }
        let mut start = 1;
        for (node, mark) in self.nodes.iter().zip(&self.marks) {
            out.unsigned(u128::from(node.end - start));
            start = node.end;
            out.unsigned(if node.name == NONE {
                0
            } else {
                u128::from(node.name) + 1
            });
            let born = node.nanos[2] != NO_BIRTH;
            out.put(&[kind_bits(node.kind) | (mark_bits(*mark) << 2) | (u8::from(born) << 4)]);
            out.unsigned(u128::from(node.links));
            out.unsigned(u128::from(node.file[0]) | (u128::from(node.file[1]) << 64));
            out.unsigned(u128::from(node.size));
            let stamps = if born { 3 } else { 2 };
            for (secs, nanos) in node.secs.iter().zip(&node.nanos).take(stamps) {
                out.signed(*secs);
                out.unsigned(u128::from(*nanos));
            }
        }
    }

    /// The tree `bytes` store, its names read with `codec`. Stored bytes are untrusted:
    /// everything is checked, and a plan built from a decoded tree is still intersected
    /// with a fresh freeze before anything runs.
    ///
    /// # Errors
    /// When the bytes aren't a version 1 tree that holds together.
    pub fn decode(bytes: &[u8], codec: &dyn NameCodec) -> Result<Self, DecodeError> {
        let mut input = Input(bytes);
        if input.take(MAGIC.len())? != MAGIC {
            return Err(DecodeError::Magic);
        }
        let version = <[u8; 2]>::try_from(input.take(2)?).map_err(|_| DecodeError::Truncated)?;
        let version = u16::from_le_bytes(version);
        if version != VERSION {
            return Err(DecodeError::Version(version));
        }
        let scope = match input.byte()? {
            0 => Scope::Whole,
            1 => Scope::Contents,
            _ => return Err(DecodeError::Malformed("scope")),
        };
        let device = input.u64()?;
        let mount = match input.byte()? {
            0 => None,
            1 => Some(input.u64()?),
            _ => return Err(DecodeError::Malformed("mount flag")),
        };
        let volume = Volume { device, mount };
        let name_count = input.count()?;
        let node_count = input.count()?;
        if node_count == 0 {
            return Err(DecodeError::Malformed("no top entry"));
        }
        let names = read_names(&mut input, name_count, codec)?;
        let mut tree = Self {
            volume,
            scope,
            names,
            nodes: Vec::new(),
            marks: Vec::new(),
        };
        read_nodes(&mut input, &mut tree, node_count)?;
        if !input.0.is_empty() {
            return Err(DecodeError::Trailing);
        }
        Ok(tree)
    }
}

fn read_names(
    input: &mut Input<'_>,
    count: u32,
    codec: &dyn NameCodec,
) -> Result<Vec<Component>, DecodeError> {
    let mut names = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let length = input.count()?;
        let bytes = input.take(length as usize)?;
        let name = codec.decode(bytes).ok_or(DecodeError::Name(None))?;
        names.push(Component::new(name).map_err(|bad| DecodeError::Name(Some(bad)))?);
    }
    let mut seen = HashSet::with_capacity(names.len());
    if !names.iter().all(|name| seen.insert(name)) {
        return Err(DecodeError::Malformed("a name stored twice"));
    }
    Ok(names)
}

/// Reads the nodes, refusing any layout a [`TreeBuilder`](super::TreeBuilder) can't make:
/// every entry's directory comes before it, a subtree left out of the plan has nothing
/// inside marked for removal, and names are numbered in the order entries first use
/// them, each used.
fn read_nodes(input: &mut Input<'_>, tree: &mut FrozenTree, count: u32) -> Result<(), DecodeError> {
    let mut parents = vec![NONE; count as usize];
    let mut next: u32 = 1;
    let mut names_used: u32 = 0;
    for index in 0..count {
        // An entry no directory before it has claimed would be its own directory, or
        // one that comes after it.
        if index > 0 && index >= next {
            return Err(DecodeError::Malformed("an entry outside the tree"));
        }
        let children = input.count()?;
        let end = next
            .checked_add(children)
            .filter(|end| *end <= count)
            .ok_or(DecodeError::Malformed("children past the last entry"))?;
        for child in next..end {
            if let Some(parent) = parents.get_mut(child as usize) {
                *parent = index;
            }
        }
        next = end;
        let parent = parents.get(index as usize).copied().unwrap_or(NONE);
        let (node, mark) = read_node(input, tree, parent, end)?;
        if children > 0 && node.kind != EntryKind::Dir {
            return Err(DecodeError::Malformed(
                "entries inside something not a directory",
            ));
        }
        check_place(tree, &node, mark)?;
        check_name_order(&node, &mut names_used)?;
        tree.nodes.push(node);
        tree.marks.push(mark);
    }
    if next != count {
        return Err(DecodeError::Malformed("entries outside the tree"));
    }
    if names_used as usize != tree.names.len() {
        return Err(DecodeError::Malformed("a name no entry uses"));
    }
    Ok(())
}

/// Checks that `node`'s name is one used before or the next one: names are numbered in
/// the order entries first use them, so one tree has one name table.
const fn check_name_order(node: &Node, used: &mut u32) -> Result<(), DecodeError> {
    if node.name == NONE || node.name < *used {
        return Ok(());
    }
    if node.name > *used {
        return Err(DecodeError::Malformed("names out of order"));
    }
    *used += 1;
    Ok(())
}

fn read_node(
    input: &mut Input<'_>,
    tree: &FrozenTree,
    parent: u32,
    end: u32,
) -> Result<(Node, Mark), DecodeError> {
    let name = match u32::try_from(input.unsigned()?) {
        Ok(0) => NONE,
        Ok(name) => name - 1,
        Err(_) => return Err(DecodeError::Malformed("an unknown name")),
    };
    if name != NONE && name as usize >= tree.names.len() {
        return Err(DecodeError::Malformed("an unknown name"));
    }
    let flags = input.byte()?;
    let kind = match flags & 0b11 {
        0 => EntryKind::Dir,
        1 => EntryKind::File,
        2 => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    let mark = match (flags >> 2) & 0b11 {
        0 => Mark::Remove,
        1 => Mark::Unauthorized(Unauthorized::Top),
        2 => Mark::Unauthorized(Unauthorized::Excluded),
        _ => return Err(DecodeError::Malformed("mark")),
    };
    let born = flags & 0b1_0000 != 0;
    if flags >> 5 != 0 {
        return Err(DecodeError::Malformed("flags"));
    }
    let links = u32::try_from(input.unsigned()?).map_err(|_| DecodeError::Malformed("links"))?;
    let file = input.unsigned()?;
    let size = input.u64()?;
    let mut secs = [0; 3];
    let mut nanos = [0, 0, NO_BIRTH];
    let stamps = if born { 3 } else { 2 };
    for (sec, nano) in secs.iter_mut().zip(nanos.iter_mut()).take(stamps) {
        *sec = input.signed()?;
        *nano = u32::try_from(input.unsigned()?)
            .ok()
            .filter(|value| *value < super::NANOS)
            .ok_or(DecodeError::Malformed("nanoseconds"))?;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the file ID is split in halves"
    )]
    let file = [file as u64, (file >> 64) as u64];
    let node = Node {
        parent,
        name,
        end,
        links,
        kind,
        file,
        size,
        secs,
        nanos,
    };
    Ok((node, mark))
}

/// Checks that the node about to be added has a name exactly when it isn't the top, a
/// name greater than its previous sibling's, the top's mark the scope implies, and no
/// removal inside a directory left out of the plan.
fn check_place(tree: &FrozenTree, node: &Node, mark: Mark) -> Result<(), DecodeError> {
    let top = tree.nodes.is_empty();
    if top != (node.name == NONE) {
        return Err(DecodeError::Malformed("a name on the top or none below it"));
    }
    if top {
        let expected = tree.scope == Scope::Contents;
        let marked = mark == Mark::Unauthorized(Unauthorized::Top);
        if expected != marked || (expected && node.kind != EntryKind::Dir) {
            return Err(DecodeError::Malformed(
                "the top's mark doesn't match the scope",
            ));
        }
        if node.kind == EntryKind::Symlink {
            return Err(DecodeError::Malformed("the top is a link"));
        }
        return Ok(());
    }
    if mark == Mark::Unauthorized(Unauthorized::Top) {
        return Err(DecodeError::Malformed("a top mark below the top"));
    }
    let excluded = Mark::Unauthorized(Unauthorized::Excluded);
    if tree.marks.get(node.parent as usize) == Some(&excluded) && mark != excluded {
        return Err(DecodeError::Malformed("a removal inside an excluded entry"));
    }
    if let Some(previous) = tree.nodes.last()
        && previous.parent == node.parent
        && tree.name(previous.name) >= tree.name(node.name)
    {
        return Err(DecodeError::Malformed("siblings out of order"));
    }
    Ok(())
}

/// What is left of the stored bytes.
struct Input<'bytes>(&'bytes [u8]);

impl<'bytes> Input<'bytes> {
    const fn take(&mut self, count: usize) -> Result<&'bytes [u8], DecodeError> {
        if count > self.0.len() {
            return Err(DecodeError::Truncated);
        }
        let (taken, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(taken)
    }

    fn byte(&mut self) -> Result<u8, DecodeError> {
        let [byte] = self.take(1)? else {
            return Err(DecodeError::Truncated);
        };
        Ok(*byte)
    }

    fn unsigned(&mut self) -> Result<u128, DecodeError> {
        let mut value: u128 = 0;
        for shift in (0..128).step_by(7) {
            let byte = self.byte()?;
            let bits = u128::from(byte & 0x7f);
            if shift == 126 && bits > 0b11 {
                return Err(DecodeError::Malformed("a number too large"));
            }
            value |= bits << shift;
            // A last byte of 0 after the first is an overlong form of a shorter number:
            // only the shortest form is stored, so one tree has one encoding.
            match (byte, shift) {
                (0, 1..) => return Err(DecodeError::Malformed("an overlong number")),
                (0..0x80, _) => return Ok(value),
                _ => {}
            }
        }
        Err(DecodeError::Malformed("a number too large"))
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        u64::try_from(self.unsigned()?).map_err(|_| DecodeError::Malformed("a number too large"))
    }

    /// A count, which can't be more than the bytes left could hold.
    fn count(&mut self) -> Result<u32, DecodeError> {
        let count = u32::try_from(self.unsigned()?)
            .ok()
            .filter(|count| *count < NONE)
            .ok_or(DecodeError::Malformed("a count too large"))?;
        if count as usize > self.0.len() {
            return Err(DecodeError::Truncated);
        }
        Ok(count)
    }

    fn signed(&mut self) -> Result<i64, DecodeError> {
        let zigzag = self.u64()?;
        #[expect(clippy::cast_possible_wrap, reason = "zigzag decoding")]
        let value = ((zigzag >> 1) as i64) ^ -((zigzag & 1) as i64);
        Ok(value)
    }
}
