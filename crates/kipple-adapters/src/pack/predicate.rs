//! `requirements` and `eligibility`: typed predicates over published facts.

use std::ops::Range;
use std::time::Duration;

use kipple_core::{
    Bounds, FactKind, FactSpec, GENERIC_FACTS, IntegrationDescriptor, IntegrationId, Predicate,
    Test,
};

use super::doc::{Checker, Table, Value};
use super::{Catalog, ErrorCode, Origin, grammar};

/// The most predicates one rule may hold, over both tables.
const MAX_PREDICATES: usize = 32;
/// The range of a count bound.
const COUNT: Range<i64> = 0..1_000_001;

/// A rule's two tables of predicates.
pub(super) struct RulePredicates {
    pub(super) requirements: Vec<Predicate>,
    pub(super) eligibility: Vec<Predicate>,
}

/// What decides which facts a rule may test.
#[derive(Debug, Clone, Copy)]
pub(super) struct FactScope<'rule> {
    /// The rule's selector, empty when it didn't validate.
    pub(super) selector: &'rule str,
    /// The rule's integration, when it validated.
    pub(super) integration: Option<IntegrationId>,
    /// Where the pack comes from.
    pub(super) origin: Origin,
    /// What the pack may reference.
    pub(super) catalog: Catalog<'rule>,
}

impl FactScope<'_> {
    /// The integration that publishes a fact called `name`, if any does.
    fn publisher(&self, name: &str) -> Option<&IntegrationDescriptor> {
        self.catalog
            .integrations
            .iter()
            .find(|i| i.facts.iter().any(|fact| fact.name == name))
    }

    /// `name` among the rule's own integration's facts.
    fn own(&self, name: &str) -> Option<&'static FactSpec> {
        let mut integrations = self.catalog.integrations.iter();
        let own = integrations.find(|i| Some(i.id) == self.integration)?;
        own.facts.iter().find(|fact| fact.name == name)
    }

    /// Every fact name, whoever publishes it: a rule that can't use one is told why.
    fn names(&self) -> impl Iterator<Item = &'static str> {
        let published = self.catalog.integrations.iter();
        GENERIC_FACTS
            .iter()
            .chain(published.flat_map(|i| i.facts))
            .map(|fact| fact.name)
    }
}

/// The rule's `requirements` and `eligibility`.
pub(super) fn rule_predicates(
    c: &mut Checker<'_>,
    rule: &Table<'_>,
    at: Range<usize>,
    scope: FactScope<'_>,
) -> Option<RulePredicates> {
    let tables: Vec<Vec<Entry<'_, '_>>> = ["requirements", "eligibility"]
        .into_iter()
        .map(|key| {
            let table = rule.get(key).and_then(|value| c.table(value));
            table.map_or_else(Vec::new, |table| entries(table, "", scope))
        })
        .collect();
    let total: usize = tables.iter().map(Vec::len).sum();
    if total > MAX_PREDICATES {
        let message = format!("a rule holds at most {MAX_PREDICATES} predicates");
        c.error(ErrorCode::TooManyPredicates, at, message);
        return None;
    }
    let mut parsed = tables
        .into_iter()
        .map(|entries| predicates(c, entries, scope));
    let requirements = parsed.next().flatten();
    let eligibility = parsed.next().flatten();
    Some(RulePredicates {
        requirements: requirements?,
        eligibility: eligibility?,
    })
}

/// One predicate as written: the fact's full name, where it is, and the test.
type Entry<'t, 'i> = (String, Range<usize>, &'t Value<'i>);

/// The predicates of `table`. A dotted fact name written without quotes, such as
/// `git.tracked_changes = 0`, is a nested table in TOML, so it is flattened back.
fn entries<'t, 'i>(table: &'t Table<'i>, prefix: &str, scope: FactScope<'_>) -> Vec<Entry<'t, 'i>> {
    let mut found = Vec::new();
    for (key, value) in table {
        let name = format!("{prefix}{}", key.get_ref());
        let nested = format!("{name}.");
        let is_prefix = scope.names().any(|fact| fact.starts_with(&nested));
        match value.get_ref().as_table() {
            Some(inner) if is_prefix && !scope.names().any(|fact| fact == name) => {
                found.extend(entries(inner, &nested, scope));
            }
            _ => found.push((name, key.span(), value)),
        }
    }
    found
}

fn predicates(
    c: &mut Checker<'_>,
    entries: Vec<Entry<'_, '_>>,
    scope: FactScope<'_>,
) -> Option<Vec<Predicate>> {
    let mut predicates = Vec::new();
    let mut valid = true;
    for (name, at, value) in entries {
        let parsed = fact(c, &name, at, scope).and_then(|fact| {
            Some(Predicate {
                fact: fact.name,
                test: test(c, value, fact.kind)?,
            })
        });
        match parsed {
            Some(predicate) => predicates.push(predicate),
            None => valid = false,
        }
    }
    valid.then_some(predicates)
}

/// The published fact called `name`, if the rule may test it: a generic fact, then one of
/// its own integration's.
fn fact(
    c: &mut Checker<'_>,
    name: &str,
    at: Range<usize>,
    scope: FactScope<'_>,
) -> Option<&'static FactSpec> {
    let generic = GENERIC_FACTS.iter().find(|fact| fact.name == name);
    let Some(fact) = generic.or_else(|| scope.own(name)) else {
        unavailable(c, name, at, scope);
        return None;
    };
    if let Some(only) = fact.selector.filter(|only| *only != scope.selector) {
        let message = format!("`{name}` applies only to the {only} selector");
        c.error(ErrorCode::UnknownCapability, at, message);
        return None;
    }
    Some(fact)
}

/// Reports why the rule can't test `name`.
fn unavailable(c: &mut Checker<'_>, name: &str, at: Range<usize>, scope: FactScope<'_>) {
    let publisher = scope.publisher(name).map(|i| i.id);
    let (code, message) = match (publisher, scope.origin) {
        (Some(_), Origin::ThirdParty) => (
            ErrorCode::NotPermitted,
            format!("`{name}` is an integration fact, which third-party rules may not use"),
        ),
        (Some(owner), Origin::Builtin) => (
            ErrorCode::UnknownCapability,
            format!("`{name}` is published by the {owner} integration, not this rule's"),
        ),
        (None, _) => (
            ErrorCode::UnknownCapability,
            format!("kipple {} publishes no fact {name:?}", scope.catalog.engine),
        ),
    };
    c.error(code, at, message);
}

fn test(c: &mut Checker<'_>, value: &Value<'_>, kind: FactKind) -> Option<Test> {
    match kind {
        FactKind::Enum(values) => one_of(c, value, values).map(Test::OneOf),
        FactKind::Bool => c.bool(value).map(Test::Is),
        FactKind::Duration => bounds(c, value, duration).map(Test::Duration),
        FactKind::Bytes => bounds(c, value, bytes).map(Test::Bytes),
        FactKind::Count => count(c, value).map(Test::Count),
    }
}

/// `"value"` or `["a", "b"]`, each one of `values`.
fn one_of(
    c: &mut Checker<'_>,
    value: &Value<'_>,
    values: &'static [&'static str],
) -> Option<Vec<&'static str>> {
    let expected = format!("one of {}", values.join(", "));
    let pick = |checker: &mut Checker<'_>, item: &Value<'_>| {
        checker.one_of(
            item,
            |s| values.iter().copied().find(|v| *v == s),
            &expected,
        )
    };
    if value.get_ref().is_str() {
        return pick(c, value).map(|v| vec![v]);
    }
    let items = c.array(value)?;
    if items.is_empty() {
        c.error(
            ErrorCode::InvalidRange,
            value.span(),
            "name at least one value",
        );
        return None;
    }
    items.iter().map(|item| pick(c, item)).collect()
}

/// `"absent"`, `"present"` or `{ min = n, max = m }`.
fn count(c: &mut Checker<'_>, value: &Value<'_>) -> Option<Bounds<u64>> {
    match value.get_ref().as_str() {
        Some("absent") => Some(Bounds {
            min: Some(0),
            max: Some(0),
        }),
        Some("present") => Some(Bounds {
            min: Some(1),
            max: None,
        }),
        Some(other) => {
            let message = format!("expected \"absent\", \"present\" or a range, found {other:?}");
            c.error(ErrorCode::WrongType, value.span(), message);
            None
        }
        None => bounds(c, value, |c, v| {
            u64::try_from(c.integer_in(v, COUNT.clone())?).ok()
        }),
    }
}

/// `{ min = .., max = .. }` with at least one bound and `min <= max`.
fn bounds<T: Ord + Copy>(
    c: &mut Checker<'_>,
    value: &Value<'_>,
    parse: impl Fn(&mut Checker<'_>, &Value<'_>) -> Option<T>,
) -> Option<Bounds<T>> {
    let table = c.table(value)?;
    c.known_keys(table, &["min", "max"]);
    let mut bound = |key| table.get(key).map(|v| parse(c, v));
    let (min, max) = (bound("min"), bound("max"));
    if min.is_none() && max.is_none() {
        c.error(
            ErrorCode::InvalidRange,
            value.span(),
            "a range needs `min`, `max` or both",
        );
        return None;
    }
    let bounds = Bounds {
        min: min.map_or(Some(None), |b| b.map(Some))?,
        max: max.map_or(Some(None), |b| b.map(Some))?,
    };
    if let (Some(low), Some(high)) = (bounds.min, bounds.max)
        && low > high
    {
        c.error(
            ErrorCode::InvalidRange,
            value.span(),
            "`min` is greater than `max`",
        );
        return None;
    }
    Some(bounds)
}

fn duration(c: &mut Checker<'_>, value: &Value<'_>) -> Option<Duration> {
    let text = c.string(value, usize::MAX)?;
    let Some(duration) = grammar::duration(text) else {
        let message =
            format!("expected a duration such as \"30d\" (s, m, h, d, w or y), found {text:?}");
        c.error(ErrorCode::WrongType, value.span(), message);
        return None;
    };
    if duration > grammar::MAX_DURATION {
        c.error(
            ErrorCode::InvalidRange,
            value.span(),
            "a duration is at most 100 years",
        );
        return None;
    }
    Some(duration)
}

fn bytes(c: &mut Checker<'_>, value: &Value<'_>) -> Option<u64> {
    let text = c.string(value, usize::MAX)?;
    let parsed = grammar::bytes(text);
    if parsed.is_none() {
        let message = format!("expected a size such as \"100MB\" or \"10GiB\", found {text:?}");
        c.error(ErrorCode::WrongType, value.span(), message);
    }
    parsed
}
