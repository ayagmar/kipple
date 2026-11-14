//! Admission at apply: what an item must still be for an operation to be authorized
//! (`docs/04-safety-model.md` §3.3, §3.5, §3.6; spec v2 pinned decisions 1, 2; v2.1 H, J).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use super::{Authorization, AuthorizedOp, Recheck, ReviewedPlan, admit};
use crate::AgentLabel;
use crate::capability::NativeTool;
use crate::digest::Digest;
use crate::engine::FindingId;
use crate::frozen::{
    Component, EntrySnapshot, Fingerprint, FrozenTree, Mark, RelPath, Scope, Stamp, TreeBuilder,
    Volume,
};
use crate::mutation::{FileSystemClass, Freeze, LocalFileSystem, Unsupported};
use crate::native::{NativeCommand, NativeKind, NativeSpec, ResolvedTool, SemanticFacts, ToolGap};
use crate::outcome::{LeftReason, Skip};
use crate::plan::{
    Anchor, Execution, ItemId, ItemMethod, Judgement, JudgementField, Plan, PlanEntry, Standing,
    VerifiedRoot,
};
use crate::policy::{Caution, Hold, Level, Proposal, Verdict};
use crate::probe::{EntryKind, FileIdentity};
use crate::profile::ProfileGrant;
use crate::root::SymbolicRoot;
use crate::rule::{Method, RuleId};
use crate::supplied::{Subject, Supplied};
use crate::tracked::{GitGap, OwnGit};
use crate::worktree::{
    Checkout, ObjectId, Registration, RegistrationState, WorktreeFacts, WorktreeWitness,
};

const VOLUME: Volume = Volume {
    device: 7,
    mount: Some(1),
};
const PARENT: FileIdentity = FileIdentity { volume: 7, file: 2 };

fn snapshot(kind: EntryKind, file: u128, changed: i64) -> EntrySnapshot {
    let stamp = Stamp::new(changed, 0).unwrap();
    EntrySnapshot {
        kind,
        identity: FileIdentity { volume: 7, file },
        fingerprint: Fingerprint {
            changed: stamp,
            modified: stamp,
            born: Some(stamp),
            size: 10,
        },
        links: 1,
    }
}

/// `target/` holding `a`, `b` and `deps/c`; `changed` moves `b`'s change time.
fn tree(b_changed: i64) -> FrozenTree {
    tree_as(Scope::Whole, b_changed)
}

/// [`tree`], frozen with `scope`.
fn tree_as(scope: Scope, b_changed: i64) -> FrozenTree {
    let mut builder = TreeBuilder::new(scope, VOLUME, &snapshot(EntryKind::Dir, 10, 1)).unwrap();
    let top = builder.top();
    let name = |name: &str| Component::new(name).unwrap();
    builder
        .add(top, name("a"), &snapshot(EntryKind::File, 11, 1))
        .unwrap();
    builder
        .add(top, name("b"), &snapshot(EntryKind::File, 12, b_changed))
        .unwrap();
    let deps = builder
        .add(top, name("deps"), &snapshot(EntryKind::Dir, 13, 1))
        .unwrap();
    builder
        .add(deps, name("c"), &snapshot(EntryKind::File, 14, 1))
        .unwrap();
    builder.finish().unwrap()
}

fn judgement(level: Level) -> Judgement {
    Judgement {
        policy: Digest(1),
        rule: RuleId("builtin.cargo.target".to_owned()),
        rule_digest: Digest(2),
        level,
        recent_days: 7,
        method: ItemMethod::Delete,
        held: Digest(3),
    }
}

fn entry(method: Method, standing: Standing, tree: FrozenTree) -> PlanEntry {
    let anchor = Anchor {
        root: VerifiedRoot {
            root: SymbolicRoot::ProjectRoots,
            path: PathBuf::from("/home/u/code"),
            identity: FileIdentity { volume: 7, file: 1 },
        },
        relative: RelPath::of("app/target".as_ref()).unwrap(),
        parent: Some(PARENT),
    };
    PlanEntry {
        item: ItemId(0),
        finding: FindingId(4),
        path: anchor.path(),
        anchor,
        execution: Execution::Files(method),
        standing,
        judgement: Judgement {
            method: method.into(),
            ..judgement(Level::Balanced)
        },
        estimate: None,
        file_system: LocalFileSystem::Ext4,
        tree: Arc::new(tree),
    }
}

fn freeze(tree: FrozenTree) -> Freeze {
    Freeze {
        tree,
        file_system: FileSystemClass::Supported(LocalFileSystem::Ext4),
        parent: Some(PARENT),
    }
}

/// Everything at apply as it was at planning.
struct Now {
    verdict: Verdict,
    proposal: Proposal,
    judgement: Judgement,
    own_git: OwnGit,
    freeze: Freeze,
    supplied: Supplied,
}

impl Now {
    fn same_as(entry: &PlanEntry) -> Self {
        Self {
            verdict: match &entry.standing {
                Standing::Safe => Verdict::Safe,
                Standing::Review(cautions) => Verdict::Review(cautions.iter().copied().collect()),
            },
            proposal: Proposal::Remove(vec![Method::Delete, Method::Trash]),
            judgement: entry.judgement.clone(),
            own_git: OwnGit::Counts,
            freeze: freeze((*entry.tree).clone()),
            supplied: Supplied::default(),
        }
    }
}

fn admit_now(
    entry: &PlanEntry,
    authorization: Option<Authorization>,
    now: Now,
) -> Result<AuthorizedOp, Skip> {
    admit_item(entry, ItemId(0), authorization, now)
}

fn admit_item(
    entry: &PlanEntry,
    item: ItemId,
    authorization: Option<Authorization>,
    now: Now,
) -> Result<AuthorizedOp, Skip> {
    let plan = Plan::new(vec![entry.clone()]);
    let authorization =
        authorization.unwrap_or_else(|| Authorization::Reviewed(ReviewedPlan::of(&plan)));
    let recheck = Recheck {
        verdict: &now.verdict,
        proposal: &now.proposal,
        judgement: &now.judgement,
        own_git: now.own_git,
        freeze: now.freeze,
        supplied: &now.supplied,
    };
    admit(&plan, item, &authorization, recheck)
}

fn review(cautions: &[Caution]) -> Standing {
    Standing::Review(cautions.iter().copied().collect::<BTreeSet<_>>())
}

#[test]
fn an_unchanged_reviewed_item_is_authorized_whole() {
    let entry = entry(Method::Delete, Standing::Safe, tree(1));
    let op = admit_now(&entry, None, Now::same_as(&entry)).unwrap();
    assert_eq!(op.method(), ItemMethod::Delete);
    assert_eq!(op.target().marks().authorized(), 5);
}

/// Catches applying a plan other than the one review showed.
#[test]
fn a_review_of_another_plan_authorizes_nothing() {
    let entry = entry(Method::Delete, Standing::Safe, tree(1));
    let other = Authorization::Reviewed(ReviewedPlan(Digest(9)));
    let skip = admit_now(&entry, Some(other), Now::same_as(&entry)).unwrap_err();
    assert_eq!(skip, Skip::NotReviewed);
}

/// Catches authorizing an item the reviewed plan doesn't hold.
#[test]
fn an_item_outside_the_plan_authorizes_nothing() {
    let entry = entry(Method::Delete, Standing::Safe, tree(1));
    let skip = admit_item(&entry, ItemId(1), None, Now::same_as(&entry)).unwrap_err();
    assert_eq!(skip, Skip::NotReviewed);
}

/// Catches automation moving a directory whole, with whatever entered it after the check,
/// when no one reviewed a whole-directory move (04 §3.5).
#[test]
fn safe_only_automation_never_moves_a_directory_whole() {
    let trash = entry(Method::Trash, Standing::Safe, tree(1));
    let skip = admit_now(&trash, Some(Authorization::SafeOnly), Now::same_as(&trash)).unwrap_err();
    assert_eq!(skip, Skip::WholeNotReviewed);
    let delete = entry(Method::Delete, Standing::Safe, tree(1));
    assert!(
        admit_now(
            &delete,
            Some(Authorization::SafeOnly),
            Now::same_as(&delete)
        )
        .is_ok(),
        "a Safe delete still runs under Safe-only"
    );
}

/// Catches automation acting on Review items (03 J6: `--yes` takes Safe only).
#[test]
fn safe_only_automation_never_takes_a_review_item() {
    let entry = entry(Method::Delete, review(&[Caution::Recent]), tree(1));
    let skip = admit_now(&entry, Some(Authorization::SafeOnly), Now::same_as(&entry)).unwrap_err();
    assert_eq!(skip, Skip::Riskier(vec![Caution::Recent]));
}

/// Catches revalidation that lets an item through in a riskier bucket than the one it
/// was authorized in.
#[test]
fn an_item_authorized_safe_that_is_review_now_is_dropped() {
    let entry = entry(Method::Delete, Standing::Safe, tree(1));
    let mut now = Now::same_as(&entry);
    now.verdict = Verdict::Review(vec![Caution::Recent]);
    let skip = admit_now(&entry, None, now).unwrap_err();
    assert_eq!(skip, Skip::Riskier(vec![Caution::Recent]));
}

/// Catches a Review item that gained a caution the user never saw.
#[test]
fn a_review_item_keeps_only_the_cautions_it_was_reviewed_with() {
    let entry = entry(Method::Delete, review(&[Caution::Recent]), tree(1));
    let mut same = Now::same_as(&entry);
    same.verdict = Verdict::Review(vec![Caution::Recent]);
    assert!(
        admit_now(&entry, None, same).is_ok(),
        "the same cautions pass"
    );

    let mut safer = Now::same_as(&entry);
    safer.verdict = Verdict::Safe;
    assert!(
        admit_now(&entry, None, safer).is_ok(),
        "fewer cautions pass"
    );

    let mut riskier = Now::same_as(&entry);
    riskier.verdict = Verdict::Review(vec![Caution::Recent, Caution::HighCost]);
    let skip = admit_now(&entry, None, riskier).unwrap_err();
    assert_eq!(skip, Skip::Riskier(vec![Caution::HighCost]));
}

/// Catches an item that became busy or unknown since review still running (04 §3.3).
#[test]
fn an_item_protected_now_is_dropped_with_every_reason() {
    let entry = entry(Method::Delete, Standing::Safe, tree(1));
    let mut now = Now::same_as(&entry);
    now.verdict = Verdict::Protected(vec![Hold::InUse, Hold::UnknownGit]);
    let skip = admit_now(&entry, None, now).unwrap_err();
    assert_eq!(skip, Skip::Protected(vec![Hold::InUse, Hold::UnknownGit]));
}

/// Catches a changed level, recent window or policy that doesn't drop the item (v2.1 H,
/// 11 M3's profile regressions).
#[test]
fn an_item_judged_under_anything_else_now_is_dropped() {
    let entry = entry(Method::Delete, Standing::Safe, tree(1));
    let mut level = Now::same_as(&entry);
    level.judgement.level = Level::Thorough;
    let skip = admit_now(&entry, None, level).unwrap_err();
    assert_eq!(skip, Skip::Rejudged(JudgementField::Level));

    let mut held = Now::same_as(&entry);
    held.judgement.held = Digest(30);
    held.judgement.policy = Digest(10);
    let held = admit_now(&entry, None, held).unwrap_err();
    assert_eq!(held, Skip::Rejudged(JudgementField::Held));
}

/// Catches a method the rule no longer offers running anyway.
#[test]
fn a_method_the_rule_no_longer_offers_is_dropped() {
    let entry = entry(Method::Trash, Standing::Safe, tree(1));
    let mut now = Now::same_as(&entry);
    now.proposal = Proposal::Remove(vec![Method::Delete]);
    let skip = admit_now(&entry, None, now).unwrap_err();
    assert_eq!(skip, Skip::MethodNotOffered(Method::Trash));
}

/// Catches mutating a network or unknown file system (04 §8).
#[test]
fn an_item_on_an_unsupported_file_system_now_is_dropped() {
    let entry = entry(Method::Delete, Standing::Safe, tree(1));
    let mut now = Now::same_as(&entry);
    now.freeze.file_system = FileSystemClass::Unsupported(Unsupported::Network);
    let skip = admit_now(&entry, None, now).unwrap_err();
    assert_eq!(skip, Skip::FileSystem(Unsupported::Network));
}

/// Catches a fresh freeze widening what the plan authorized, or an entry changed since
/// review still being removed.
#[test]
fn a_delete_keeps_only_what_the_fresh_freeze_confirms() {
    let entry = entry(Method::Delete, Standing::Safe, tree(1));
    let mut now = Now::same_as(&entry);
    now.freeze = freeze(tree(5));
    let AuthorizedOp::Delete(op) = admit_now(&entry, None, now).unwrap() else {
        panic!("a delete stays a delete");
    };
    let target = op.target();
    let b = target
        .tree()
        .find(&RelPath::of("b".as_ref()).unwrap())
        .unwrap();
    assert_eq!(target.marks().get(b), Mark::Dropped(LeftReason::Changed));
    assert_eq!(target.marks().authorized(), 4);
    assert_ne!(
        target.digest(),
        target.tree().digest_with(&crate::Marks::of(target.tree()))
    );
}

/// Catches a target whose directory was swapped since the plan.
#[test]
fn a_replaced_parent_drops_everything() {
    let entry = entry(Method::Delete, Standing::Safe, tree(1));
    let mut now = Now::same_as(&entry);
    now.freeze.parent = Some(FileIdentity {
        volume: 7,
        file: 99,
    });
    let Err(Skip::AllDropped(left)) = admit_now(&entry, None, now) else {
        panic!("nothing is authorized");
    };
    assert_eq!(left.0[&LeftReason::Replaced].count, 5);
}

/// Catches an item run with a tree that doesn't have its anchor's scope: a root's
/// contents frozen for a target below the root names other entries than the anchor, so
/// nothing is authorized even when a fresh freeze agrees with the stored tree.
#[test]
fn a_tree_with_another_scope_than_its_anchor_authorizes_nothing() {
    let entry = entry(Method::Delete, Standing::Safe, tree_as(Scope::Contents, 1));

    let Err(Skip::AllDropped(left)) = admit_now(&entry, None, Now::same_as(&entry)) else {
        panic!("nothing is authorized");
    };

    assert_eq!(
        left.0[&LeftReason::Replaced].count,
        4,
        "everything below the top"
    );
}

/// Catches a trash move carrying an entry revalidation dropped (v2.1 J): the move takes
/// the whole directory, so it needs every entry.
#[test]
fn a_trash_move_needs_every_entry_still_authorized() {
    let entry = entry(Method::Trash, Standing::Safe, tree(1));
    let whole = admit_now(&entry, None, Now::same_as(&entry)).unwrap();
    assert_eq!(whole.method(), ItemMethod::Trash);

    let mut changed = Now::same_as(&entry);
    changed.freeze = freeze(tree(5));
    let Err(Skip::NotWhole { left, excluded: 0 }) = admit_now(&entry, None, changed) else {
        panic!("a trash move with a dropped entry is refused, never deleted");
    };
    assert_eq!(left.0[&LeftReason::Changed].count, 1);
}

/// Catches a trash move carrying a file the plan left out, such as one git tracks
/// (v2.1 J).
#[test]
fn a_trash_move_refuses_an_item_with_an_excluded_entry() {
    let mut frozen = tree(1);
    let c = frozen
        .find(&RelPath::of("deps/c".as_ref()).unwrap())
        .unwrap();
    frozen.exclude(c);
    let entry = entry(Method::Trash, Standing::Safe, frozen);
    let skip = admit_now(&entry, None, Now::same_as(&entry)).unwrap_err();
    assert!(
        matches!(skip, Skip::NotWhole { excluded: 1, .. }),
        "the excluded entry refuses the move: {skip:?}"
    );
}

/// Catches trashing a root, which would move the root directory itself.
#[test]
fn a_root_target_is_never_moved_to_the_trash() {
    let mut builder =
        TreeBuilder::new(Scope::Contents, VOLUME, &snapshot(EntryKind::Dir, 10, 1)).unwrap();
    let top = builder.top();
    builder
        .add(
            top,
            Component::new("a").unwrap(),
            &snapshot(EntryKind::File, 11, 1),
        )
        .unwrap();
    let mut entry = entry(Method::Trash, Standing::Safe, builder.finish().unwrap());
    entry.anchor.relative = RelPath::default();
    let mut now = Now::same_as(&entry);
    now.freeze.parent = Some(PARENT);
    entry.anchor.parent = Some(PARENT);
    let skip = admit_now(&entry, None, now).unwrap_err();
    assert!(
        matches!(skip, Skip::NotWhole { .. }),
        "a root's directory isn't authorized, so it can't move: {skip:?}"
    );
}

fn resolved() -> ResolvedTool {
    ResolvedTool {
        tool: NativeTool::Git,
        path: PathBuf::from("/usr/bin/git"),
        identity: FileIdentity { volume: 7, file: 9 },
        fingerprint: snapshot(EntryKind::File, 9, 1).fingerprint,
        version: "2.47.0".to_owned(),
        runtime: None,
    }
}

fn witness() -> WorktreeWitness {
    let identity = FileIdentity { volume: 7, file: 5 };
    WorktreeWitness {
        common: identity,
        admin: identity,
        gitfile: identity,
        gitdir_file: identity,
        head: ObjectId("0".repeat(40)),
        branch: None,
    }
}

/// What a worktree's integration reads again at apply, as it was at planning.
fn worktree_read(path: PathBuf) -> Subject {
    Subject::Worktree(Box::new(WorktreeFacts {
        registration: Registration {
            path,
            head: Some(ObjectId("0".repeat(40))),
            checkout: Checkout::Detached,
            locked: None,
            prunable: false,
            bare: false,
        },
        state: RegistrationState::RegisteredPresent,
        repository: PathBuf::from("/home/u/code/app"),
        admin: Some(PathBuf::from("/home/u/code/app/.git/worktrees/target")),
        witness: Ok(witness()),
        ignored: Ok(Vec::new()),
        tool: Ok(resolved()),
        agent: AgentLabel::Unknown,
    }))
}

/// A native item on `tree`, reviewed, with everything at apply as it was at planning.
fn native(command: NativeCommand, tree: FrozenTree) -> (PlanEntry, Now) {
    let mut native = entry(Method::Delete, Standing::Safe, tree);
    let subject = match &command {
        NativeCommand::NpmCacheClean { .. } => Subject::Semantic(SemanticFacts {
            kind: NativeKind::NpmCacheClean,
            tool: Ok(resolved()),
        }),
        NativeCommand::GitWorktreeRemove { .. } => worktree_read(native.path.clone()),
    };
    native.execution = Execution::Native(Box::new(NativeSpec {
        command,
        tool: resolved(),
        scope: native.anchor.clone(),
    }));
    native.judgement.method = ItemMethod::Native;
    let mut now = Now::same_as(&native);
    now.proposal = Proposal::Native(match &native.execution {
        Execution::Native(spec) => spec.command.kind(),
        Execution::Files(_) => NativeKind::NpmCacheClean,
    });
    now.supplied.subject = subject;
    (native, now)
}

fn npm(tree: FrozenTree) -> (PlanEntry, Now) {
    let cache = entry(Method::Delete, Standing::Safe, tree.clone()).anchor;
    native(NativeCommand::NpmCacheClean { cache }, tree)
}

fn worktree(tree: FrozenTree) -> (PlanEntry, Now) {
    let checkout = entry(Method::Delete, Standing::Safe, tree.clone()).anchor;
    let witness = witness();
    let (entry, mut now) = native(NativeCommand::GitWorktreeRemove { checkout, witness }, tree);
    now.own_git = OwnGit::Replaced;
    (entry, now)
}

/// A worktree's tree with a `.git` of `kind` at `path` below its top.
fn with_git(path: &[&str], kind: EntryKind) -> FrozenTree {
    let mut builder =
        TreeBuilder::new(Scope::Whole, VOLUME, &snapshot(EntryKind::Dir, 20, 1)).unwrap();
    let mut at = builder.top();
    for (index, name) in path.iter().enumerate() {
        let file = 21 + index as u128;
        let last = index + 1 == path.len();
        let entry = snapshot(if last { kind } else { EntryKind::Dir }, file, 1);
        at = builder
            .add(at, Component::new(*name).unwrap(), &entry)
            .unwrap();
    }
    builder.finish().unwrap()
}

/// Why `item`, as it is at apply, isn't authorized, for a person who reviewed it.
fn refused((item, now): (PlanEntry, Now)) -> Skip {
    admit_now(&item, None, now).unwrap_err()
}

/// `item`, with `change` made to what is there at apply.
fn changed((item, mut now): (PlanEntry, Now), change: impl FnOnce(&mut Now)) -> (PlanEntry, Now) {
    change(&mut now);
    (item, now)
}

/// Catches a tool's command run over a tree with a mark on it: a command can't subtract,
/// so what revalidation dropped or the plan left out would go anyway (pinned 4). npm's
/// clean alone compares only its target's top (pinned 29), so a cache entry rewritten
/// since review doesn't refuse it, and the plan's own exclusion still does.
#[test]
fn dropped_mark_on_a_native_tree_refuses_the_item() {
    let worktree_tree = || with_git(&[".git"], EntryKind::File);
    let rewritten = || {
        let mut builder =
            TreeBuilder::new(Scope::Whole, VOLUME, &snapshot(EntryKind::Dir, 20, 1)).unwrap();
        let top = builder.top();
        builder
            .add(
                top,
                Component::new(".git").unwrap(),
                &snapshot(EntryKind::File, 21, 9),
            )
            .unwrap();
        builder.finish().unwrap()
    };
    let dropped = refused(changed(worktree(worktree_tree()), |now| {
        now.freeze = freeze(rewritten());
    }));
    assert!(
        matches!(dropped, Skip::NotWhole { .. }),
        "a dropped entry refuses the whole item: {dropped:?}"
    );

    let (item, now) = changed(npm(tree(1)), |now| now.freeze = freeze(tree(5)));
    let cleaned = admit_now(&item, None, now);
    assert!(
        matches!(&cleaned, Ok(AuthorizedOp::Native(_))),
        "npm's entries are its own to change: {cleaned:?}"
    );

    let mut frozen = tree(1);
    let c = frozen
        .find(&RelPath::of("deps/c".as_ref()).unwrap())
        .unwrap();
    frozen.exclude(c);
    let excluded = refused(npm(frozen));
    assert!(
        matches!(excluded, Skip::NotWhole { excluded: 1, .. }),
        "an exclusion refuses the whole item: {excluded:?}"
    );
}

/// Catches automation reaching a tool's command that passes every other check (pinned 24),
/// and a person's review of one never authorizing it.
#[test]
fn only_a_person_authorizes_a_native_item() {
    let (item, now) = npm(tree(1));
    let automated = admit_now(&item, Some(Authorization::SafeOnly), now).unwrap_err();
    assert_eq!(automated, Skip::SemanticNotReviewed);

    let (granted, at_apply) = npm(tree(1));
    let grant = Authorization::Profile(ProfileGrant::for_core_tests());
    assert_eq!(
        admit_now(&granted, Some(grant), at_apply).unwrap_err(),
        Skip::SemanticNotReviewed,
        "a profile recorded no review of a command, whatever it grants"
    );

    let (reviewed, at_review) = npm(tree(1));
    let op = admit_now(&reviewed, None, at_review);
    assert!(
        matches!(&op, Ok(AuthorizedOp::Native(op)) if op.spec().command.kind() == NativeKind::NpmCacheClean),
        "a person who saw it authorizes it: {op:?}"
    );
}

/// Catches a command run by a tool other than the one a person saw: one read again as
/// another file or version, or not found now, skips the item before its intent (pinned
/// 29). A revisit that kept the scan's tool is caught again by the runner's own check.
#[test]
fn executable_swapped_before_apply_skips() {
    let mut other = npm(tree(1));
    if let Subject::Semantic(facts) = &mut other.1.supplied.subject
        && let Ok(tool) = &mut facts.tool
    {
        tool.identity.file += 1;
    }
    assert_eq!(refused(other), Skip::ToolChanged(ToolGap::Changed));

    // Rewritten in place, or another file in its inode: the identity alone can't tell.
    let mut rewritten = npm(tree(1));
    if let Subject::Semantic(facts) = &mut rewritten.1.supplied.subject
        && let Ok(tool) = &mut facts.tool
    {
        tool.fingerprint.changed = Stamp::new(2, 0).unwrap();
    }
    assert_eq!(refused(rewritten), Skip::ToolChanged(ToolGap::Changed));

    let mut upgraded = worktree(with_git(&[".git"], EntryKind::File));
    if let Subject::Worktree(facts) = &mut upgraded.1.supplied.subject
        && let Ok(tool) = &mut facts.tool
    {
        tool.version = "2.47.1".to_owned();
    }
    assert_eq!(refused(upgraded), Skip::ToolChanged(ToolGap::Changed));

    let mut gone = npm(tree(1));
    gone.1.supplied.subject = Subject::Semantic(SemanticFacts {
        kind: NativeKind::NpmCacheClean,
        tool: Err(ToolGap::Missing),
    });
    assert_eq!(refused(gone), Skip::ToolChanged(ToolGap::Missing));
}

/// Catches a worktree removed when its registration, repository or HEAD changed since
/// review, which booleans and counts alone can't see (pinned 20), or when the witness
/// can't be built again.
#[test]
fn a_witness_built_again_must_equal_the_reviewed_one() {
    let with_witness = |witness: Result<WorktreeWitness, GitGap>| {
        let mut item = worktree(with_git(&[".git"], EntryKind::File));
        if let Subject::Worktree(facts) = &mut item.1.supplied.subject {
            facts.witness = witness;
        }
        refused(item)
    };
    let mut moved = witness();
    moved.head = ObjectId("1".repeat(40));
    assert_eq!(with_witness(Ok(moved)), Skip::WitnessChanged);
    let mut other = witness();
    other.admin.file += 1;
    assert_eq!(with_witness(Ok(other)), Skip::WitnessChanged);
    assert_eq!(with_witness(Err(GitGap::Failed)), Skip::Git(GitGap::Failed));

    let mut plain = worktree(with_git(&[".git"], EntryKind::File));
    plain.1.supplied.subject = Subject::Plain;
    assert_eq!(refused(plain), Skip::Protected(vec![Hold::SubjectMismatch]));
}

/// Catches a file added inside a worktree since review going with it: `status` prints an
/// ignored directory and stops there, so the tree's own comparison is all that sees it
/// (pinned 29). npm's cache gaining entries is its stated effect, and doesn't refuse.
#[test]
fn an_entry_added_since_review_refuses_a_worktree_and_not_npm() {
    let grown = || {
        let mut builder =
            TreeBuilder::new(Scope::Whole, VOLUME, &snapshot(EntryKind::Dir, 20, 1)).unwrap();
        let top = builder.top();
        builder
            .add(
                top,
                Component::new(".git").unwrap(),
                &snapshot(EntryKind::File, 21, 1),
            )
            .unwrap();
        builder
            .add(
                top,
                Component::new("new").unwrap(),
                &snapshot(EntryKind::File, 30, 1),
            )
            .unwrap();
        builder.finish().unwrap()
    };
    let added = refused(changed(
        worktree(with_git(&[".git"], EntryKind::File)),
        |now| {
            now.freeze = freeze(grown());
        },
    ));
    assert!(
        matches!(added, Skip::AllDropped(_)),
        "an entry the review never saw refuses the worktree it landed in: {added:?}"
    );

    let (item, now) = changed(npm(with_git(&["index-v5"], EntryKind::Dir)), |now| {
        now.freeze = freeze(with_git(&["index-v5", "bucket"], EntryKind::File));
    });
    assert!(
        matches!(admit_now(&item, None, now), Ok(AuthorizedOp::Native(_))),
        "a cache entry that landed since review is npm's to remove"
    );
}

/// Catches a command reviewed as one operation admitted when the rule now proposes
/// another, or files.
#[test]
fn a_native_item_needs_the_same_operation_proposed_now() {
    let method = Skip::Rejudged(JudgementField::Method);
    let other = changed(npm(tree(1)), |now| {
        now.proposal = Proposal::Native(NativeKind::GitWorktreeRemove);
    });
    assert_eq!(refused(other), method);

    let files = changed(npm(tree(1)), |now| {
        now.proposal = Proposal::Remove(vec![Method::Delete]);
    });
    assert_eq!(refused(files), method);
}

/// Catches the worktree exception widened past the worktree's own top-level `.git`: a
/// repository anywhere deeper, a directory or a case variant at the top, holds it, and an
/// item that lost the exception's key holds whatever it contains (pinned 4).
#[test]
fn only_a_worktrees_own_git_file_is_excused_at_apply() {
    let holding = [
        with_git(&["sub", ".git"], EntryKind::File),
        with_git(&["sub", ".git"], EntryKind::Dir),
        with_git(&[".git"], EntryKind::Dir),
        with_git(&[".GIT"], EntryKind::File),
    ];
    for tree in holding {
        assert_eq!(refused(worktree(tree)), Skip::Git(GitGap::NestedRepository));
    }

    let own = || worktree(with_git(&[".git"], EntryKind::File));
    let (item, now) = own();
    assert!(
        matches!(admit_now(&item, None, now), Ok(AuthorizedOp::Native(_))),
        "past every hold"
    );
    assert_eq!(
        refused(changed(own(), |counts| counts.own_git = OwnGit::Counts)),
        Skip::Protected(vec![Hold::SubjectMismatch])
    );
}
