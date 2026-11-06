//! Authorization: the one place an operation that may change files is made
//! (`docs/04-safety-model.md` §1, §2, §3.5, §3.6).
//!
//! An [`AuthorizedOp`] has no public constructor. Only revalidation in this crate makes
//! one, from an item of a [`Plan`], the user's [`Authorization`] and the item's facts
//! established afresh at apply. A frontend, the platform or a plan file can't make one,
//! so nothing reaches the [`Executor`](crate::Executor) or the
//! [`TrashBackend`](crate::TrashBackend) without passing through it.
//!
//! A delete and a trash move are different types: a [`TrashOp`] can't become a
//! [`DeleteOp`], so a failed trash move can't fall back to deleting.
//!
//! ```compile_fail,E0616
//! // A trash item can't be turned into a delete.
//! fn fall_back(op: kipple_core::TrashOp) -> kipple_core::DeleteOp {
//!     kipple_core::DeleteOp(op.0)
//! }
//! ```

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::digest::Digest;
use crate::frozen::{FrozenTree, Mark, Marks, NodeId};
use crate::mutation::{FileSystemClass, Freeze};
use crate::native::{NativeKind, NativeSpec};
use crate::outcome::{LeftReason, Skip, Tally};
use crate::plan::{
    Anchor, Execution, ItemId, ItemMethod, Judgement, JudgementField, Plan, PlanEntry, Standing,
};
use crate::policy::{Hold, Proposal, Verdict};
use crate::profile::ProfileGrant;
use crate::rule::Method;
use crate::tracked::{GitGap, OwnGit, holds_repository};

/// What the user authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authorization {
    /// The user selected these items and saw this plan, frozen, in review. Items may be
    /// Safe or Review; each keeps the cautions it was reviewed with. A plan with another
    /// digest isn't authorized.
    Reviewed(ReviewedPlan),
    /// Automation (`--only`, an imported plan, each with `--yes`): Safe items only, never
    /// Review, at planning and again at apply, and deletes only: a trash move takes a
    /// directory whole, which needs a review automation doesn't have.
    SafeOnly,
    /// A profile with `--yes` (D-068): Safe items only, by delete or trash, of the rules
    /// the profile's check found unchanged, by the method and below the roots it froze.
    /// Anything else is not reviewed.
    Profile(ProfileGrant),
}

/// The plan a person saw: its digest ([`Plan::digest`]), which no frontend can name.
///
/// ```compile_fail,E0423
/// // A frontend can't authorize a plan by naming a digest.
/// fn forge(digest: kipple_core::Digest) -> kipple_core::ReviewedPlan {
///     kipple_core::ReviewedPlan(digest)
/// }
/// ```
///
/// ```compile_fail,E0624
/// // Nor by naming a plan it never showed.
/// fn forge(plan: &kipple_core::Plan) -> kipple_core::ReviewedPlan {
///     kipple_core::ReviewedPlan::of(plan)
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewedPlan(Digest);

impl ReviewedPlan {
    /// `plan`, as shown: `clean` makes it from the plan it prepared
    /// ([`Confirmed::Person`](crate::Confirmed)).
    #[must_use]
    pub(crate) const fn of(plan: &Plan) -> Self {
        Self(plan.digest())
    }

    /// `plan`, as if a person saw it. Only for the core's own tests, which drive
    /// [`apply`](crate::apply) directly with no frontend to show it. Only under the
    /// `platform-tests` feature, as [`DeleteOp::for_platform_tests`].
    #[cfg(feature = "platform-tests")]
    #[must_use]
    pub const fn for_core_tests(plan: &Plan) -> Self {
        Self::of(plan)
    }
}

/// What an operation may change: an item's reviewed tree with what revalidation left of
/// it. Reachable only inside a [`DeleteOp`] or a [`TrashOp`].
#[derive(Debug)]
pub struct Authorized {
    item: ItemId,
    anchor: Anchor,
    tree: Arc<FrozenTree>,
    marks: Marks,
    digest: Digest,
}

impl Authorized {
    /// The plan item.
    #[must_use]
    pub const fn item(&self) -> ItemId {
        self.item
    }

    /// Where the target is.
    #[must_use]
    pub const fn anchor(&self) -> &Anchor {
        &self.anchor
    }

    /// The tree the user reviewed.
    #[must_use]
    pub fn tree(&self) -> &FrozenTree {
        &self.tree
    }

    /// What of the tree may still be removed.
    #[must_use]
    pub const fn marks(&self) -> &Marks {
        &self.marks
    }

    /// Whether `id` may be removed.
    #[must_use]
    pub fn may_remove(&self, id: NodeId) -> bool {
        self.marks.is_authorized(id)
    }

    /// The digest the intent records for this set ([`FrozenTree::digest_with`]).
    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }
}

/// An authorized delete. Only [`Executor::delete`](crate::Executor::delete) runs it.
#[derive(Debug)]
pub struct DeleteOp(Authorized);

/// An authorized trash move of a whole item: every entry of its reviewed tree is still
/// authorized, and none was left out. Only a [`TrashBackend`](crate::TrashBackend)
/// runs it.
#[derive(Debug)]
pub struct TrashOp(Authorized);

impl DeleteOp {
    /// What it may remove.
    #[must_use]
    pub const fn target(&self) -> &Authorized {
        &self.0
    }

    /// A delete of `tree` at `anchor`, with what `marks` authorize, made without a plan,
    /// a review or revalidation. Only for the platform's native tests of its executor,
    /// which must hand it operations the apply loop would never make (a tree frozen
    /// before a test changes the disk). The `platform-tests` feature is only ever enabled
    /// by a dev-dependency (`cargo xtask check-arch`).
    #[cfg(feature = "platform-tests")]
    #[must_use]
    pub fn for_platform_tests(anchor: Anchor, tree: FrozenTree, marks: Marks) -> Self {
        Self(Authorized {
            item: ItemId(0),
            anchor,
            digest: tree.digest_with(&marks),
            tree: Arc::new(tree),
            marks,
        })
    }
}

impl TrashOp {
    /// What it moves.
    #[must_use]
    pub const fn target(&self) -> &Authorized {
        &self.0
    }

    /// A trash move of `tree` at `anchor`, with what `marks` authorize, made without a
    /// plan, a review or revalidation, and without checking the marks are whole: the
    /// platform's native tests must show its backend refuses a move `admit` would never
    /// make. Only under the `platform-tests` feature, as
    /// [`DeleteOp::for_platform_tests`].
    #[cfg(feature = "platform-tests")]
    #[must_use]
    pub fn for_platform_tests(anchor: Anchor, tree: FrozenTree, marks: Marks) -> Self {
        Self(Authorized {
            item: ItemId(0),
            anchor,
            digest: tree.digest_with(&marks),
            tree: Arc::new(tree),
            marks,
        })
    }
}

/// An authorized native command. Only a [`NativeRunner`](crate::NativeRunner) runs it, and
/// only revalidation makes one.
#[derive(Debug)]
pub struct NativeOp {
    target: Authorized,
    spec: NativeSpec,
    repository: Option<PathBuf>,
    admin: Option<PathBuf>,
}

impl NativeOp {
    /// What it was reviewed as acting on.
    #[must_use]
    pub const fn target(&self) -> &Authorized {
        &self.target
    }

    /// The command, the tool that runs it and its scope.
    #[must_use]
    pub const fn spec(&self) -> &NativeSpec {
        &self.spec
    }

    /// For a worktree, its repository's main worktree: the checkout is gone after the run,
    /// and the command holds only identities.
    #[must_use]
    pub fn repository(&self) -> Option<&Path> {
        self.repository.as_deref()
    }

    /// For a worktree, its admin directory, which the final check ties to the witness.
    #[must_use]
    pub fn admin(&self) -> Option<&Path> {
        self.admin.as_deref()
    }

    /// A native command over what `target` authorizes, made without a plan, a review or
    /// revalidation. Only for the platform's native tests of its runner. Only under the
    /// `platform-tests` feature, as [`DeleteOp::for_platform_tests`].
    #[cfg(feature = "platform-tests")]
    #[must_use]
    pub fn for_platform_tests(
        target: DeleteOp,
        spec: NativeSpec,
        repository: Option<PathBuf>,
        admin: Option<PathBuf>,
    ) -> Self {
        Self {
            target: target.0,
            spec,
            repository,
            admin,
        }
    }
}

/// ```compile_fail,E0451
/// // A frontend can't make a native command run: only revalidation makes the operation.
/// fn forge(
///     target: kipple_core::Authorized,
///     spec: kipple_core::NativeSpec,
/// ) -> kipple_core::NativeOp {
///     kipple_core::NativeOp { target, spec, repository: None, admin: None }
/// }
/// ```
#[cfg(doctest)]
pub const fn native_op_compile_fail() {}

/// An operation revalidation authorized.
#[derive(Debug)]
pub enum AuthorizedOp {
    /// A delete.
    Delete(DeleteOp),
    /// A trash move.
    Trash(TrashOp),
}

impl AuthorizedOp {
    /// What it acts on.
    #[must_use]
    pub const fn target(&self) -> &Authorized {
        match self {
            Self::Delete(op) => op.target(),
            Self::Trash(op) => op.target(),
        }
    }

    /// Its method.
    #[must_use]
    pub const fn method(&self) -> Method {
        match self {
            Self::Delete(_) => Method::Delete,
            Self::Trash(_) => Method::Trash,
        }
    }
}

/// An item's facts established afresh at apply, through the same ports and
/// [`Policy::verdict`](crate::Policy::verdict) as the scan, under the current policy, with
/// a fresh activity snapshot and a fresh freeze.
#[derive(Debug)]
pub(crate) struct Recheck<'facts> {
    /// The verdict now.
    pub(crate) verdict: &'facts Verdict,
    /// What the rule proposes now.
    pub(crate) proposal: &'facts Proposal,
    /// What the item is judged under now.
    pub(crate) judgement: &'facts Judgement,
    /// Whether its own top-level `.git` stands for a repository inside it now.
    pub(crate) own_git: OwnGit,
    /// The target frozen now.
    pub(crate) freeze: Freeze,
}

/// Authorizes `item` of `plan`, or says why not. Only ever subtracts: an item survives
/// only if every check passes, and its tree only with what the fresh freeze confirms.
///
/// In order: the item must be in the plan and the authorization for this plan, and
/// Safe-only only for a Safe item removed by a delete, a profile only for a Safe item
/// its grant admits; the item must be judged under the
/// same policy, rule, level, recent window, method and kept and excluded paths; it must
/// not be Protected now; its bucket must still be one it was authorized in (a Safe item
/// stays Safe, a Review item gains no caution, and under Safe-only it is Safe); its rule
/// must still offer its method; its file system must be supported; the fresh freeze must
/// confirm at least one reviewed entry; and a trash move or a native command needs every
/// entry of the tree confirmed, nothing left out. No native command is authorized yet: a
/// native item that passes everything is still refused.
pub(crate) fn admit(
    plan: &Plan,
    item: ItemId,
    authorization: &Authorization,
    recheck: Recheck<'_>,
) -> Result<AuthorizedOp, Skip> {
    let entry = plan.entry(item).ok_or(Skip::NotReviewed)?;
    authorized_for(plan, entry, authorization)?;
    if let Some(field) = entry.judgement.first_difference(recheck.judgement) {
        return Err(Skip::Rejudged(field));
    }
    still_allowed(&entry.standing, recheck.verdict, authorization)?;
    still_offered(&entry.execution, recheck.proposal)?;
    let own_git = recheck.own_git;
    let freeze = recheck.freeze;
    if let FileSystemClass::Unsupported(why) = freeze.file_system {
        return Err(Skip::FileSystem(why));
    }
    if let Execution::Native(spec) = &entry.execution {
        native_stands(spec, own_git, &freeze.tree)?;
    }
    let tree = &entry.tree;
    let mut marks = tree.recheck(&freeze.tree);
    // A tree of a root's contents for a target below it, or the reverse, isn't what the
    // anchor names, whatever a freezer returned.
    if entry.anchor.parent != freeze.parent || tree.scope() != entry.anchor.scope() {
        marks.drop_subtree(tree, NodeId::TOP, LeftReason::Replaced);
    }
    if marks.authorized() == 0 {
        return Err(Skip::AllDropped(Tally::new(tree, &marks).before_run().left));
    }
    let whole = marks_whole(&marks, tree);
    let target = Authorized {
        item: entry.item,
        anchor: entry.anchor.clone(),
        digest: tree.digest_with(&marks),
        tree: Arc::clone(tree),
        marks,
    };
    match &entry.execution {
        Execution::Files(Method::Delete) => Ok(AuthorizedOp::Delete(DeleteOp(target))),
        Execution::Files(Method::Trash) | Execution::Native(_) if !whole => {
            let effect = Tally::new(tree, &target.marks).before_run();
            Err(Skip::NotWhole {
                left: effect.left,
                excluded: effect.excluded,
            })
        }
        Execution::Files(Method::Trash) => Ok(AuthorizedOp::Trash(TrashOp(target))),
        Execution::Native(_) => Err(Skip::SemanticNotReviewed),
    }
}

/// Whether what the rule proposes now still offers how `execution` runs.
fn still_offered(execution: &Execution, proposal: &Proposal) -> Result<(), Skip> {
    match (execution, proposal) {
        (
            Execution::Files(method),
            Proposal::Remove(methods) | Proposal::RemoveRelease(methods),
        ) if methods.contains(method) => Ok(()),
        (Execution::Native(spec), Proposal::Native(kind)) if spec.command.kind() == *kind => Ok(()),
        (Execution::Files(method), _) => Err(Skip::MethodNotOffered(*method)),
        (Execution::Native(_), _) => Err(Skip::Rejudged(JudgementField::Method)),
    }
}

/// Whether a native command may act on `tree`: a worktree's removal only under the
/// exception it was planned with, and no command over a repository inside its target.
fn native_stands(spec: &NativeSpec, own_git: OwnGit, tree: &FrozenTree) -> Result<(), Skip> {
    if spec.command.kind() == NativeKind::GitWorktreeRemove && own_git != OwnGit::Replaced {
        return Err(Skip::Protected(vec![Hold::SubjectMismatch]));
    }
    if holds_repository(tree, own_git) {
        return Err(Skip::Git(GitGap::NestedRepository));
    }
    Ok(())
}

/// Whether every entry of `tree` is still to be removed: nothing dropped, excluded, or
/// kept as a root.
fn marks_whole(marks: &Marks, tree: &FrozenTree) -> bool {
    tree.entries()
        .all(|entry| marks.get(entry.id()) == Mark::Remove)
}

fn authorized_for(
    plan: &Plan,
    entry: &PlanEntry,
    authorization: &Authorization,
) -> Result<(), Skip> {
    match authorization {
        Authorization::Reviewed(shown) if *shown != ReviewedPlan::of(plan) => {
            Err(Skip::NotReviewed)
        }
        Authorization::Reviewed(_) => Ok(()),
        // Only a person who saw a tool's command authorizes it (04 §6).
        Authorization::SafeOnly | Authorization::Profile(_)
            if entry.method() == ItemMethod::Native =>
        {
            Err(Skip::SemanticNotReviewed)
        }
        Authorization::SafeOnly => match (&entry.standing, entry.file_method()) {
            (Standing::Review(cautions), _) => {
                Err(Skip::Riskier(cautions.iter().copied().collect()))
            }
            // A trash move takes the directory whole, with whatever is in it then (04
            // §3.5): automation has no review of that; a profile records one.
            (Standing::Safe, Some(Method::Trash)) => Err(Skip::WholeNotReviewed),
            (Standing::Safe, Some(Method::Delete)) => Ok(()),
            (Standing::Safe, None) => Err(Skip::SemanticNotReviewed),
        },
        Authorization::Profile(grant) => match &entry.standing {
            Standing::Review(cautions) => Err(Skip::Riskier(cautions.iter().copied().collect())),
            Standing::Safe if grant.admits(entry) => Ok(()),
            Standing::Safe => Err(Skip::NotReviewed),
        },
    }
}

/// Whether `verdict` now is within the `standing` the item was authorized with.
fn still_allowed(
    standing: &Standing,
    verdict: &Verdict,
    authorization: &Authorization,
) -> Result<(), Skip> {
    let cautions = match verdict {
        Verdict::Protected(holds) => return Err(Skip::Protected(holds.clone())),
        Verdict::Safe => return Ok(()),
        Verdict::Review(cautions) => cautions,
    };
    let reviewed = match (standing, authorization) {
        (Standing::Review(reviewed), Authorization::Reviewed(_)) => reviewed,
        (Standing::Safe, _)
        | (Standing::Review(_), Authorization::SafeOnly | Authorization::Profile(_)) => {
            return Err(Skip::Riskier(cautions.clone()));
        }
    };
    let new: Vec<_> = cautions
        .iter()
        .filter(|caution| !reviewed.contains(caution))
        .copied()
        .collect();
    if new.is_empty() {
        Ok(())
    } else {
        Err(Skip::Riskier(new))
    }
}
