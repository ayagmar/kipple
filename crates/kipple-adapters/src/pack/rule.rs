//! The `[[rule]]` tables of a pack.

use std::collections::HashSet;
use std::ops::Range;

use kipple_core::{
    Category, GENERIC_SELECTORS, IntegrationId, Method, NativeKind, OPERATIONS, Operation,
    OperationSpec, PackId, Platform, Rule, RuleId, RuleText, Selector, SymbolicRoot,
};

use super::doc::{Checker, Table, Value};
use super::predicate::FactScope;
use super::{Catalog, ErrorCode, MAX_RULES, Origin, grammar, predicate, retention};

const RULE_KEYS: &[&str] = &[
    "id",
    "integration",
    "title",
    "description",
    "why",
    "loss",
    "references",
    "category",
    "platforms",
    "scope",
    "requirements",
    "eligibility",
    "retention",
    "operation",
    "selection",
];

/// The most references a rule may cite.
const MAX_REFERENCES: usize = 5;
/// The default and range of `max_matches`.
const MAX_MATCHES: (i64, Range<i64>) = (1000, 1..10_001);
/// The default and range of `max_depth`.
const MAX_DEPTH: (i64, Range<i64>) = (6, 1..13);
/// How many marker names `project-artifact` takes.
const MARKERS: Range<usize> = 1..9;
/// The range of `selection.recent_days`.
const RECENT_DAYS: Range<i64> = 0..366;

/// Integration selectors whose findings one operation alone may remove, each with that
/// operation. Anything else they pair with has to be `report`.
const REMOVED_ONLY_BY: &[(&str, &str)] = &[
    (
        "registered-linked-worktrees",
        NativeKind::GitWorktreeRemove.name(),
    ),
    ("verified-release-directories", "remove-verified-release"),
];

/// What every rule of one pack is checked against.
#[derive(Debug, Clone, Copy)]
pub(super) struct RuleCx<'pack> {
    /// The pack's ID, which prefixes every rule ID.
    pub(super) pack: &'pack PackId,
    /// The pack's platforms, which a rule may only narrow.
    pub(super) platforms: &'pack [Platform],
    /// Where the pack comes from.
    pub(super) origin: Origin,
    /// What the pack may reference.
    pub(super) catalog: Catalog<'pack>,
}

impl RuleCx<'_> {
    /// Every `[[rule]]` table, from the array that spans `at`.
    pub(super) fn rules(
        &self,
        c: &mut Checker<'_>,
        items: &[Value<'_>],
        at: Range<usize>,
    ) -> Option<Vec<Rule>> {
        if items.is_empty() {
            let message = "a pack needs at least one `[[rule]]`";
            c.error(ErrorCode::MissingKey, at, message);
            return None;
        }
        if let Some(extra) = items.get(MAX_RULES) {
            let message = format!("a pack holds at most {MAX_RULES} rules");
            c.error(ErrorCode::TooManyRules, extra.span(), message);
            return None;
        }
        let mut seen = HashSet::new();
        let rules: Vec<Option<Rule>> = items
            .iter()
            .map(|item| self.rule(c, item, &mut seen))
            .collect();
        rules.into_iter().collect()
    }

    fn rule(
        &self,
        c: &mut Checker<'_>,
        value: &Value<'_>,
        seen: &mut HashSet<String>,
    ) -> Option<Rule> {
        let before = c.error_count();
        let table = c.table(value)?;
        let at = value.span();
        c.known_keys(table, RULE_KEYS);
        let id = self.id(c, table, at.clone(), seen);
        let integration = self.integration(c, table, at.clone());
        let text = text(c, table, at.clone());
        let category = c
            .required(table, at.clone(), "category")
            .and_then(|v| c.one_of(v, Category::from_name, "a category"));
        let platforms = self.platforms(c, table);
        let scope =
            integration.and_then(|integration| self.scope(c, table, at.clone(), integration));
        let selector_name = scope.as_ref().map_or("", |(_, selector)| selector.name());
        let facts = FactScope {
            selector: selector_name,
            integration,
            origin: self.origin,
            catalog: self.catalog,
        };
        let predicates = predicate::rule_predicates(c, table, at.clone(), facts);
        let operation = self.operation(c, table, at.clone());
        if let Some(operation) = &operation {
            fits_operation(c, table, at, selector_name, operation);
        }
        let recent_days = selection(c, table);
        if c.error_count() != before {
            return None;
        }
        let ((root, selector), predicates) = (scope?, predicates?);
        Some(Rule {
            id: id?,
            integration: integration?,
            text: text?,
            category: category?,
            platforms: platforms?,
            root,
            selector,
            requirements: predicates.requirements,
            eligibility: predicates.eligibility,
            operation: operation?,
            recent_days,
        })
    }

    fn id(
        &self,
        c: &mut Checker<'_>,
        table: &Table<'_>,
        at: Range<usize>,
        seen: &mut HashSet<String>,
    ) -> Option<RuleId> {
        let value = c.required(table, at, "id")?;
        let id = c.string(value, usize::MAX)?;
        let pack = &self.pack.0;
        if !grammar::is_rule_id(id, pack) {
            let message =
                format!("rule ID {id:?} must be `{pack}.<name>` with a name of `[a-z0-9-]+`");
            c.error(ErrorCode::InvalidId, value.span(), message);
            return None;
        }
        if !seen.insert(id.to_owned()) {
            c.error(
                ErrorCode::DuplicateId,
                value.span(),
                format!("rule ID {id:?} is used twice"),
            );
            return None;
        }
        Some(RuleId(id.to_owned()))
    }

    /// `generic`, or a built-in integration for a built-in pack.
    fn integration(
        &self,
        c: &mut Checker<'_>,
        table: &Table<'_>,
        at: Range<usize>,
    ) -> Option<IntegrationId> {
        let value = c.required(table, at, "integration")?;
        let name = c.string(value, usize::MAX)?;
        if name == IntegrationId::GENERIC.0 {
            return Some(IntegrationId::GENERIC);
        }
        if self.origin == Origin::ThirdParty {
            let message = format!("third-party rules use the `generic` integration, not {name:?}");
            c.error(ErrorCode::NotPermitted, value.span(), message);
            return None;
        }
        let found = self.catalog.integrations.iter().find(|i| i.id.0 == name);
        if found.is_none() {
            let message = format!("kipple {} has no integration {name:?}", self.catalog.engine);
            c.error(ErrorCode::UnknownCapability, value.span(), message);
        }
        found.map(|descriptor| descriptor.id)
    }

    /// The rule's platforms: a subset of the pack's, which they default to.
    fn platforms(&self, c: &mut Checker<'_>, table: &Table<'_>) -> Option<Vec<Platform>> {
        let Some(value) = table.get("platforms") else {
            return Some(self.platforms.to_vec());
        };
        let platforms = super::platforms(c, value)?;
        if let Some(extra) = platforms.iter().find(|p| !self.platforms.contains(p)) {
            let message = format!("{extra} is not one of the pack's platforms");
            c.error(ErrorCode::PlatformMismatch, value.span(), message);
            return None;
        }
        Some(platforms)
    }

    /// `[rule.scope]`: the root and the selector with its parameters.
    fn scope(
        &self,
        c: &mut Checker<'_>,
        table: &Table<'_>,
        at: Range<usize>,
        integration: IntegrationId,
    ) -> Option<(SymbolicRoot, Selector)> {
        let value = c.required(table, at, "scope")?;
        let scope = c.table(value)?;
        let root_value = c.required(scope, value.span(), "root");
        let root = root_value.and_then(|v| self.root(c, v));
        let selection = c.required(scope, value.span(), "selection")?;
        let selector = self.selector(c, scope, selection, integration)?;
        let (root, root_value) = (root?, root_value?);
        if root == SymbolicRoot::Home && !home_scope_is_bounded(&selector) {
            let message = "`home` needs a pattern whose first component is a literal name that isn't only dots";
            c.error(ErrorCode::InvalidPattern, root_value.span(), message);
            return None;
        }
        Some((root, selector))
    }

    fn root(&self, c: &mut Checker<'_>, value: &Value<'_>) -> Option<SymbolicRoot> {
        let name = c.string(value, usize::MAX)?;
        let root = SymbolicRoot::from_name(name);
        if root.is_none() {
            let message = format!("kipple {} publishes no root {name:?}", self.catalog.engine);
            c.error(ErrorCode::UnknownCapability, value.span(), message);
        }
        root
    }

    fn selector(
        &self,
        c: &mut Checker<'_>,
        scope: &Table<'_>,
        selection: &Value<'_>,
        integration: IntegrationId,
    ) -> Option<Selector> {
        let name = c.string(selection, usize::MAX)?;
        if let Some((_, params)) = GENERIC_SELECTORS.iter().find(|(n, _)| *n == name) {
            let keys: Vec<&str> = ["root", "selection"]
                .iter()
                .chain(*params)
                .copied()
                .collect();
            c.known_keys(scope, &keys);
            return generic_selector(c, scope, selection.span(), name);
        }
        let found = self
            .catalog
            .integrations
            .iter()
            .find_map(|i| Some((i.id, i.selectors.iter().find(|s| s.name == name)?)));
        let Some((owner, published)) = found else {
            let message = format!(
                "kipple {} publishes no selector {name:?}",
                self.catalog.engine
            );
            c.error(ErrorCode::UnknownCapability, selection.span(), message);
            return None;
        };
        if self.origin == Origin::ThirdParty {
            let message =
                format!("{name:?} is an integration selector, which third-party rules may not use");
            c.error(ErrorCode::NotPermitted, selection.span(), message);
            return None;
        }
        if owner != integration {
            let message =
                format!("{name:?} is published by the {owner} integration, not {integration}");
            c.error(ErrorCode::UnknownCapability, selection.span(), message);
            return None;
        }
        c.known_keys(scope, &["root", "selection"]);
        Some(Selector::Integration(published.name))
    }

    /// `[rule.operation]`, with the `[rule.retention]` it declares.
    fn operation(
        &self,
        c: &mut Checker<'_>,
        table: &Table<'_>,
        at: Range<usize>,
    ) -> Option<Operation> {
        let value = c.required(table, at.clone(), "operation")?;
        let operation = c.table(value)?;
        let kind = c.required(operation, value.span(), "kind")?;
        let spec = self.published(c, kind)?;
        let methods = operation_methods(c, operation, value.span(), spec);
        if spec.retention {
            let retention = retention::of_rule(c, table, at)?;
            return methods.map(|methods| Operation::RemoveVerifiedRelease { methods, retention });
        }
        retention::refuse(c, table, spec);
        if let Some(native) = NativeKind::from_name(spec.name) {
            return Some(Operation::Native(native));
        }
        match spec.name {
            "report" => Some(Operation::Report),
            "remove-files" => methods.map(Operation::RemoveFiles),
            other => {
                let message = format!("kipple can't read a rule using {other:?}");
                c.error(ErrorCode::UnknownCapability, kind.span(), message);
                None
            }
        }
    }

    /// The operation `kind` names, if this pack may use it.
    fn published(&self, c: &mut Checker<'_>, kind: &Value<'_>) -> Option<&'static OperationSpec> {
        let name = c.string(kind, usize::MAX)?;
        let Some(spec) = OPERATIONS.iter().find(|spec| spec.name == name) else {
            let message = format!(
                "kipple {} publishes no operation {name:?}; it publishes {}",
                self.catalog.engine,
                self.published_operations().join(", ")
            );
            c.error(ErrorCode::UnknownCapability, kind.span(), message);
            return None;
        };
        if spec.first_party && self.origin == Origin::ThirdParty {
            let message = format!("{name:?} is first-party: third-party rules may not use it");
            c.error(ErrorCode::NotPermitted, kind.span(), message);
            return None;
        }
        Some(spec)
    }

    /// The operations this pack can name.
    fn published_operations(&self) -> Vec<&'static str> {
        OPERATIONS
            .iter()
            .filter(|spec| !spec.first_party || self.origin == Origin::Builtin)
            .map(|spec| spec.name)
            .collect()
    }
}

/// The methods of an operation that chooses one, none for any other.
fn operation_methods(
    c: &mut Checker<'_>,
    operation: &Table<'_>,
    at: Range<usize>,
    spec: &OperationSpec,
) -> Option<Vec<Method>> {
    if spec.methods.is_empty() {
        c.known_keys(operation, &["kind"]);
        return Some(Vec::new());
    }
    c.known_keys(operation, &["kind", "methods"]);
    let methods = c.required(operation, at, "methods")?;
    methods_of(c, methods, spec.methods)
}

/// Refuses what `operation` can't carry out: predicates on a report rule, which would
/// decide nothing, and a removal of what its selector's findings aren't.
fn fits_operation(
    c: &mut Checker<'_>,
    table: &Table<'_>,
    at: Range<usize>,
    selector: &str,
    operation: &Operation,
) {
    if *operation == Operation::Report {
        for key in ["requirements", "eligibility"] {
            if let Some(value) = table.get(key) {
                let message = format!("unknown key `{key}`: a report rule decides nothing");
                c.error(ErrorCode::UnknownKey, value.span(), message);
            }
        }
        return;
    }
    let only = REMOVED_ONLY_BY.iter().find(|(name, _)| *name == selector);
    if let Some((_, only)) = only.filter(|(_, only)| *only != operation.name()) {
        let message = format!(
            "{selector:?} pairs only with {only:?} or `report`, not {:?}",
            operation.name()
        );
        c.error(ErrorCode::UnknownCapability, at, message);
    }
}

/// The text the rule shows people.
fn text(c: &mut Checker<'_>, table: &Table<'_>, at: Range<usize>) -> Option<RuleText> {
    let required = |checker: &mut Checker<'_>, key: &str, limit| {
        checker
            .required(table, at.clone(), key)
            .and_then(|v| checker.string(v, limit))
            .map(str::to_owned)
    };
    let title = required(c, "title", 200);
    let why = required(c, "why", 300);
    let loss = required(c, "loss", 300);
    let description = table.get("description").map_or_else(
        || Some(String::new()),
        |value| c.string(value, 1000).map(str::to_owned),
    );
    let references = table
        .get("references")
        .map_or_else(|| Some(Vec::new()), |value| references(c, value));
    Some(RuleText {
        title: title?,
        description: description?,
        why: why?,
        loss: loss?,
        references: references?,
    })
}

/// At most five HTTPS URLs.
fn references(c: &mut Checker<'_>, value: &Value<'_>) -> Option<Vec<String>> {
    let items = c.array(value)?;
    if items.len() > MAX_REFERENCES {
        let message = format!("at most {MAX_REFERENCES} references");
        c.error(ErrorCode::InvalidRange, value.span(), message);
        return None;
    }
    let mut urls = Vec::new();
    for item in items {
        let url = c.string(item, usize::MAX)?;
        let host = url.strip_prefix("https://").unwrap_or_default();
        if host.is_empty() || host.starts_with('/') || url.contains(char::is_whitespace) {
            c.error(
                ErrorCode::WrongType,
                item.span(),
                format!("expected an HTTPS URL, found {url:?}"),
            );
            return None;
        }
        urls.push(url.to_owned());
    }
    Some(urls)
}

fn generic_selector(
    c: &mut Checker<'_>,
    scope: &Table<'_>,
    at: Range<usize>,
    name: &str,
) -> Option<Selector> {
    if name == "project-artifact" {
        let markers = c
            .required(scope, at.clone(), "marker")
            .and_then(|v| markers(c, v));
        let artifact = c
            .required(scope, at, "artifact")
            .and_then(|v| literal(c, v));
        let max_depth = optional_integer(c, scope, "max_depth", MAX_DEPTH);
        return Some(Selector::ProjectArtifact {
            markers: markers?,
            artifact: artifact?,
            max_depth: max_depth?,
        });
    }
    let pattern = c.required(scope, at, "pattern").and_then(|value| {
        let text = c.raw_string(value)?;
        grammar::pattern(text)
            .map_err(|message| c.error(ErrorCode::InvalidPattern, value.span(), message))
            .ok()
    });
    let max_matches = optional_integer(c, scope, "max_matches", MAX_MATCHES);
    let (pattern, max_matches) = (pattern?, max_matches?);
    Some(if name == "dirs-matching" {
        Selector::DirsMatching {
            pattern,
            max_matches,
        }
    } else {
        Selector::FilesMatching {
            pattern,
            max_matches,
        }
    })
}

/// A literal name, such as an artifact directory.
fn literal(c: &mut Checker<'_>, value: &Value<'_>) -> Option<kipple_core::Name> {
    let text = c.raw_string(value)?;
    grammar::name(text)
        .map_err(|message| c.error(ErrorCode::InvalidPattern, value.span(), message))
        .ok()
}

fn markers(c: &mut Checker<'_>, value: &Value<'_>) -> Option<Vec<kipple_core::Name>> {
    let items = c.array(value)?;
    if !MARKERS.contains(&items.len()) {
        let message = format!("name {} to {} marker files", MARKERS.start, MARKERS.end - 1);
        c.error(ErrorCode::InvalidRange, value.span(), message);
        return None;
    }
    items.iter().map(|item| literal(c, item)).collect()
}

/// An optional integer parameter with its default and range.
fn optional_integer(
    c: &mut Checker<'_>,
    table: &Table<'_>,
    key: &str,
    (default, range): (i64, Range<i64>),
) -> Option<u32> {
    let n = table
        .get(key)
        .map_or(Some(default), |v| c.integer_in(v, range))?;
    u32::try_from(n).ok()
}

fn methods_of(c: &mut Checker<'_>, value: &Value<'_>, offered: &[Method]) -> Option<Vec<Method>> {
    let items = c.array(value)?;
    if items.is_empty() {
        c.error(
            ErrorCode::InvalidRange,
            value.span(),
            "name at least one method",
        );
        return None;
    }
    let mut methods = Vec::new();
    for item in items {
        let from_name = |name: &str| Method::from_name(name).filter(|m| offered.contains(m));
        let method = c.one_of(item, from_name, "a method: delete or trash")?;
        if !methods.contains(&method) {
            methods.push(method);
        }
    }
    Some(methods)
}

/// `[rule.selection]`: `default` may only be `unselected`, and `recent_days` lengthens the
/// recent window.
///
/// `None` when there is no `recent_days`, or when the table is invalid: the caller sees
/// the reported error.
fn selection(c: &mut Checker<'_>, table: &Table<'_>) -> Option<u16> {
    let value = table.get("selection")?;
    let selection = c.table(value)?;
    c.known_keys(selection, &["default", "recent_days"]);
    if let Some(default) = selection.get("default") {
        c.one_of(
            default,
            |s| (s == "unselected").then_some(()),
            "\"unselected\"",
        )?;
    }
    let days = c.integer_in(selection.get("recent_days")?, RECENT_DAYS)?;
    u16::try_from(days).ok()
}

/// Whether a `home` rule's scope stops short of the whole home directory: its pattern
/// starts with a literal name that isn't only dots.
fn home_scope_is_bounded(selector: &Selector) -> bool {
    let (Selector::DirsMatching { pattern, .. } | Selector::FilesMatching { pattern, .. }) =
        selector
    else {
        return false;
    };
    pattern
        .0
        .first()
        .is_some_and(|first| grammar::is_literal(first) && !first.chars().all(|c| c == '.'))
}
