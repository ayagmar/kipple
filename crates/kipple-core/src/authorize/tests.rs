//! Admission at apply: what an item must still be for an operation to be authorized
//! (`docs/04-safety-model.md` §3.3, §3.5, §3.6; spec v2 pinned decisions 1, 2; v2.1 H, J).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use super::{Authorization, AuthorizedOp, Recheck, admit};
use crate::digest::Digest;
use crate::engine::FindingId;
use crate::frozen::{
    Component, EntrySnapshot, Fingerprint, FrozenTree, Mark, RelPath, Scope, Stamp, TreeBuilder,
    Volume,
};
use crate::mutation::{FileSystemClass, Freeze, LocalFileSystem, Unsupported};
use crate::outcome::{LeftReason, Skip};
use crate::plan::{Anchor, Judgement, JudgementField, Plan, PlanEntry, Standing, VerifiedRoot};
use crate::policy::{Caution, Hold, Level, Proposal, Verdict};
use crate::probe::{EntryKind, FileIdentity};
use crate::root::SymbolicRoot;
use crate::rule::{Method, RuleId};

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
    let mut builder =
        TreeBuilder::new(Scope::Whole, VOLUME, &snapshot(EntryKind::Dir, 10, 1)).unwrap();
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
        method: Method::Delete,
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
        item: crate::plan::ItemId(0),
        finding: FindingId(4),
        path: anchor.path(),
        anchor,
        method,
        standing,
        judgement: Judgement {
            method,
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
    freeze: Freeze,
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
            freeze: freeze((*entry.tree).clone()),
        }
    }
}

fn admit_now(
    entry: &PlanEntry,
    authorization: Option<Authorization>,
    now: Now,
) -> Result<AuthorizedOp, Skip> {
    let plan = Plan::new(vec![entry.clone()]);
    let planned = &plan.entries()[0];
    let authorization = authorization.unwrap_or_else(|| Authorization::Reviewed {
        plan: plan.digest(),
    });
    let recheck = Recheck {
        verdict: &now.verdict,
        proposal: &now.proposal,
        judgement: &now.judgement,
        freeze: now.freeze,
    };
    admit(&plan, planned, authorization, recheck)
}

fn review(cautions: &[Caution]) -> Standing {
    Standing::Review(cautions.iter().copied().collect::<BTreeSet<_>>())
}

#[test]
fn an_unchanged_reviewed_item_is_authorized_whole() {
    let entry = entry(Method::Delete, Standing::Safe, tree(1));
    let op = admit_now(&entry, None, Now::same_as(&entry)).unwrap();
    assert_eq!(op.method(), Method::Delete);
    assert_eq!(op.target().marks().authorized(), 5);
}

/// Catches applying a plan other than the one review showed.
#[test]
fn a_review_of_another_plan_authorizes_nothing() {
    let entry = entry(Method::Delete, Standing::Safe, tree(1));
    let other = Authorization::Reviewed { plan: Digest(9) };
    let skip = admit_now(&entry, Some(other), Now::same_as(&entry)).unwrap_err();
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

/// Catches a trash move carrying an entry revalidation dropped (v2.1 J): the move takes
/// the whole directory, so it needs every entry.
#[test]
fn a_trash_move_needs_every_entry_still_authorized() {
    let entry = entry(Method::Trash, Standing::Safe, tree(1));
    let whole = admit_now(&entry, None, Now::same_as(&entry)).unwrap();
    assert_eq!(whole.method(), Method::Trash);

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
