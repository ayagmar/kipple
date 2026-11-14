//! `[rule.retention]`: how a release's group is formed. What each group keeps is fixed, so
//! a rule can only say so (`docs/06-adapters-and-rules.md` §4).

use std::ops::Range;

use kipple_core::{GroupKey, OperationSpec, Retention};

use super::ErrorCode;
use super::doc::{Checker, Table, Value};

/// What a group always keeps. A rule states each as `true`.
const KEEPS: [&str; 4] = [
    "keep_active",
    "keep_previous",
    "keep_running",
    "keep_pending_install",
];

/// What a directory whose version can't be read gets.
const UNPARSABLE_VERSION: &str = "unparsable_version";

/// The `[rule.retention]` of a rule whose operation declares retention.
pub(super) fn of_rule(
    c: &mut Checker<'_>,
    rule: &Table<'_>,
    at: Range<usize>,
) -> Option<Retention> {
    let value = c.required(rule, at, "retention")?;
    retention(c, value)
}

/// Reports a `[rule.retention]` on the operation `spec`, which declares none.
pub(super) fn refuse(c: &mut Checker<'_>, rule: &Table<'_>, spec: &OperationSpec) {
    if let Some(value) = rule.get("retention") {
        let message = format!("unknown key `retention`: `{}` keeps nothing", spec.name);
        c.error(ErrorCode::UnknownKey, value.span(), message);
    }
}

fn retention(c: &mut Checker<'_>, value: &Value<'_>) -> Option<Retention> {
    let table = c.table(value)?;
    let mut keys = vec!["group_by", UNPARSABLE_VERSION];
    keys.extend(KEEPS);
    c.known_keys(table, &keys);
    let group_by = c
        .required(table, value.span(), "group_by")
        .and_then(|v| group_by(c, v));
    let mut fixed = true;
    for key in KEEPS {
        fixed &= required_value(c, table, value.span(), key, |v| {
            v.get_ref().as_bool() == Some(true)
        });
    }
    fixed &= required_value(c, table, value.span(), UNPARSABLE_VERSION, |v| {
        v.get_ref().as_str() == Some("keep")
    });
    fixed.then_some(Retention {
        group_by: group_by?,
    })
}

/// Whether `key` is there and `accepted`, reporting it when it isn't.
fn required_value(
    c: &mut Checker<'_>,
    table: &Table<'_>,
    at: Range<usize>,
    key: &str,
    accepted: impl Fn(&Value<'_>) -> bool,
) -> bool {
    let Some(value) = c.required(table, at, key) else {
        return false;
    };
    let fine = accepted(value);
    if !fine {
        let message = format!("`{key}` is fixed: the host always keeps these");
        c.error(ErrorCode::InvalidRange, value.span(), message);
    }
    fine
}

/// `group_by`, which is fixed too: a release is compared only with releases of its own
/// component and target, as upstream's prune compares them, so either key alone would
/// settle a release against another platform's.
fn group_by(c: &mut Checker<'_>, value: &Value<'_>) -> Option<Vec<GroupKey>> {
    const FIXED: [GroupKey; 2] = [GroupKey::Component, GroupKey::Target];
    let items = c.array(value)?;
    let mut keys = Vec::new();
    for item in items {
        keys.push(c.one_of(
            item,
            GroupKey::from_name,
            "a group key: component or target",
        )?);
    }
    let fixed = keys.len() == FIXED.len() && FIXED.iter().all(|key| keys.contains(key));
    if !fixed {
        let message = "`group_by` is fixed: name `component` and `target`, once each";
        c.error(ErrorCode::InvalidRange, value.span(), message);
        return None;
    }
    Some(FIXED.to_vec())
}
