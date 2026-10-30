//! Running a delete: removing what an operation authorizes, entry by entry, each checked
//! on its open handle first.

use kipple_core::{
    Allocated, ApplyError, CancelToken, DeleteOp, EffectSink, Entry, EntryKind, ErrorCode,
    FileSystemClass, Finish, Freed, FrozenTree, LeftReason, Marks, NodeId, Scope, Volume,
};

use super::{Form, Seen, left_reason, reach, sys};

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
    };
    match tree.scope() {
        Scope::Contents => run.contents(reached.root, &reached.root_seen, cancel),
        Scope::Whole => {
            let last = target.anchor().relative.components().last();
            let name = last.and_then(|last| sys::name(last.as_os_str()).ok());
            run.whole(reached.holder(), name, cancel)
        }
    }
}

/// One delete in progress.
struct Run<'run> {
    tree: &'run FrozenTree,
    marks: &'run Marks,
    effects: &'run mut dyn EffectSink,
    volume: Volume,
}

/// A frozen directory being emptied: its open handle, its name in the directory holding
/// it (none for a root), its node, what it was when it was opened, and its children not
/// visited yet.
struct Frame<'run> {
    dir: sys::Dir,
    name: Option<sys::Name>,
    node: NodeId,
    seen: Seen,
    children: Box<dyn Iterator<Item = Entry<'run>> + 'run>,
}

impl<'run> Frame<'run> {
    fn new(dir: sys::Dir, name: Option<sys::Name>, entry: Entry<'run>, seen: Seen) -> Self {
        Self {
            dir,
            name,
            node: entry.id(),
            seen,
            children: Box::new(entry.children()),
        }
    }
}

/// What visiting one entry did.
enum Visit<'run> {
    /// It was removed or left, and reported.
    Done,
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
        if let Some(reason) = drift(top, seen) {
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
            Visit::Entered(frame) => self.descend(*frame, Some(holder), cancel),
        }
    }

    /// Empties `top`'s directory depth first, then removes it from `holder` when it may.
    fn descend(
        &mut self,
        top: Frame<'run>,
        holder: Option<&sys::Dir>,
        cancel: &CancelToken,
    ) -> Result<Finish, ApplyError> {
        let mut stack = vec![top];
        while let Some(frame) = stack.last_mut() {
            if cancel.is_cancelled() {
                return Ok(Finish::Cancelled);
            }
            let Some(child) = frame.children.next() else {
                self.finish_dir(&mut stack, holder);
                continue;
            };
            let name = child
                .name()
                .and_then(|name| sys::name(name.as_os_str()).ok());
            if let Visit::Entered(below) = self.visit(&frame.dir, child, name)? {
                stack.push(*below);
            }
        }
        Ok(Finish::Completed)
    }

    /// Pops the emptied directory on top of `stack` and removes it from the one below it,
    /// or from `holder` for the target itself. The root of a root's contents stays.
    fn finish_dir(&mut self, stack: &mut Vec<Frame<'run>>, holder: Option<&sys::Dir>) {
        let Some(done) = stack.pop() else { return };
        let parent = stack.last().map(|frame| &frame.dir).or(holder);
        if let Some(parent) = parent {
            self.remove_dir(parent, done);
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
            Ok(()) => self.effects.removed(entry.id(), freed(&seen)),
            Err(error) => self.effects.left(entry.id(), left_reason(&error)),
        }
        Ok(Visit::Done)
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
        if let Some(reason) = drift(entry, &seen) {
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
    /// is still the directory frozen, and only once it is empty.
    fn remove_dir(&mut self, holder: &sys::Dir, done: Frame<'run>) {
        if !self.marks.is_authorized(done.node) {
            return;
        }
        let (Some(name), Some(frozen)) = (&done.name, self.entry(done.node).snapshot()) else {
            self.effects.left(done.node, LeftReason::Failed);
            return;
        };
        let freed = freed(&done.seen);
        match holder.remove_dir(name, done.dir, frozen.identity) {
            Ok(()) => self.effects.removed(done.node, freed),
            Err(sys::Refusal::Replaced) => self.effects.left(done.node, LeftReason::Replaced),
            Err(sys::Refusal::Os(error)) => self.effects.left(done.node, left_reason(&error)),
        }
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

/// How `seen` differs from the frozen `entry`, as a reason to leave it.
fn drift(entry: Entry<'_>, seen: &Seen) -> Option<LeftReason> {
    let Some(frozen) = entry.snapshot() else {
        return Some(LeftReason::Gone);
    };
    frozen.drift(&seen.snapshot).map(|drift| match drift {
        kipple_core::Drift::Replaced => LeftReason::Replaced,
        kipple_core::Drift::Changed => LeftReason::Changed,
    })
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
