//! `cargo xtask check-compat`: every JSON schema kipple publishes still describes what the
//! readers of its last released baseline rely on (`docs/08-engineering-standards.md` §8).

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use serde_json::{Map, Value};

use crate::gen_docs;

/// Where the baselines live: `<format>/<version>/`, holding `schema.json` and the sample
/// documents `crates/kipple/tests/compat.rs` reads.
const BASELINES: &str = "crates/kipple/tests/compat";

/// Who reads documents of a format, which decides what a compatible change is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadBy {
    /// kipple writes them and scripts read them. A reader ignores properties it doesn't
    /// know, so a new property is compatible; anything else kipple may now write that the
    /// baseline doesn't allow is not.
    Others,
    /// People, or an earlier kipple, write them and kipple reads them. Every document the
    /// baseline allows must still be read.
    Kipple,
}

/// A published schema and its version.
#[derive(Debug)]
struct Format {
    /// Its name in `kipple::json::schemas()`, and its directory under [`BASELINES`].
    name: &'static str,
    /// Its version; the part after the last `/` names the baseline's directory.
    version: &'static str,
    read_by: ReadBy,
}

impl Format {
    fn dir(&self) -> &'static str {
        self.version
            .rsplit_once('/')
            .map_or(self.version, |(_, version)| version)
    }
}

const FORMATS: &[Format] = &[
    Format {
        name: "doctor",
        version: kipple::json::DOCTOR,
        read_by: ReadBy::Others,
    },
    Format {
        name: "event",
        version: kipple::json::EVENT,
        read_by: ReadBy::Others,
    },
    Format {
        name: "rules",
        version: kipple::json::RULES,
        read_by: ReadBy::Others,
    },
    Format {
        name: "scan",
        version: kipple::json::SCAN,
        read_by: ReadBy::Others,
    },
    Format {
        name: "support-matrix",
        version: kipple::json::SUPPORT_MATRIX,
        read_by: ReadBy::Others,
    },
    // A theme file says `version = 1`.
    Format {
        name: "theme",
        version: "v1",
        read_by: ReadBy::Kipple,
    },
];

/// Fails when a published schema breaks its baseline or has none. With `refresh`, a
/// schema that is compatible, or whose version has no baseline yet, becomes its baseline.
pub(crate) fn run(root: &Path, refresh: bool) -> Result<()> {
    let schemas = kipple::json::schemas();
    let names: Vec<&str> = schemas.iter().map(|(name, _)| *name).collect();
    let mut problems = unlisted(&names);
    for (name, schema) in &schemas {
        let Some(format) = FORMATS.iter().find(|f| f.name == *name) else {
            continue;
        };
        let written = refresh.then(|| gen_docs::pretty(schema)).transpose()?;
        problems.extend(check(root, format, schema.as_value(), written.as_deref())?);
    }
    if problems.is_empty() {
        return Ok(());
    }
    let mut stderr = io::stderr().lock();
    for problem in &problems {
        writeln!(stderr, "check-compat: {problem}")?;
    }
    bail!(
        "{} incompatible change(s), see docs/08-engineering-standards.md §8",
        problems.len()
    )
}

/// Published schemas missing from [`FORMATS`], and formats no longer published.
fn unlisted(published: &[&str]) -> Vec<String> {
    let mut problems: Vec<String> = published
        .iter()
        .filter(|name| !FORMATS.iter().any(|f| f.name == **name))
        .map(|name| format!("{name}: published but not in FORMATS (xtask/src/compat.rs)"))
        .collect();
    problems.extend(
        FORMATS
            .iter()
            .filter(|f| !published.contains(&f.name))
            .map(|f| format!("{} {}: no longer published", f.name, f.version)),
    );
    problems
}

/// What breaks `format`'s baseline. When nothing does, `refresh` (the current schema as
/// written) becomes the baseline.
fn check(
    root: &Path,
    format: &Format,
    current: &Value,
    refresh: Option<&str>,
) -> Result<Vec<String>> {
    let relative = format!("{BASELINES}/{}/{}/schema.json", format.name, format.dir());
    let path = root.join(&relative);
    let problems = match fs::read_to_string(&path) {
        Ok(text) => {
            let baseline: Value = serde_json::from_str(&text)
                .with_context(|| format!("could not parse {relative}"))?;
            changes(&baseline, current, format.read_by)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound && refresh.is_none() => {
            vec![format!(
                "no baseline at {relative}; a new format or version needs one \
             (`cargo xtask check-compat --refresh`)"
            )]
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error).with_context(|| format!("could not read {relative}")),
    };
    if let Some(text) = refresh
        && problems.is_empty()
    {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
        }
        fs::write(&path, text).with_context(|| format!("could not write {relative}"))?;
    }
    let label = format!("{} {}", format.name, format.version);
    Ok(problems
        .into_iter()
        .map(|problem| format!("{label}: {problem}"))
        .collect())
}

/// Keywords that only document a schema.
const ANNOTATIONS: &[&str] = &[
    "$schema",
    "$defs",
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
];

/// Keywords the comparison understands. Any other makes it fail rather than pass a
/// change it can't judge.
const CHECKED: &[&str] = &[
    "$ref",
    "type",
    "enum",
    "const",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "anyOf",
    "oneOf",
    "minimum",
    "maximum",
    "pattern",
    "format",
];

/// Every change from `old` to `new` that breaks a reader of `old`, worded for people.
fn changes(old: &Value, new: &Value, read_by: ReadBy) -> Vec<String> {
    let mut diff = Diff {
        old_root: old,
        new_root: new,
        read_by,
        seen: BTreeSet::new(),
        changes: Vec::new(),
    };
    diff.schema("", old, new);
    diff.changes
}

/// One comparison of a baseline schema (`old`) with the current one (`new`).
///
/// Each check finds what one side allows that the other doesn't. That side is `new` when
/// others read the format (kipple may now write it), and `old` when kipple reads it
/// (kipple no longer reads it).
struct Diff<'schema> {
    old_root: &'schema Value,
    new_root: &'schema Value,
    read_by: ReadBy,
    /// The `$ref` pairs already compared: each is reported once, and a recursive schema
    /// ends.
    seen: BTreeSet<(String, String)>,
    changes: Vec<String>,
}

impl Diff<'_> {
    fn schema(&mut self, at: &str, old: &Value, new: &Value) {
        let (old, old_ref) = match resolve(self.old_root, old) {
            Ok(resolved) => resolved,
            Err(problem) => return self.unsupported(at, &problem),
        };
        let (new, new_ref) = match resolve(self.new_root, new) {
            Ok(resolved) => resolved,
            Err(problem) => return self.unsupported(at, &problem),
        };
        if let (Some(old_ref), Some(new_ref)) = (old_ref, new_ref)
            && !self.seen.insert((old_ref.to_owned(), new_ref.to_owned()))
        {
            return;
        }
        if let Some(problem) = unsupported(old).or_else(|| unsupported(new)) {
            return self.unsupported(at, &problem);
        }
        if is_union(old) || is_union(new) {
            self.union(at, old, new);
        } else {
            self.single(at, old, new);
        }
    }

    /// Compares schemas that are a union of variants on at least one side: every variant
    /// of the side that must fit has to fit one variant of the other.
    fn union(&mut self, at: &str, old: &Value, new: &Value) {
        let (old_variants, new_variants) =
            match (variants(self.old_root, old), variants(self.new_root, new)) {
                (Ok(old_list), Ok(new_list)) => (old_list, new_list),
                (Err(problem), _) | (_, Err(problem)) => return self.unsupported(at, &problem),
            };
        let read_by = self.read_by;
        let (fitting, fitted) = match read_by {
            ReadBy::Others => (&new_variants, &old_variants),
            ReadBy::Kipple => (&old_variants, &new_variants),
        };
        for variant in fitting {
            let pair = |other| match read_by {
                ReadBy::Others => (other, variant),
                ReadBy::Kipple => (variant, other),
            };
            if fitted.iter().any(|other| {
                let (old_variant, new_variant) = pair(other);
                self.fits(at, old_variant, new_variant)
            }) {
                continue;
            }
            let wanted = label(variant);
            if let Some(other) = fitted.iter().find(|other| label(other) == wanted) {
                let (old_variant, new_variant) = pair(other);
                self.schema(&format!("{at}<{wanted}>"), old_variant, new_variant);
            } else {
                self.escapes(at, &format!("the variant {wanted}"));
            }
        }
    }

    /// Whether `old` and `new` compare without a change, leaving this comparison as it was.
    fn fits(&self, at: &str, old: &Value, new: &Value) -> bool {
        let mut trial = Diff {
            old_root: self.old_root,
            new_root: self.new_root,
            read_by: self.read_by,
            seen: self.seen.clone(),
            changes: Vec::new(),
        };
        trial.schema(at, old, new);
        trial.changes.is_empty()
    }

    fn single(&mut self, at: &str, old: &Value, new: &Value) {
        self.types(at, old, new);
        self.values(at, old, new);
        self.bounds(at, old, new);
        for keyword in ["pattern", "format"] {
            self.same_text(at, old, new, keyword);
        }
        self.items(at, old, new);
        self.properties(at, old, new);
    }

    /// The side whose every document the other must allow, then that other side.
    const fn sides<'v>(&self, old: &'v Value, new: &'v Value) -> (&'v Value, &'v Value) {
        match self.read_by {
            ReadBy::Others => (old, new),
            ReadBy::Kipple => (new, old),
        }
    }

    fn types(&mut self, at: &str, old: &Value, new: &Value) {
        let (wide, narrow) = self.sides(old, new);
        let Some(allowed) = types(wide) else {
            return;
        };
        let Some(types) = types(narrow) else {
            return self.escapes(at, &format!("types other than {}", allowed.join(", ")));
        };
        for kind in types {
            let integer_in_number = kind == "integer" && allowed.contains(&"number");
            if !allowed.contains(&kind) && !integer_in_number {
                self.escapes(at, &format!("the type {kind}"));
            }
        }
    }

    fn values(&mut self, at: &str, old: &Value, new: &Value) {
        let (wide, narrow) = self.sides(old, new);
        let Some(allowed) = values(wide) else {
            return;
        };
        let Some(values) = values(narrow) else {
            let list: Vec<String> = allowed.iter().map(ToString::to_string).collect();
            return self.escapes(at, &format!("values other than {}", list.join(", ")));
        };
        for value in values {
            if !allowed.contains(&value) {
                self.escapes(at, &format!("the value {value}"));
            }
        }
    }

    fn bounds(&mut self, at: &str, old: &Value, new: &Value) {
        let (wide, narrow) = self.sides(old, new);
        let bound = |schema: &Value, keyword| schema.get(keyword).and_then(Value::as_f64);
        if let Some(min) = bound(wide, "minimum")
            && bound(narrow, "minimum").is_none_or(|own| own < min)
        {
            self.escapes(at, &format!("values below {min}"));
        }
        if let Some(max) = bound(wide, "maximum")
            && bound(narrow, "maximum").is_none_or(|own| own > max)
        {
            self.escapes(at, &format!("values above {max}"));
        }
    }

    /// A `pattern` or `format`, which can only be compared for equality.
    fn same_text(&mut self, at: &str, old: &Value, new: &Value, keyword: &str) {
        let (wide, narrow) = self.sides(old, new);
        if let Some(required) = wide.get(keyword)
            && narrow.get(keyword) != Some(required)
        {
            self.escapes(at, &format!("a {keyword} other than {required}"));
        }
    }

    fn items(&mut self, at: &str, old: &Value, new: &Value) {
        match (old.get("items"), new.get("items")) {
            (Some(old), Some(new)) => self.schema(&format!("{at}[]"), old, new),
            (None, None) => {}
            _ => {
                let (wide, narrow) = self.sides(old, new);
                if wide.get("items").is_some() && narrow.get("items").is_none() {
                    self.escapes(at, "items of any kind");
                }
            }
        }
    }

    fn properties(&mut self, at: &str, old: &Value, new: &Value) {
        for (name, old_property) in properties(old) {
            let path = if at.is_empty() {
                name.clone()
            } else {
                format!("{at}.{name}")
            };
            match new.get("properties").and_then(|p| p.get(name)) {
                Some(new_property) => self.schema(&path, old_property, new_property),
                None => self.push(at, &format!("`{name}` was removed")),
            }
        }
        let (wide, narrow) = self.sides(old, new);
        let narrow_required = required(narrow);
        for name in required(wide) {
            if !narrow_required.contains(&name) {
                self.escapes(at, &format!("an object without `{name}`"));
            }
        }
        // Others ignore properties they don't know, so only kipple's reading cares.
        let closed =
            |schema: &Value| schema.get("additionalProperties") == Some(&Value::Bool(false));
        if self.read_by == ReadBy::Kipple && closed(new) && !closed(old) {
            self.escapes(at, "properties the baseline doesn't list");
        }
    }

    /// The side that must fit allows `what`, which the other doesn't.
    fn escapes(&mut self, at: &str, what: &str) {
        let change = match self.read_by {
            ReadBy::Others => {
                format!("kipple may now write {what}, which the baseline doesn't allow")
            }
            ReadBy::Kipple => format!("kipple no longer reads {what}, which the baseline allows"),
        };
        self.push(at, &change);
    }

    fn unsupported(&mut self, at: &str, problem: &str) {
        self.push(
            at,
            &format!("{problem}, which the check can't compare (xtask/src/compat.rs)"),
        );
    }

    fn push(&mut self, at: &str, change: &str) {
        let at = if at.is_empty() { "(root)" } else { at };
        self.changes.push(format!("{at}: {change}"));
    }
}

/// `schema` with its `$ref` followed into `root`'s `$defs`, and the reference's name.
fn resolve<'v>(root: &'v Value, schema: &'v Value) -> Result<(&'v Value, Option<&'v str>), String> {
    let Some(reference) = schema.get("$ref") else {
        return Ok((schema, None));
    };
    if let Some(keyword) = keywords(schema).find(|k| *k != "$ref" && !ANNOTATIONS.contains(k)) {
        return Err(format!("`{keyword}` beside `$ref`"));
    }
    let name = reference
        .as_str()
        .and_then(|r| r.strip_prefix("#/$defs/"))
        .ok_or_else(|| format!("the reference {reference}"))?;
    let target = root
        .get("$defs")
        .and_then(|defs| defs.get(name))
        .ok_or_else(|| format!("the missing definition {name}"))?;
    if target.get("$ref").is_some() {
        return Err(format!("the definition {name}, itself a reference"));
    }
    Ok((target, Some(name)))
}

fn keywords(schema: &Value) -> impl Iterator<Item = &str> {
    schema
        .as_object()
        .into_iter()
        .flat_map(|map| map.keys().map(String::as_str))
}

/// Why the comparison can't judge `schema`, when it can't.
fn unsupported(schema: &Value) -> Option<String> {
    if !schema.is_object() {
        return Some(format!("the schema {schema}"));
    }
    if let Some(keyword) =
        keywords(schema).find(|k| !ANNOTATIONS.contains(k) && !CHECKED.contains(k))
    {
        return Some(format!("`{keyword}`"));
    }
    if schema.get("anyOf").is_some() && schema.get("oneOf").is_some() {
        return Some("both `anyOf` and `oneOf`".to_owned());
    }
    if schema
        .get("additionalProperties")
        .is_some_and(|v| !v.is_boolean())
    {
        return Some("`additionalProperties` that is a schema".to_owned());
    }
    let named =
        |v: &Value| v.is_string() || v.as_array().is_some_and(|l| l.iter().all(Value::is_string));
    if schema.get("type").is_some_and(|v| !named(v)) {
        return Some("a `type` that isn't a name or a list of names".to_owned());
    }
    if schema.get("items").is_some_and(|v| !v.is_object()) {
        return Some("`items` that isn't one schema".to_owned());
    }
    None
}

fn is_union(schema: &Value) -> bool {
    schema.get("anyOf").is_some() || schema.get("oneOf").is_some()
}

/// The variants of `schema`, each with the keywords beside its `anyOf` or `oneOf` merged
/// in, and nested unions flattened.
fn variants(root: &Value, schema: &Value) -> Result<Vec<Value>, String> {
    let Some(list) = schema.get("anyOf").or_else(|| schema.get("oneOf")) else {
        return Ok(vec![schema.clone()]);
    };
    let list = list.as_array().ok_or("a union that isn't a list")?;
    let mut base = schema.as_object().cloned().unwrap_or_default();
    base.retain(|keyword, _| !matches!(keyword.as_str(), "anyOf" | "oneOf"));
    let mut found = Vec::new();
    for variant in list {
        let (variant, _) = resolve(root, variant)?;
        let merged = merge(&base, variant)?;
        found.extend(variants(root, &merged)?);
    }
    Ok(found)
}

/// `variant` with the keywords of `base` that constrain, properties and required ones
/// joined.
fn merge(base: &Map<String, Value>, variant: &Value) -> Result<Value, String> {
    let mut merged = variant
        .as_object()
        .cloned()
        .ok_or("a variant that isn't a schema")?;
    for (keyword, value) in base {
        if ANNOTATIONS.contains(&keyword.as_str()) {
            continue;
        }
        let joined = match (keyword.as_str(), merged.get(keyword)) {
            (_, None) => value.clone(),
            (_, Some(own)) if own == value => continue,
            ("properties", Some(Value::Object(own))) => join_properties(own, value)?,
            ("required", Some(Value::Array(own))) => {
                let mut all = own.clone();
                all.extend(value.as_array().into_iter().flatten().cloned());
                Value::Array(all)
            }
            (keyword, Some(_)) => {
                return Err(format!("`{keyword}` both beside and inside a union"));
            }
        };
        merged.insert(keyword.clone(), joined);
    }
    Ok(Value::Object(merged))
}

/// The properties `own` lists and those `more` does, which may not differ on one.
fn join_properties(own: &Map<String, Value>, more: &Value) -> Result<Value, String> {
    let mut all = own.clone();
    for (name, property) in more.as_object().into_iter().flatten() {
        if all.get(name).is_some_and(|p| p != property) {
            return Err(format!("`{name}` both beside and inside a union"));
        }
        all.insert(name.clone(), property.clone());
    }
    Ok(Value::Object(all))
}

/// How a variant is told from the others: its constant properties, else its values, else
/// its types.
fn label(variant: &Value) -> String {
    let tags: Vec<String> = properties(variant)
        .filter_map(|(name, schema)| schema.get("const").map(|value| format!("{name}={value}")))
        .collect();
    if !tags.is_empty() {
        return tags.join(",");
    }
    if let Some(values) = values(variant) {
        let list: Vec<String> = values.iter().map(ToString::to_string).collect();
        return list.join("|");
    }
    types(variant).map_or_else(|| "of any type".to_owned(), |types| types.join("|"))
}

/// The types `schema` allows, or `None` for any.
fn types(schema: &Value) -> Option<Vec<&str>> {
    match schema.get("type")? {
        Value::String(kind) => Some(vec![kind.as_str()]),
        Value::Array(kinds) => Some(kinds.iter().filter_map(Value::as_str).collect()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => None,
    }
}

/// The values `schema` allows, or `None` for any.
fn values(schema: &Value) -> Option<Vec<&Value>> {
    if let Some(value) = schema.get("const") {
        return Some(vec![value]);
    }
    schema
        .get("enum")?
        .as_array()
        .map(|list| list.iter().collect())
}

fn properties(schema: &Value) -> impl Iterator<Item = (&String, &Value)> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
}

fn required(schema: &Value) -> BTreeSet<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A report in the shapes schemars writes: a `$ref` to a one-of-constants enum, a
    /// nullable count, and an object whose required list says what is always there.
    fn report() -> Value {
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "schema": { "type": "string" },
                "bucket": { "description": "The bucket.", "$ref": "#/$defs/Bucket" },
                "count": { "type": "integer", "format": "uint", "minimum": 0 }
            },
            "required": ["schema", "bucket", "count"],
            "$defs": {
                "Bucket": {
                    "oneOf": [
                        { "type": "string", "const": "safe" },
                        { "type": "string", "const": "review" },
                        { "type": "string", "const": "protected" }
                    ]
                }
            }
        })
    }

    /// A record stream in the shape of `kipple.event`: common properties beside a
    /// `oneOf` of variants told apart by a constant `type`.
    fn events() -> Value {
        json!({
            "type": "object",
            "properties": { "seq": { "type": "integer" } },
            "required": ["seq"],
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "type": { "type": "string", "const": "started" },
                        "integrations": { "type": "integer" }
                    },
                    "required": ["type", "integrations"]
                },
                {
                    "type": "object",
                    "properties": { "type": { "type": "string", "const": "completed" } },
                    "required": ["type"]
                }
            ]
        })
    }

    fn edited(mut schema: Value, edit: impl FnOnce(&mut Value)) -> Value {
        edit(&mut schema);
        schema
    }

    fn with_variant(variant: Value) -> Value {
        edited(events(), |s| {
            s["oneOf"].as_array_mut().unwrap().push(variant);
        })
    }

    #[test]
    fn an_added_optional_property_passes_for_every_reader() {
        let new = edited(report(), |s| {
            s["properties"]["note"] = json!({ "type": ["string", "null"] });
        });
        for read_by in [ReadBy::Others, ReadBy::Kipple] {
            assert_eq!(changes(&report(), &new, read_by), Vec::<String>::new());
        }
    }

    #[test]
    fn a_removed_or_renamed_property_fails_for_every_reader() {
        let new = edited(report(), |s| {
            let count = s["properties"].as_object_mut().unwrap().remove("count");
            s["properties"]["total"] = count.unwrap();
        });
        for read_by in [ReadBy::Others, ReadBy::Kipple] {
            let found = changes(&report(), &new, read_by);
            assert!(
                found.iter().any(|c| c == "(root): `count` was removed"),
                "{found:?}"
            );
        }
    }

    // Catches an old theme or config without a key the new version insists on.
    #[test]
    fn a_new_required_property_breaks_kipples_reading_only() {
        let new = edited(report(), |s| {
            s["properties"]["level"] = json!({ "type": "string" });
            s["required"].as_array_mut().unwrap().push(json!("level"));
        });

        let found = changes(&report(), &new, ReadBy::Kipple);

        assert_eq!(
            found,
            ["(root): kipple no longer reads an object without `level`, which the baseline allows"]
        );
        assert_eq!(
            changes(&report(), &new, ReadBy::Others),
            Vec::<String>::new()
        );
    }

    // Catches an output field turned optional: a script reading it on every record breaks.
    #[test]
    fn a_property_no_longer_always_written_breaks_others_reading() {
        let new = edited(report(), |s| {
            s["required"] = json!(["schema", "bucket"]);
            s["properties"]["count"]["type"] = json!(["integer", "null"]);
        });

        let found = changes(&report(), &new, ReadBy::Others);

        assert_eq!(
            found,
            [
                "count: kipple may now write the type null, which the baseline doesn't allow",
                "(root): kipple may now write an object without `count`, which the baseline \
                 doesn't allow",
            ]
        );
    }

    #[test]
    fn a_narrowed_enum_breaks_kipples_reading() {
        let new = edited(report(), |s| {
            s["$defs"]["Bucket"]["oneOf"].as_array_mut().unwrap().pop();
        });

        let found = changes(&report(), &new, ReadBy::Kipple);

        assert_eq!(
            found,
            [
                "bucket: kipple no longer reads the variant \"protected\", which the baseline \
                 allows"
            ]
        );
    }

    // Catches a new bucket that a script filtering on the three it knows would misfile.
    #[test]
    fn a_widened_enum_breaks_others_reading() {
        let new = edited(report(), |s| {
            let maybe = json!({ "type": "string", "const": "maybe" });
            s["$defs"]["Bucket"]["oneOf"]
                .as_array_mut()
                .unwrap()
                .push(maybe);
        });

        let found = changes(&report(), &new, ReadBy::Others);

        assert_eq!(
            found,
            [
                "bucket: kipple may now write the variant \"maybe\", which the baseline doesn't allow"
            ]
        );
        assert_eq!(
            changes(&report(), &new, ReadBy::Kipple),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_new_event_type_breaks_others_and_a_removed_one_breaks_kipple() {
        let paused = json!({
            "type": "object",
            "properties": { "type": { "type": "string", "const": "paused" } },
            "required": ["type"]
        });
        let more = with_variant(paused);

        let added = changes(&events(), &more, ReadBy::Others);
        let removed = changes(&more, &events(), ReadBy::Kipple);

        assert_eq!(added.len(), 1, "{added:?}");
        assert!(
            added[0].contains("the variant type=\"paused\""),
            "{added:?}"
        );
        assert_eq!(removed.len(), 1, "{removed:?}");
        assert!(removed[0].contains("no longer reads the variant type=\"paused\""));
    }

    // Catches a change deep in one variant passing because some variant still matches.
    #[test]
    fn a_change_inside_a_variant_is_reported_at_that_variant() {
        let new = edited(events(), |s| {
            s["oneOf"][0]["properties"]["integrations"]["type"] = json!("string");
        });

        let found = changes(&events(), &new, ReadBy::Others);

        assert_eq!(
            found,
            [
                "<type=\"started\">.integrations: kipple may now write the type string, which \
                 the baseline doesn't allow"
            ]
        );
    }

    // Catches a keyword the comparison ignores letting any change to it through.
    #[test]
    fn a_keyword_the_check_does_not_know_fails() {
        let new = edited(report(), |s| {
            s["properties"]["count"]["exclusiveMaximum"] = json!(10);
        });

        let found = changes(&report(), &new, ReadBy::Others);

        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("count: `exclusiveMaximum`, which the check can't compare"));
    }

    // Catches the check misreading kipple's real schemas: every change would fail, or
    // the event union's variants would never be paired.
    #[test]
    fn every_published_schema_is_compatible_with_itself_and_a_new_bucket_is_found() {
        for (name, schema) in kipple::json::schemas() {
            let format = FORMATS.iter().find(|f| f.name == name).unwrap();
            let schema = schema.as_value();
            assert_eq!(
                changes(schema, schema, format.read_by),
                Vec::<String>::new()
            );
        }
        let (_, scan) = kipple::json::schemas()
            .into_iter()
            .find(|(name, _)| *name == "scan")
            .unwrap();
        let fewer = edited(scan.as_value().clone(), |s| {
            s["$defs"]["BucketOut"]["oneOf"]
                .as_array_mut()
                .unwrap()
                .pop();
        });

        let found = changes(&fewer, scan.as_value(), ReadBy::Others);

        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .starts_with("findings[].verdict<object>.bucket: kipple may now write the variant"),
            "{found:?}"
        );
    }

    // Catches a schema published without a format entry escaping the check, and a format
    // dropped from the published list without anyone noticing.
    #[test]
    fn every_published_schema_has_exactly_one_format() {
        let published: Vec<&str> = FORMATS.iter().map(|f| f.name).collect();
        assert_eq!(unlisted(&published), Vec::<String>::new());

        let mut changed: Vec<&str> = published
            .iter()
            .copied()
            .filter(|n| *n != "theme")
            .collect();
        changed.push("journal");

        let found = unlisted(&changed);

        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with("journal: published but not in FORMATS"));
        assert!(found[1].starts_with("theme v1: no longer published"));
    }

    // Catches a bumped version passing with no baseline, and `--refresh` blessing a
    // change that breaks the baseline it replaces.
    #[test]
    fn refresh_adds_missing_baselines_but_never_blesses_a_break() {
        let root = tempfile::tempdir().unwrap();
        assert!(run(root.path(), false).is_err());

        run(root.path(), true).unwrap();
        run(root.path(), false).unwrap();

        let scan = root.path().join(BASELINES).join("scan/v1/schema.json");
        let mut baseline: Value =
            serde_json::from_str(&fs::read_to_string(&scan).unwrap()).unwrap();
        baseline["properties"]["gone"] = json!({ "type": "string" });
        let broken = gen_docs::pretty(&baseline).unwrap();
        fs::write(&scan, &broken).unwrap();

        assert!(run(root.path(), true).is_err());
        assert_eq!(fs::read_to_string(&scan).unwrap(), broken);
    }
}
