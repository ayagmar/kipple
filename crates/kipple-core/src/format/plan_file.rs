//! `kipple.plan/v1`: a plan `plan -o` writes and `apply` plans again.
//!
//! It names each item by its root, its path below the root and its rule, with its method
//! and frozen tree (`kipple.frozen-tree` v1 in base64). It authorizes nothing: decoding it
//! gives an [`ImportedPlan`], whose only use is [`replan`](crate::replan).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::stored_plan::rule_id_ok;
use super::{
    FormatError, IdentityOut, PLAN, TREE_LIMIT, TextOut, base64, path_out, rel_of, rel_out, shape,
    unbase64,
};
use crate::frozen::{FrozenTree, NameCodec};
use crate::imported::{ImportedItem, ImportedPlan};
use crate::plan::{Plan, VerifiedRoot};
use crate::root::SymbolicRoot;
use crate::rule::{Method, RuleId};

/// The most bytes a plan file may take.
pub(crate) const PLAN_LIMIT: usize = 1024 * 1024 * 1024;

/// A plan file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanFileOut {
    /// `kipple.plan/v1`.
    pub schema: String,
    /// The kipple version that wrote it.
    pub kipple: String,
    /// Every item, in plan order.
    pub items: Vec<PlanItemOut>,
}

/// One item of a plan file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanItemOut {
    /// The root it is below.
    pub root: SymbolicRoot,
    /// That root's directory.
    pub root_path: TextOut,
    /// That root's identity when the plan was made.
    pub root_identity: IdentityOut,
    /// The components from the root to the target; none for the root's contents.
    pub relative: Vec<TextOut>,
    /// The rule that found it.
    #[schemars(regex(pattern = r"^[a-z0-9-]+(\.[a-z0-9-]+)+$"))]
    pub rule: String,
    /// How it was to be removed.
    pub method: Method,
    /// Its frozen tree, `kipple.frozen-tree` v1 in base64.
    pub tree: String,
}

/// `plan` as a plan file.
///
/// # Errors
/// [`FormatError::TooLarge`] when an item's tree or the whole file is larger than the
/// reader takes.
pub fn encode_plan_file(plan: &Plan, codec: &dyn NameCodec) -> Result<Vec<u8>, FormatError> {
    let items = plan
        .entries()
        .iter()
        .map(|entry| {
            let anchor = entry.anchor();
            let tree = entry.tree().encode(codec);
            if tree.len() > TREE_LIMIT {
                return Err(FormatError::TooLarge(PLAN));
            }
            Ok(PlanItemOut {
                root: anchor.root.root,
                root_path: path_out(&anchor.root.path, codec),
                root_identity: IdentityOut(anchor.root.identity),
                relative: rel_out(&anchor.relative, codec),
                rule: entry.judgement().rule.0.clone(),
                method: entry.method(),
                tree: base64(&tree),
            })
        })
        .collect::<Result<_, _>>()?;
    let file = PlanFileOut {
        schema: PLAN.to_owned(),
        kipple: env!("CARGO_PKG_VERSION").to_owned(),
        items,
    };
    // Serializing records of strings, numbers and enums can't fail.
    let mut bytes = serde_json::to_vec_pretty(&file).unwrap_or_default();
    bytes.push(b'\n');
    if bytes.len() > PLAN_LIMIT {
        return Err(FormatError::TooLarge(PLAN));
    }
    Ok(bytes)
}

/// The plan a plan file holds, untrusted.
///
/// # Errors
/// When it is too large, isn't a valid `kipple.plan/v1` document, or a tree in it doesn't
/// decode.
pub fn decode_plan_file(bytes: &[u8], codec: &dyn NameCodec) -> Result<ImportedPlan, FormatError> {
    if bytes.len() > PLAN_LIMIT {
        return Err(FormatError::TooLarge(PLAN));
    }
    let file: PlanFileOut = serde_json::from_slice(bytes).map_err(|error| shape(PLAN, &error))?;
    if file.schema != PLAN {
        return Err(FormatError::Schema {
            format: PLAN,
            found: file.schema,
        });
    }
    let items = file
        .items
        .iter()
        .map(|item| read_item(item, codec))
        .collect::<Result<_, _>>()?;
    Ok(ImportedPlan { items })
}

fn read_item(item: &PlanItemOut, codec: &dyn NameCodec) -> Result<ImportedItem, FormatError> {
    let invalid = |what| FormatError::invalid(PLAN, what);
    if !rule_id_ok(&item.rule) {
        return Err(invalid("rule ID"));
    }
    if item.tree.len() > TREE_LIMIT / 3 * 4 + 4 {
        return Err(FormatError::TooLarge(PLAN));
    }
    let bytes = unbase64(&item.tree).ok_or_else(|| invalid("tree"))?;
    Ok(ImportedItem {
        root: VerifiedRoot {
            root: item.root,
            path: item
                .root_path
                .path(codec)
                .ok_or_else(|| invalid("root path"))?,
            identity: item.root_identity.0,
        },
        relative: rel_of(&item.relative, codec).ok_or_else(|| invalid("relative path"))?,
        rule: RuleId(item.rule.clone()),
        method: item.method,
        tree: FrozenTree::decode(&bytes, codec)?,
    })
}
