//! Running a delete: removing what an operation authorizes, entry by entry, each checked
//! on its open handle first, then making the removals durable before returning: each
//! directory that had an entry removed and is still there is synced, the item's own
//! directory included (D-065). A directory removed in turn isn't: once its removal from
//! its parent is durable, nothing in it can come back.

use std::collections::HashSet;
use std::ffi::OsStr;

use kipple_core::{
    Allocated, ApplyError, CancelToken, Component, DeleteOp, EffectSink, Entry, EntryKind,
    ErrorCode, FileSystemClass, Finish, Freed, FrozenTree, LeftReason, Marks, NodeId, Scope,
    Volume,
};

use super::{Form, Seen, drift, left_reason, reach, sys};

/// Runs `op`. See [`Executor`](kipple_core::Executor).
pub(super) fn delete(
    op: &DeleteOp,
    effects: &mut dyn EffectSink,
    cancel: &CancelToken,
) -> Result<Finish, ApplyError> {
    let target = op.target();
    let tree = target.tree();
    // A root's contents removed for a target below it, or the reverse, would remove
    // something other than what the anchor names.
    if tree.scope() != target.anchor().scope() {
        return Err(ApplyError::new(ErrorCode::TargetChanged));
    }
    let reached = reach(target.anchor())?;
    if let FileSystemClass::Unsupported(_) = reached.file_system {
        return Err(ApplyError::new(ErrorCode::UnsupportedFileSystem));
    }
    if tree.volume() != reached.volume {
        return Err(ApplyError::new(ErrorCode::RootChanged));
    }
    let mut run = Run {
        tree,
        marks: target.marks(),
        effects,
        volume: reached.volume,
        unsynced: None,
    };
    let finish = match tree.scope() {
        Scope::Contents => run.contents(reached.root, &reached.root_seen, cancel),
        Scope::Whole => {
            let last = target.anchor().relative.components().last();
            let name = last.and_then(|last| sys::name(last.as_os_str()).ok());
            run.whole(reached.holder(), name, cancel)
        }
    };
    // A removal a power loss could undo is never reported as done.
    run.unsynced.map_or(finish, Err)
}

/// One delete in progress.
struct Run<'run> {
    tree: &'run FrozenTree,
    marks: &'run Marks,
    effects: &'run mut dyn EffectSink,
    volume: Volume,
    /// The first sync that failed: the run's removals aren't all durable.
    unsynced: Option<ApplyError>,
}

/// A frozen directory being emptied: its open handle, its name in the directory holding
/// it (none for a root), its node, what it was when it was opened, its children not
/// visited yet, and whether an entry was removed from it.
struct Frame<'run> {
    dir: sys::Dir,
    name: Option<sys::Name>,
    node: NodeId,
    seen: Seen,
    children: Box<dyn Iterator<Item = Entry<'run>> + 'run>,
    dirty: bool,
}

impl<'run> Frame<'run> {
    fn new(dir: sys::Dir, name: Option<sys::Name>, entry: Entry<'run>, seen: Seen) -> Self {
        Self {
            dir,
            name,
            node: entry.id(),
            seen,
            children: Box::new(entry.children()),
            dirty: false,
        }
    }
}

/// What visiting one entry did.
enum Visit<'run> {
    /// It was left or passed over.
    Done,
    /// It was removed from the directory it was in, and reported.
    Removed,
    /// It is a directory, entered: its entries come next.
    Entered(Box<Frame<'run>>),
}

impl<'run> Run<'run> {
    /// The contents of a verified root, which itself is never removed.
    fn contents(
        &mut self,
        root: sys::Dir,
        seen: &Seen,
        cancel: &CancelToken,
    ) -> Result<Finish, ApplyError> {
        let top = self.tree.top();
        if let Some(reason) = changed(&root, top, seen) {
            self.leave(top, reason);
            return Ok(Finish::Completed);
        }
        self.descend(Frame::new(root, None, top, *seen), None, cancel)
    }

    /// A target below its root, called `name` in the directory `holder`.
    fn whole(
        &mut self,
        holder: &sys::Dir,
        name: Option<sys::Name>,
        cancel: &CancelToken,
    ) -> Result<Finish, ApplyError> {
        if cancel.is_cancelled() {
            return Ok(Finish::Cancelled);
        }
        match self.visit(holder, self.tree.top(), name)? {
            Visit::Done => Ok(Finish::Completed),
            Visit::Removed => {
                self.sync(holder);
                Ok(Finish::Completed)
            }
            Visit::Entered(frame) => self.descend(*frame, Some(holder), cancel),
        }
    }

    /// Empties `top`'s directory depth first, then removes it from `holder` when it may.
    /// However the walk ends, each directory it leaves open is synced if it had an entry
    /// removed.
    fn descend(
        &mut self,
        top: Frame<'run>,
        holder: Option<&sys::Dir>,
        cancel: &CancelToken,
    ) -> Result<Finish, ApplyError> {
        let mut stack = vec![top];
        let finish = self.walk(&mut stack, holder, cancel);
        for frame in stack.iter().rev() {
            self.settle(frame);
        }
        finish
    }

    fn walk(
        &mut self,
        stack: &mut Vec<Frame<'run>>,
        holder: Option<&sys::Dir>,
        cancel: &CancelToken,
    ) -> Result<Finish, ApplyError> {
        while let Some(frame) = stack.last_mut() {
            if cancel.is_cancelled() {
                return Ok(Finish::Cancelled);
            }
            let Some(child) = frame.children.next() else {
                self.finish_dir(stack, holder);
                continue;
            };
            let name = child
                .name()
                .and_then(|name| sys::name(name.as_os_str()).ok());
            match self.visit(&frame.dir, child, name)? {
                Visit::Done => {}
                Visit::Removed => frame.dirty = true,
                Visit::Entered(below) => stack.push(*below),
            }
        }
        Ok(Finish::Completed)
    }

    /// Pops the emptied directory on top of `stack` and removes it from the one below it,
    /// or from `holder` for the target itself. The root of a root's contents stays, and
    /// is synced if it had an entry removed.
    fn finish_dir(&mut self, stack: &mut Vec<Frame<'run>>, holder: Option<&sys::Dir>) {
        let Some(done) = stack.pop() else { return };
        let Some(parent) = stack.last().map(|frame| &frame.dir).or(holder) else {
            self.settle(&done);
            return;
        };
        if !self.remove_dir(parent, done) {
            return;
        }
        match stack.last_mut() {
            Some(frame) => frame.dirty = true,
            // The target itself went: its directory is synced now, as nothing else in it
            // changes.
            None => {
                if let Some(holder) = holder {
                    self.sync(holder);
                }
            }
        }
    }

    /// Syncs `frame`'s directory if an entry was removed from it.
    fn settle(&mut self, frame: &Frame<'run>) {
        if frame.dirty {
            self.sync(&frame.dir);
        }
    }

    /// Syncs `dir`, keeping the first failure.
    fn sync(&mut self, dir: &sys::Dir) {
        if let Err(error) = dir.sync() {
            self.unsynced
                .get_or_insert_with(|| ApplyError::io(ErrorCode::Unsynced, error));
        }
    }

    fn entry(&self, id: NodeId) -> Entry<'run> {
        self.tree.entry(id).unwrap_or_else(|| self.tree.top())
    }

    /// Visits `entry`, called `name` in `dir`: removes a file or link after checking it,
    /// or opens and checks a directory. An entry the run may not touch is passed over:
    /// everything below one the plan left out or revalidation dropped is out too.
    fn visit(
        &mut self,
        dir: &sys::Dir,
        entry: Entry<'run>,
        name: Option<sys::Name>,
    ) -> Result<Visit<'run>, ApplyError> {
        if !self.marks.is_authorized(entry.id()) {
            return Ok(Visit::Done);
        }
        let Some(name) = name else {
            self.leave(entry, LeftReason::Failed);
            return Ok(Visit::Done);
        };
        if entry.kind() == EntryKind::Dir {
            return self.enter(dir, entry, name);
        }
        let victim = match dir.take(&name) {
            Ok(victim) => victim,
            Err(error) => {
                self.effects.left(entry.id(), left_reason(&error));
                return Ok(Visit::Done);
            }
        };
        let seen = *victim.seen();
        self.confined(entry, &seen)?;
        if seen.form == Form::Placeholder {
            self.effects.left(entry.id(), LeftReason::Unsupported);
            return Ok(Visit::Done);
        }
        if let Some(reason) = drift(entry, &seen) {
            self.effects.left(entry.id(), reason);
            return Ok(Visit::Done);
        }
        match victim.remove() {
            Ok(()) => {
                self.effects.removed(entry.id(), freed(&seen));
                Ok(Visit::Removed)
            }
            Err(error) => {
                self.effects.left(entry.id(), left_reason(&error));
                Ok(Visit::Done)
            }
        }
    }

    /// Opens the directory `entry` and checks it once, before anything below it is
    /// touched. One that changed is left with everything inside it.
    fn enter(
        &mut self,
        dir: &sys::Dir,
        entry: Entry<'run>,
        name: sys::Name,
    ) -> Result<Visit<'run>, ApplyError> {
        let opened = dir.open_dir(&name).and_then(|sub| {
            let seen = sub.seen()?;
            Ok((sub, seen))
        });
        let (sub, seen) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                let reason = if sys::is_link_or_file(&error) {
                    LeftReason::Replaced
                } else {
                    left_reason(&error)
                };
                self.leave(entry, reason);
                return Ok(Visit::Done);
            }
        };
        self.confined(entry, &seen)?;
        if let Some(reason) = changed(&sub, entry, &seen) {
            self.leave(entry, reason);
            return Ok(Visit::Done);
        }
        Ok(Visit::Entered(Box::new(Frame::new(
            sub,
            Some(name),
            entry,
            seen,
        ))))
    }

    /// Stops the run when `seen` is on another mount: what lies there is unknown.
    fn confined(&self, entry: Entry<'_>, seen: &Seen) -> Result<(), ApplyError> {
        if seen.on(self.volume) {
            Ok(())
        } else {
            Err(ApplyError::new(ErrorCode::MountBoundary).at(entry.path()))
        }
    }

    /// Removes the emptied directory `done` from `holder`, if the run may: only while it
    /// is still the directory frozen, and only once it is empty. Whether it was removed;
    /// one that stays is synced if an entry was removed from it.
    fn remove_dir(&mut self, holder: &sys::Dir, done: Frame<'run>) -> bool {
        if !self.marks.is_authorized(done.node) {
            self.settle(&done);
            return false;
        }
        let (Some(name), Some(frozen)) = (&done.name, self.entry(done.node).snapshot()) else {
            self.effects.left(done.node, LeftReason::Failed);
            self.settle(&done);
            return false;
        };
        let (freed, dirty) = (freed(&done.seen), done.dirty);
        let reason = match holder.remove_dir(name, done.dir, frozen.identity) {
            Ok(()) => {
                self.effects.removed(done.node, freed);
                return true;
            }
            Err(sys::Refusal::Replaced) => LeftReason::Replaced,
            Err(sys::Refusal::Os(error)) => left_reason(&error),
        };
        self.effects.left(done.node, reason);
        // Its handle went with the attempt: it is synced by name, if it is still there.
        if dirty && let Err(error) = holder.sync_named(name, frozen.identity) {
            self.unsynced
                .get_or_insert_with(|| ApplyError::io(ErrorCode::Unsynced, error));
        }
        false
    }

    /// Leaves `entry` and every entry below it the run may touch, for `reason`.
    fn leave(&mut self, entry: Entry<'_>, reason: LeftReason) {
        let mut stack = vec![entry];
        while let Some(next) = stack.pop() {
            if self.marks.is_authorized(next.id()) {
                self.effects.left(next.id(), reason);
            }
            stack.extend(next.children());
        }
    }
}

/// How the directory `dir`, frozen as `entry` and read now as `seen`, differs from what
/// was frozen: its fingerprint, or a name it holds now that the freeze didn't list. A
/// clock that ticks coarsely (Windows stamps a change with a time that moves every 15.6
/// ms or so) can leave a directory's times as they were when an entry is added to it.
fn changed(dir: &sys::Dir, entry: Entry<'_>, seen: &Seen) -> Option<LeftReason> {
    if let Some(reason) = drift(entry, seen) {
        return Some(reason);
    }
    let listed = match dir.names() {
        Ok(listed) => listed,
        Err(error) => return Some(left_reason(&error)),
    };
    let frozen: HashSet<&OsStr> = entry
        .children()
        .filter_map(|child| child.name())
        .map(Component::as_os_str)
        .collect();
    listed
        .iter()
        .any(|name| !frozen.contains(name.as_os_str()))
        .then_some(LeftReason::Changed)
}

/// What removing `seen` frees: nothing for a file that keeps another name.
fn freed(seen: &Seen) -> Freed {
    if seen.snapshot.kind != EntryKind::Dir && seen.snapshot.links > 1 {
        return Freed::Shared;
    }
    seen.allocated.map_or(Freed::Unmeasured, |bytes| {
        Freed::Allocated(Allocated(bytes))
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use kipple_core::{Executor as _, Found, Freezer as _, ItemOutcome, Tally};
    use tempfile::TempDir;

    use super::super::ConfinedFs;
    use super::super::faults::{anchor, fail_syncs_after};
    use super::*;

    /// Catches a removal reported done although its directory couldn't be synced, so a
    /// power loss could bring it back (D-065): the item is indeterminate, never done.
    #[test]
    fn a_removal_whose_directory_sync_fails_is_indeterminate() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("target/sub")).unwrap();
        fs::write(temp.path().join("target/sub/a"), b"a").unwrap();
        let anchor = anchor(temp.path(), "target");
        let tree = ConfinedFs
            .freeze(&anchor, &CancelToken::new())
            .unwrap()
            .tree;
        let marks = Marks::of(&tree);
        let op = DeleteOp::for_platform_tests(anchor, tree, marks);
        let mut tally = Tally::new(op.target().tree(), op.target().marks());
        let _failing = fail_syncs_after(0);

        let ran = ConfinedFs.delete(&op, &mut tally, &CancelToken::new());
        let outcome = tally.finish(ran);

        assert!(
            matches!(&outcome, ItemOutcome::Indeterminate(Found::Unsynced(_))),
            "{outcome:?}"
        );
        assert!(
            fs::symlink_metadata(temp.path().join("target")).is_err(),
            "the removal itself happened"
        );
    }
}
