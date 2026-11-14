//! What a native item's run did (`docs/04-safety-model.md` §7, §8, §8a; spec M4 pinned
//! 28–32).
//!
//! The platform's runner checks the tool and the target by identity, starts the command
//! from the target's checked handle, waits for it and makes its effect durable before it
//! returns. What comes back is settled here, and only one way leads to `done`: the command
//! exited 0 by itself, the runner saw its postcondition, the sync barrier held, and for a
//! worktree git, asked once more, no longer lists it. Anything else that may have changed
//! the target is `indeterminate`, never done: a missed deadline, a failed sync, git unable
//! to say, or a target that isn't as it was. Only a run that changed nothing is `failed`.
//! A command that never started did nothing and is `failed` with no effect.
//!
//! Nothing was enumerated, so the receipt measures the target before the re-check and
//! after the run, and says "not measured" when either fell short. What the command printed
//! goes back to the frontend, which shows it once; it is never recorded.

use std::path::Path;

use super::{ApplyPorts, in_full, reconcile};
use crate::authorize::NativeOp;
use crate::cancel::CancelToken;
use crate::native::{NativeCommand, NativeExit, NativeOutput, NativeStatus, Postcondition};
use crate::outcome::{ApplyError, Effect, ErrorCode, Found, ItemOutcome, Measured, NativeEffect};
use crate::size::Allocated;

/// How a native item ended, and what its command printed.
#[derive(Debug)]
pub(super) struct Ran {
    pub(super) outcome: ItemOutcome,
    pub(super) output: Option<NativeOutput>,
}

/// What git lists of a worktree after its removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Listed {
    /// Nothing to ask: the command isn't a worktree's removal.
    NotAsked,
    /// git no longer lists it.
    Gone,
    /// git still lists it.
    Named,
    /// git couldn't be asked, or there is no repository to ask.
    Unknown,
}

/// Runs `op`, whose target allocated `before` when it was measured in full, and settles
/// what it did.
pub(super) fn run(
    op: &NativeOp,
    before: Option<Allocated>,
    ports: &ApplyPorts<'_>,
    cancel: &CancelToken,
) -> Ran {
    match ports.mutation.runner.run(op) {
        Ok(exit) => {
            let NativeExit {
                status,
                postcondition,
                synced,
                output,
            } = exit;
            let ended = Ended {
                status,
                postcondition,
                synced,
            };
            Ran {
                outcome: settle(op, &ended, before, ports, cancel),
                output: Some(output),
            }
        }
        Err(error) => Ran {
            outcome: ItemOutcome::Failed {
                error,
                effect: None,
            },
            output: None,
        },
    }
}

/// What the runner saw, past the output.
#[derive(Debug)]
struct Ended {
    status: NativeStatus,
    postcondition: Postcondition,
    synced: Result<(), ApplyError>,
}

/// The outcome of `op` once its command ended as `ended` says.
fn settle(
    op: &NativeOp,
    ended: &Ended,
    before: Option<Allocated>,
    ports: &ApplyPorts<'_>,
    cancel: &CancelToken,
) -> ItemOutcome {
    let end = ended.status.end();
    if let NativeStatus::TimedOut(_) = ended.status {
        // Its own child was killed; what it started may still be at work.
        return ItemOutcome::Indeterminate(Found::Unproven(ApplyError::native(
            ErrorCode::NativeTimedOut,
            end,
        )));
    }
    if let Err(error) = &ended.synced {
        return ItemOutcome::Indeterminate(Found::Unsynced(error.clone()));
    }
    // What runs after the command is the item's own evidence, so a Ctrl+C pressed while
    // it ran doesn't leave a removal it can't confirm.
    let whole_run = CancelToken::new();
    let listed = listed(op, ports, &whole_run);
    let path = op.target().anchor().path();
    let effect = Effect::Native(NativeEffect {
        exit: end,
        reclaimed: reclaimed(before, ended.postcondition, &path, ports, cancel),
    });
    let confirmed = matches!(listed, Listed::NotAsked | Listed::Gone);
    if end.succeeded() && ended.postcondition == Postcondition::Met && confirmed {
        return ItemOutcome::Done(effect);
    }
    let code = if end.succeeded() {
        ErrorCode::NativeUnconfirmed
    } else {
        ErrorCode::NativeFailed
    };
    let error = ApplyError::native(code, end);
    let still_listed = matches!(listed, Listed::NotAsked | Listed::Named);
    if still_listed && unchanged(op, ports, &whole_run) {
        ItemOutcome::Failed {
            error,
            effect: Some(effect),
        }
    } else {
        ItemOutcome::Indeterminate(Found::Unproven(error))
    }
}

/// What git lists of `op`'s worktree now, through the repository `op` was admitted with.
fn listed(op: &NativeOp, ports: &ApplyPorts<'_>, cancel: &CancelToken) -> Listed {
    let NativeCommand::GitWorktreeRemove { .. } = op.spec().command else {
        return Listed::NotAsked;
    };
    let Some(repository) = op.repository() else {
        return Listed::Unknown;
    };
    let reviewed = op.target().anchor().path();
    let Ok(registrations) = ports.mutation.worktrees.list(repository, cancel) else {
        return Listed::Unknown;
    };
    let names = |path: &Path| path == reviewed || op.registered() == Some(path);
    if registrations
        .iter()
        .any(|registration| names(&registration.path))
    {
        Listed::Named
    } else {
        Listed::Gone
    }
}

/// What the command freed: `before` against what the target allocates now. A target the
/// runner saw gone allocates nothing; one still there is measured again.
fn reclaimed(
    before: Option<Allocated>,
    postcondition: Postcondition,
    path: &Path,
    ports: &ApplyPorts<'_>,
    cancel: &CancelToken,
) -> Measured {
    let after = match postcondition {
        Postcondition::Met => Some(Allocated(0)),
        Postcondition::Unmet => in_full(ports.rescan.size(path, cancel)),
    };
    match (before, after) {
        (Some(before), Some(after)) => Measured::Bytes { before, after },
        (None, _) | (_, None) => Measured::Unknown,
    }
}

/// Whether every entry of `op`'s reviewed tree is still there, the same file: a fresh
/// freeze compared as reconciliation compares one.
fn unchanged(op: &NativeOp, ports: &ApplyPorts<'_>, cancel: &CancelToken) -> bool {
    let target = op.target();
    ports
        .mutation
        .freezer
        .freeze(target.anchor(), cancel)
        .is_ok_and(|fresh| reconcile::compare(target.tree(), &fresh) == Found::Unchanged)
}
