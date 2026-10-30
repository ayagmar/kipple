//! Rule packs, the one document format for built-in and third-party rules, and their
//! validator for schema v1 (`docs/06-adapters-and-rules.md` §3).
//!
//! Validation is strict: unknown keys anywhere are errors, every limit is checked, and
//! every error carries a stable code with the file, line and column it applies to.

mod doc;
mod grammar;
mod predicate;
mod rule;

use std::fmt;

use kipple_core::{IntegrationDescriptor, Pack, PackId, Platform};

use doc::{Checker, Table, Value};

/// The largest pack document, in bytes.
pub const MAX_PACK_BYTES: usize = 256 * 1024;

/// The most rules a pack may hold.
const MAX_RULES: usize = 64;

/// The longest pack text field (`publisher`, `description`, `license`, `source`).
const MAX_PACK_TEXT: usize = 200;

/// The prefix only packs shipped in the binary may use.
const BUILTIN_PREFIX: &str = "builtin.";

/// Where a pack comes from, which decides what it may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Shipped in the binary. May use integration selectors and the `builtin.` prefix.
    Builtin,
    /// Installed by the user. Generic selectors, facts and operations only.
    ThirdParty,
}

/// What a pack may reference: this engine's version and its integrations.
#[derive(Debug, Clone, Copy)]
pub struct Catalog<'engine> {
    /// The engine version a pack's `kipple` requirement is checked against.
    pub engine: &'engine semver::Version,
    /// The built-in integrations, with the selectors they publish.
    pub integrations: &'engine [IntegrationDescriptor],
}

/// A validation error's stable code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorCode {
    /// Invalid TOML or UTF-8.
    ParseError,
    /// A document over [`MAX_PACK_BYTES`].
    TooLarge,
    /// A `schema_version` other than 1.
    UnsupportedSchema,
    /// A key the schema doesn't define.
    UnknownKey,
    /// A required key is absent.
    MissingKey,
    /// A value of the wrong type, or a predicate form that doesn't match the fact.
    WrongType,
    /// A string over its limit, or containing control characters.
    StringTooLong,
    /// A pack or rule ID that breaks the ID grammar.
    InvalidId,
    /// The same rule ID twice.
    DuplicateId,
    /// A `version` that isn't a semantic version, or a `kipple` that isn't a version requirement.
    InvalidVersion,
    /// The `kipple` requirement excludes this engine.
    EngineIncompatible,
    /// A rule's platforms aren't a subset of the pack's.
    PlatformMismatch,
    /// More than 64 rules.
    TooManyRules,
    /// More than 32 predicates in a rule.
    TooManyPredicates,
    /// A pattern or literal name that breaks the grammar.
    InvalidPattern,
    /// A parameter or bound out of range, or `min > max`.
    InvalidRange,
    /// A root, selector, fact or operation this engine doesn't publish.
    UnknownCapability,
    /// Something a third-party pack may not use.
    NotPermitted,
}

impl ErrorCode {
    /// The code as documented, such as `unknown-key`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ParseError => "parse-error",
            Self::TooLarge => "too-large",
            Self::UnsupportedSchema => "unsupported-schema",
            Self::UnknownKey => "unknown-key",
            Self::MissingKey => "missing-key",
            Self::WrongType => "wrong-type",
            Self::StringTooLong => "string-too-long",
            Self::InvalidId => "invalid-id",
            Self::DuplicateId => "duplicate-id",
            Self::InvalidVersion => "invalid-version",
            Self::EngineIncompatible => "engine-incompatible",
            Self::PlatformMismatch => "platform-mismatch",
            Self::TooManyRules => "too-many-rules",
            Self::TooManyPredicates => "too-many-predicates",
            Self::InvalidPattern => "invalid-pattern",
            Self::InvalidRange => "invalid-range",
            Self::UnknownCapability => "unknown-capability",
            Self::NotPermitted => "not-permitted",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One thing wrong with a pack, and where.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{file}:{line}:{column}: {code}: {message}")]
pub struct PackError {
    /// Its stable code.
    pub code: ErrorCode,
    /// The document's name, as the caller gave it.
    pub file: String,
    /// The line, from 1.
    pub line: usize,
    /// The column in characters, from 1.
    pub column: usize,
    /// What is wrong, for people.
    pub message: String,
}

/// Validates the pack document `bytes`, named `file` in errors.
///
/// # Errors
/// Every problem found, sorted by position. A document that isn't UTF-8 TOML, or
/// declares another schema version, is not checked further.
pub fn validate(
    file: &str,
    bytes: &[u8],
    origin: Origin,
    catalog: Catalog<'_>,
) -> Result<Pack, Vec<PackError>> {
    if bytes.len() > MAX_PACK_BYTES {
        let message = format!("the document is over {MAX_PACK_BYTES} bytes");
        return Err(single(file, "", ErrorCode::TooLarge, 0, message));
    }
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let at = error.valid_up_to();
            let valid = String::from_utf8_lossy(bytes.get(..at).unwrap_or_default());
            let message = "the document is not UTF-8".to_owned();
            return Err(single(file, &valid, ErrorCode::ParseError, at, message));
        }
    };
    let mut checker = Checker::new(file, text);
    let pack = document(&mut checker, text, origin, catalog);
    match pack {
        Some(pack) if checker.is_clean() => Ok(pack),
        _ => Err(checker.into_errors()),
    }
}

/// The one error of a document that can't be read at all, at byte `at` of `text`.
fn single(file: &str, text: &str, code: ErrorCode, at: usize, message: String) -> Vec<PackError> {
    let mut checker = Checker::new(file, text);
    checker.error(code, at..at, message);
    checker.into_errors()
}

fn document(c: &mut Checker<'_>, text: &str, origin: Origin, catalog: Catalog<'_>) -> Option<Pack> {
    let root = match toml::de::DeTable::parse(text) {
        Ok(root) => root,
        Err(error) => {
            let at = error.span().unwrap_or(0..0);
            c.error(ErrorCode::ParseError, at, error.message().to_owned());
            return None;
        }
    };
    let whole = root.span();
    let root = root.get_ref();
    schema_version(c, root, whole.clone())?;
    c.known_keys(root, &["schema_version", "pack", "rule"]);
    let header = c.required(root, whole.clone(), "pack");
    let header = header.and_then(|value| pack_header(c, value, origin, catalog))?;
    let rules_value = c.required(root, whole, "rule")?;
    let items = c.array(rules_value)?;
    // Rules are checked whenever the pack's ID and platforms are known, so one pass
    // reports problems in the header and in the rules together.
    let cx = rule::RuleCx {
        pack: header.id.as_ref()?,
        platforms: header.platforms.as_deref()?,
        origin,
        catalog,
    };
    let rules = cx.rules(c, items, rules_value.span())?;
    Some(Pack {
        rules,
        ..header.pack?
    })
}

/// The `[pack]` table: the pack without rules when all of it is valid, and the parts
/// rules are checked against whenever those are.
#[derive(Debug)]
struct Header {
    id: Option<PackId>,
    platforms: Option<Vec<Platform>>,
    pack: Option<Pack>,
}

/// Checks `schema_version` first: a document for another schema isn't read further.
fn schema_version(
    c: &mut Checker<'_>,
    root: &Table<'_>,
    whole: std::ops::Range<usize>,
) -> Option<()> {
    let value = c.required(root, whole, "schema_version")?;
    let version = c.integer(value)?;
    if version != 1 {
        c.error(
            ErrorCode::UnsupportedSchema,
            value.span(),
            format!("schema_version {version} is not supported; this engine reads 1"),
        );
        return None;
    }
    Some(())
}

/// The `[pack]` table, as a [`Pack`] with no rules yet.
fn pack_header(
    c: &mut Checker<'_>,
    value: &Value<'_>,
    origin: Origin,
    catalog: Catalog<'_>,
) -> Option<Header> {
    let table = c.table(value)?;
    let at = value.span();
    c.known_keys(
        table,
        &[
            "id",
            "version",
            "publisher",
            "description",
            "license",
            "source",
            "kipple",
            "platforms",
        ],
    );
    let id = pack_id(c, table, at.clone(), origin);
    let version = semver_field(c, table, at.clone(), "version", |text| {
        semver::Version::parse(text).map(drop)
    });
    let text = |checker: &mut Checker<'_>, key| {
        checker
            .required(table, at.clone(), key)
            .and_then(|field| checker.string(field, MAX_PACK_TEXT))
            .map(str::to_owned)
    };
    let publisher = text(c, "publisher");
    let description = text(c, "description");
    let license = text(c, "license");
    let source = table.get("source").map_or(Some(None), |field| {
        c.string(field, MAX_PACK_TEXT).map(|s| Some(s.to_owned()))
    });
    let kipple = engine_requirement(c, table, at.clone(), catalog.engine);
    let platforms = c
        .required(table, at, "platforms")
        .and_then(|field| platforms(c, field));
    let pack = (|| {
        Some(Pack {
            id: id.clone()?,
            version: version?,
            publisher: publisher?,
            description: description?,
            license: license?,
            source: source?,
            kipple: kipple?,
            platforms: platforms.clone()?,
            rules: Vec::new(),
        })
    })();
    Some(Header {
        id,
        platforms,
        pack,
    })
}

fn pack_id(
    c: &mut Checker<'_>,
    table: &Table<'_>,
    at: std::ops::Range<usize>,
    origin: Origin,
) -> Option<PackId> {
    let value = c.required(table, at, "id")?;
    let id = c.string(value, usize::MAX)?;
    if !grammar::is_pack_id(id) {
        let message = format!(
            "pack ID {id:?} must be reverse-domain `[a-z0-9-]+(.[a-z0-9-]+)+`, at most 64 characters"
        );
        c.error(ErrorCode::InvalidId, value.span(), message);
        return None;
    }
    if origin == Origin::ThirdParty && id.starts_with(BUILTIN_PREFIX) {
        let message =
            format!("the `{BUILTIN_PREFIX}` prefix is reserved for packs shipped with kipple");
        c.error(ErrorCode::NotPermitted, value.span(), message);
        return None;
    }
    Some(PackId(id.to_owned()))
}

/// A semantic-version string field, checked by `parse`.
fn semver_field<E: fmt::Display>(
    c: &mut Checker<'_>,
    table: &Table<'_>,
    at: std::ops::Range<usize>,
    key: &str,
    parse: impl Fn(&str) -> Result<(), E>,
) -> Option<String> {
    let value = c.required(table, at, key)?;
    let text = c.string(value, MAX_PACK_TEXT)?;
    if let Err(error) = parse(text) {
        c.error(
            ErrorCode::InvalidVersion,
            value.span(),
            format!("`{key}`: {error}"),
        );
        return None;
    }
    Some(text.to_owned())
}

/// The `kipple` requirement, which must accept this engine.
fn engine_requirement(
    c: &mut Checker<'_>,
    table: &Table<'_>,
    at: std::ops::Range<usize>,
    engine: &semver::Version,
) -> Option<String> {
    let text = semver_field(c, table, at.clone(), "kipple", |text| {
        semver::VersionReq::parse(text).map(drop)
    })?;
    let accepts = semver::VersionReq::parse(&text).is_ok_and(|req| req.matches(engine));
    if !accepts {
        let span = table.get("kipple").map_or(at, Value::span);
        let message = format!("the pack needs kipple {text}, and this is kipple {engine}");
        c.error(ErrorCode::EngineIncompatible, span, message);
        return None;
    }
    Some(text)
}

/// A non-empty array of platform names.
fn platforms(c: &mut Checker<'_>, value: &Value<'_>) -> Option<Vec<Platform>> {
    let items = c.array(value)?;
    if items.is_empty() {
        c.error(
            ErrorCode::InvalidRange,
            value.span(),
            "name at least one platform",
        );
        return None;
    }
    let mut platforms = Vec::new();
    for item in items {
        let platform = c.one_of(
            item,
            Platform::from_name,
            "a platform: linux, macos or windows",
        )?;
        if !platforms.contains(&platform) {
            platforms.push(platform);
        }
    }
    Some(platforms)
}
