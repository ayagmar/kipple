//! `kipple.stored-plan/v1`: a run's plan, written once, before its first intent.
//!
//! ```text
//! {"schema":"kipple.stored-plan/v1","kipple":"…","plan":"<digest>","items":[…],"digest":"<32 hex>"}\n
//! <item 0's tree><item 1's tree>…
//! ```
//!
//! The header line holds every item's header: what `history` reads, without the trees.
//! Each item's tree follows in the `kipple.frozen-tree` v1 codec, at the offset and length
//! its header gives counted from the end of the header line, with the digest of those
//! stored bytes and the tree's own digest. The plan's digest is stored as written and only
//! ever compared, never computed again (pinned 9).

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::outcome::EstimateOut;
use super::{
    DigestOut, FormatError, IdentityOut, STORED_PLAN, TREE_LIMIT, TextOut, path_out, rel_of,
    rel_out, signed, verified,
};
use crate::digest::Digest;
use crate::engine::FindingId;
use crate::frozen::{FrozenTree, NameCodec};
use crate::mutation::LocalFileSystem;
use crate::plan::{Anchor, ItemId, Judgement, Plan, PlanEntry, Standing, VerifiedRoot};
use crate::policy::{Caution, Level};
use crate::root::SymbolicRoot;
use crate::rule::{Method, RuleId};
use crate::run::{StoredItem, StoredPlan};

/// The header line of a stored plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct HeaderOut {
    /// `kipple.stored-plan/v1`.
    pub schema: String,
    /// The kipple version that wrote it.
    pub kipple: String,
    /// The plan's digest as it was written.
    pub plan: DigestOut,
    /// Every item, in plan order.
    pub items: Vec<ItemOut>,
    /// The digest of every byte of the line before `,"digest":`.
    pub digest: DigestOut,
}

/// The header without its digest: what the digest covers.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Unsigned {
    schema: String,
    kipple: String,
    plan: DigestOut,
    items: Vec<ItemOut>,
}

/// One item's header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ItemOut {
    /// Its place in the plan, from 0.
    pub item: u32,
    /// The finding it was planned from, by its ID in that scan.
    pub finding: u64,
    /// The finding's path, as shown.
    pub path: TextOut,
    /// The root it is below.
    pub root: SymbolicRoot,
    /// That root's directory.
    pub root_path: TextOut,
    /// That root's identity when the scan opened it.
    pub root_identity: IdentityOut,
    /// The components from the root to the target; none for the root's contents.
    pub relative: Vec<TextOut>,
    /// The identity of the target's directory, for a target below its root.
    pub parent: Option<IdentityOut>,
    /// How it is removed.
    pub method: Method,
    /// The bucket and cautions it was planned with.
    pub standing: StandingOut,
    /// What it was judged under.
    pub judgement: JudgementOut,
    /// What the scan estimated removing it frees.
    pub estimate: Option<EstimateOut>,
    /// The file system it is on.
    pub file_system: LocalFileSystem,
    /// Where its frozen tree is.
    pub tree: TreeOut,
}

/// The bucket an item was planned in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "bucket", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum StandingOut {
    /// Safe.
    Safe,
    /// Review, with these cautions.
    Review {
        /// The cautions it was reviewed with.
        cautions: Vec<Caution>,
    },
}

/// What an item was judged under: digests compared at apply, never computed again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct JudgementOut {
    /// The whole policy.
    pub policy: DigestOut,
    /// The rule's ID.
    #[schemars(regex(pattern = r"^[a-z0-9-]+(\.[a-z0-9-]+)+$"))]
    pub rule: String,
    /// The rule's content.
    pub rule_digest: DigestOut,
    /// The effective level.
    pub level: Level,
    /// The effective recent window, in days.
    pub recent_days: u16,
    /// The method.
    pub method: Method,
    /// The kept and excluded paths.
    pub held: DigestOut,
}

/// Where an item's frozen tree is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TreeOut {
    /// Where its bytes start, counted from the end of the header line.
    pub offset: u64,
    /// How many bytes it takes.
    pub length: u64,
    /// The digest of those bytes as stored.
    pub stored: DigestOut,
    /// The tree's own digest ([`FrozenTree::digest`]), which intents name.
    pub digest: DigestOut,
    /// How many entries it holds.
    pub entries: u64,
}

/// Whether `id` is a rule ID as packs write them: `<pack>.<name>`, lowercase.
pub(super) fn rule_id_ok(id: &str) -> bool {
    let part = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    id.len() <= 130 && id.contains('.') && id.split('.').all(part)
}

/// `plan` as stored, with every tree.
///
/// # Errors
/// [`FormatError::TooLarge`] when a tree or the header is larger than the reader takes,
/// so the run could never be read back, reconciled or restored.
pub fn encode_stored_plan(plan: &Plan, codec: &dyn NameCodec) -> Result<Vec<u8>, FormatError> {
    let mut blobs = Vec::new();
    let mut items = Vec::with_capacity(plan.entries().len());
    for entry in plan.entries() {
        let tree = entry.tree().encode(codec);
        if tree.len() > TREE_LIMIT {
            return Err(FormatError::TooLarge(STORED_PLAN));
        }
        items.push(item_out(entry, &tree, blobs.len() as u64, codec));
        blobs.extend_from_slice(&tree);
    }
    let unsigned = Unsigned {
        schema: STORED_PLAN.to_owned(),
        kipple: env!("CARGO_PKG_VERSION").to_owned(),
        plan: DigestOut(plan.digest()),
        items,
    };
    let mut bytes = signed(&unsigned);
    if bytes.len() as u64 > crate::store::HEADER_LIMIT {
        return Err(FormatError::TooLarge(STORED_PLAN));
    }
    bytes.extend_from_slice(&blobs);
    Ok(bytes)
}

fn item_out(entry: &PlanEntry, tree: &[u8], offset: u64, codec: &dyn NameCodec) -> ItemOut {
    let anchor = entry.anchor();
    let judgement = entry.judgement();
    ItemOut {
        item: entry.item().0,
        finding: entry.finding().0,
        path: path_out(entry.path(), codec),
        root: anchor.root.root,
        root_path: path_out(&anchor.root.path, codec),
        root_identity: IdentityOut(anchor.root.identity),
        relative: rel_out(&anchor.relative, codec),
        parent: anchor.parent.map(IdentityOut),
        method: entry.method(),
        standing: match entry.standing() {
            Standing::Safe => StandingOut::Safe,
            Standing::Review(cautions) => StandingOut::Review {
                cautions: cautions.iter().copied().collect(),
            },
        },
        judgement: JudgementOut {
            policy: DigestOut(judgement.policy),
            rule: judgement.rule.0.clone(),
            rule_digest: DigestOut(judgement.rule_digest),
            level: judgement.level,
            recent_days: judgement.recent_days,
            method: judgement.method,
            held: DigestOut(judgement.held),
        },
        estimate: entry.estimate().map(EstimateOut::of),
        file_system: entry.file_system(),
        tree: TreeOut {
            offset,
            length: tree.len() as u64,
            stored: DigestOut(Digest::of_bytes(tree)),
            digest: DigestOut(entry.tree().digest()),
            entries: entry.tree().entry_count() as u64,
        },
    }
}

/// The stored plan `header` describes, with its trees from `trees` when given (the bytes
/// after the header line), or headers only.
///
/// # Errors
/// When the header's digest doesn't match, it isn't a valid `kipple.stored-plan/v1`
/// header, or a tree's bytes don't match their digests or don't decode.
pub fn decode_stored_plan(
    header: &[u8],
    trees: Option<&[u8]>,
    codec: &dyn NameCodec,
) -> Result<StoredPlan, FormatError> {
    let unsigned: Unsigned = verified(header, STORED_PLAN)?;
    if unsigned.schema != STORED_PLAN {
        return Err(FormatError::Schema {
            format: STORED_PLAN,
            found: unsigned.schema,
        });
    }
    let mut items = Vec::with_capacity(unsigned.items.len());
    for (index, item) in unsigned.items.iter().enumerate() {
        if item.item as usize != index {
            return Err(invalid("item numbering"));
        }
        let mut stored = read_item(item, codec)?;
        if let Some(trees) = trees {
            stored.tree = Some(Arc::new(read_tree(&item.tree, trees, codec)?));
        }
        items.push(stored);
    }
    Ok(StoredPlan {
        digest: unsigned.plan.0,
        items,
    })
}

fn read_item(item: &ItemOut, codec: &dyn NameCodec) -> Result<StoredItem, FormatError> {
    let judgement = &item.judgement;
    if !rule_id_ok(&judgement.rule) {
        return Err(invalid("rule ID"));
    }
    if judgement.method != item.method {
        return Err(invalid("method"));
    }
    let relative = rel_of(&item.relative, codec).ok_or_else(|| invalid("relative path"))?;
    if relative.is_empty() == item.parent.is_some() {
        return Err(invalid("parent"));
    }
    let anchor = Anchor {
        root: VerifiedRoot {
            root: item.root,
            path: item
                .root_path
                .path(codec)
                .ok_or_else(|| invalid("root path"))?,
            identity: item.root_identity.0,
        },
        relative,
        parent: item.parent.map(|parent| parent.0),
    };
    Ok(StoredItem {
        item: ItemId(item.item),
        finding: FindingId(item.finding),
        path: item.path.path(codec).ok_or_else(|| invalid("path"))?,
        anchor,
        method: item.method,
        standing: match &item.standing {
            StandingOut::Safe => Standing::Safe,
            StandingOut::Review { cautions } => {
                Standing::Review(cautions.iter().copied().collect())
            }
        },
        judgement: Judgement {
            policy: judgement.policy.0,
            rule: RuleId(judgement.rule.clone()),
            rule_digest: judgement.rule_digest.0,
            level: judgement.level,
            recent_days: judgement.recent_days,
            method: judgement.method,
            held: judgement.held.0,
        },
        estimate: item.estimate.as_ref().map(EstimateOut::read),
        file_system: item.file_system,
        tree_digest: item.tree.digest.0,
        entries: item.tree.entries,
        tree: None,
    })
}

/// The tree `at` places in `trees`, checked against both its digests.
fn read_tree(at: &TreeOut, trees: &[u8], codec: &dyn NameCodec) -> Result<FrozenTree, FormatError> {
    let start = usize::try_from(at.offset).map_err(|_| invalid("tree offset"))?;
    let length = usize::try_from(at.length).map_err(|_| invalid("tree length"))?;
    if length > TREE_LIMIT {
        return Err(FormatError::TooLarge(STORED_PLAN));
    }
    let bytes = start
        .checked_add(length)
        .and_then(|end| trees.get(start..end))
        .ok_or_else(|| invalid("tree place"))?;
    if Digest::of_bytes(bytes) != at.stored.0 {
        return Err(FormatError::Digest(STORED_PLAN));
    }
    let tree = FrozenTree::decode(bytes, codec)?;
    if tree.digest() != at.digest.0 || tree.entry_count() as u64 != at.entries {
        return Err(FormatError::Digest(STORED_PLAN));
    }
    Ok(tree)
}

const fn invalid(what: &'static str) -> FormatError {
    FormatError::invalid(STORED_PLAN, what)
}
