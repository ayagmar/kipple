//! Revalidating a frozen tree against a fresh freeze (`docs/04-safety-model.md` §3.6).

use std::cmp::Ordering;

use super::{Drift, Entry, FrozenTree, Mark, Marks, NodeId, Unauthorized};
use crate::outcome::LeftReason;
use crate::probe::EntryKind;

impl FrozenTree {
    /// What of this tree a fresh freeze of the same target still authorizes: the plan's
    /// marks, with every entry the fresh tree doesn't confirm dropped for its reason.
    ///
    /// An entry stays only where the fresh tree has the same name in the same directory,
    /// of the same kind and identity, with a fingerprint that [`EntrySnapshot::drift`]
    /// finds unchanged, and still marked for removal: an entry the fresh plan leaves out
    /// (a file git tracks now) is dropped as excluded. A directory that is gone, replaced,
    /// changed or left out is dropped with everything inside it, each entry counted under
    /// that reason: a changed directory may have gained an entry, and nothing below it is
    /// entered. A tree on another file system or mount is replaced as a whole. Entries
    /// only the fresh tree has are not looked at: they were never authorized.
    ///
    /// The result is always this tree's own marks with some [`Mark::Remove`] turned into
    /// [`Mark::Dropped`]: a subset, never more.
    ///
    /// [`EntrySnapshot::drift`]: super::EntrySnapshot::drift
    #[must_use]
    pub fn recheck(&self, fresh: &Self) -> Marks {
        let mut marks = Marks::of(self);
        if self.volume != fresh.volume || self.scope != fresh.scope {
            marks.drop_subtree(self, NodeId::TOP, LeftReason::Replaced);
            return marks;
        }
        let mut pairs = vec![(self.top(), fresh.top())];
        while let Some((frozen, now)) = pairs.pop() {
            if let Some(reason) = withheld(now).or_else(|| drift(frozen, now)) {
                marks.drop_subtree(self, frozen.id(), reason);
                continue;
            }
            if frozen.kind() == EntryKind::Dir {
                match_children(frozen, now, &mut marks, &mut pairs);
            }
        }
        marks
    }
}

/// Why the fresh plan doesn't authorize `now`, if it doesn't. The top of a root's
/// contents is never authorized in either tree (the scopes match by now), and what is
/// inside it is judged entry by entry.
fn withheld(now: Entry<'_>) -> Option<LeftReason> {
    match now.mark() {
        Mark::Remove | Mark::Unauthorized(Unauthorized::Top) => None,
        Mark::Unauthorized(Unauthorized::Excluded) => Some(LeftReason::Excluded),
        Mark::Dropped(reason) => Some(reason),
    }
}

fn drift(frozen: Entry<'_>, now: Entry<'_>) -> Option<LeftReason> {
    let (Some(then), Some(now)) = (frozen.snapshot(), now.snapshot()) else {
        return Some(LeftReason::Gone);
    };
    then.drift(&now).map(|drift| match drift {
        Drift::Replaced => LeftReason::Replaced,
        Drift::Changed => LeftReason::Changed,
    })
}

/// Pairs `frozen`'s children with `now`'s by name, both sorted, dropping those `now`
/// doesn't have.
fn match_children<'tree>(
    frozen: Entry<'tree>,
    now: Entry<'tree>,
    marks: &mut Marks,
    pairs: &mut Vec<(Entry<'tree>, Entry<'tree>)>,
) {
    let tree = frozen.tree;
    let mut fresh = now.children().peekable();
    for child in frozen.children() {
        let found = loop {
            let Some(candidate) = fresh.peek() else {
                break None;
            };
            match candidate.name().cmp(&child.name()) {
                Ordering::Less => {
                    fresh.next();
                }
                Ordering::Equal => break fresh.next(),
                Ordering::Greater => break None,
            }
        };
        match found {
            Some(fresh_child) => pairs.push((child, fresh_child)),
            None => marks.drop_subtree(tree, child.id(), LeftReason::Gone),
        }
    }
}
