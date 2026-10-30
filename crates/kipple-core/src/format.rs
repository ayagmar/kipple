//! The formats kipple stores and writes about mutation (spec M3b pinned 6, 9, 18, 19).
//!
//! Each is versioned, its JSON Schema generated from these types (AGENTS.md 18), and its
//! decoder strict: an unknown field, value or shape is refused, never guessed.
//!
//! - `kipple.stored-plan/v1`: a run's plan, written once before its first intent. One JSON
//!   header line with every item's header, then each item's frozen tree in the
//!   `kipple.frozen-tree` v1 codec, so `history` reads the header alone.
//! - `kipple.journal/v1`: a run's records, one JSON line each, appended.
//! - `kipple.plan/v1`: a plan written by `plan -o`, which `apply` plans again.
//! - `kipple.receipt/v1`, `kipple.history/v1` and `kipple.apply-event/v1`: what the CLI
//!   prints.
//!
//! Records hold paths, identities, sizes, rule IDs, codes and OS error kinds and numbers,
//! never an OS message, stderr or a command line (04 §8a). Names and paths keep the OS's
//! exact form: text when it is valid Unicode, its bytes (Unix) or UTF-16 code units
//! (Windows) in base64 when it isn't.
//!
//! Every stored line and tree carries the digest of its own bytes as stored
//! ([`Digest::of_bytes`]). Integrity is checked with that, never by computing a value's
//! digest again: a digest kipple computed from how it prints a value
//! ([`Digest::of_debug`]) is stored as written and only ever compared.

mod events;
mod journal;
mod outcome;
mod plan_file;
mod receipt;
mod stored_plan;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::digest::Digest;
use crate::frozen::{Component, NameCodec, RelPath};
use crate::probe::FileIdentity;

pub use events::{ApplyEventOut, EventKindOut};
pub use journal::{decode_line, encode_line};
pub use outcome::OutcomeOut;
pub use plan_file::{decode_plan_file, encode_plan_file};
pub use receipt::{HistoryOut, ReceiptOut, RunOut};
pub use stored_plan::{decode_stored_plan, encode_stored_plan};

/// A run's stored plan.
pub const STORED_PLAN: &str = "kipple.stored-plan/v1";
/// Each line of a run's journal.
pub const JOURNAL: &str = "kipple.journal/v1";
/// A plan written by `plan -o`.
pub const PLAN: &str = "kipple.plan/v1";
/// What `clean` and `apply` print with `--json`.
pub const RECEIPT: &str = "kipple.receipt/v1";
/// What `history --json` prints.
pub const HISTORY: &str = "kipple.history/v1";
/// Each apply line of `--events jsonl`.
pub const APPLY_EVENT: &str = "kipple.apply-event/v1";

/// The most bytes one frozen tree may take in a plan file: about 3M entries with unique
/// names (D-063).
pub const TREE_LIMIT: usize = 256 * 1024 * 1024;

/// Why stored or written bytes aren't a document of a format.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    /// They aren't JSON of the format's shape: what serde said, about the structure only.
    #[error("not a {format} document: {why}")]
    Shape {
        /// The format.
        format: &'static str,
        /// Where and how the shape differs.
        why: String,
    },
    /// The schema field names another format or version.
    #[error("a {found} document, not {format}")]
    Schema {
        /// The format expected.
        format: &'static str,
        /// What the document says it is.
        found: String,
    },
    /// A line or tree doesn't match the digest stored with it: torn or corrupt.
    #[error("a {0} record doesn't match its digest")]
    Digest(&'static str),
    /// A value holds together as JSON but not as what it stands for: what.
    #[error("a {format} document holds an invalid {what}")]
    Invalid {
        /// The format.
        format: &'static str,
        /// What is invalid.
        what: &'static str,
    },
    /// A frozen tree in it doesn't decode.
    #[error("a frozen tree doesn't decode: {0}")]
    Tree(#[from] crate::frozen::DecodeError),
    /// It is larger than the format allows.
    #[error("a {0} document is too large")]
    TooLarge(&'static str),
}

impl FormatError {
    const fn invalid(format: &'static str, what: &'static str) -> Self {
        Self::Invalid { format, what }
    }
}

/// A name or a path, exactly: its text when it is valid Unicode, else the OS's own form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum TextOut {
    /// The text.
    Text(String),
    /// Not valid Unicode.
    Raw {
        /// Base64 of the bytes (Unix) or UTF-16LE code units (Windows).
        raw: String,
    },
}

impl TextOut {
    /// `text`, through `codec` when it isn't valid Unicode.
    #[must_use]
    pub fn of(text: &OsStr, codec: &dyn NameCodec) -> Self {
        text.to_str().map_or_else(
            || Self::Raw {
                raw: base64(&codec.encode(text)),
            },
            |text| Self::Text(text.to_owned()),
        )
    }

    /// The OS's form of it.
    fn bytes(&self, codec: &dyn NameCodec) -> Option<Vec<u8>> {
        match self {
            Self::Text(text) => Some(codec.encode(OsStr::new(text))),
            Self::Raw { raw } => unbase64(raw),
        }
    }

    /// The name it stores, read as the platform reads a stored name.
    pub(crate) fn name(&self, codec: &dyn NameCodec) -> Option<Component> {
        let name = codec.decode(&self.bytes(codec)?)?;
        Component::new(name).ok()
    }

    /// The absolute path it stores.
    pub(crate) fn path(&self, codec: &dyn NameCodec) -> Option<PathBuf> {
        codec
            .decode_path(&self.bytes(codec)?)
            .filter(|path| path.is_absolute())
    }
}

/// `path` as stored.
pub(crate) fn path_out(path: &Path, codec: &dyn NameCodec) -> TextOut {
    TextOut::of(path.as_os_str(), codec)
}

/// `path`'s components as stored.
pub(crate) fn rel_out(path: &RelPath, codec: &dyn NameCodec) -> Vec<TextOut> {
    path.components()
        .iter()
        .map(|name| TextOut::of(name.as_os_str(), codec))
        .collect()
}

/// The relative path `names` store.
pub(crate) fn rel_of(names: &[TextOut], codec: &dyn NameCodec) -> Option<RelPath> {
    names
        .iter()
        .map(|name| name.name(codec))
        .collect::<Option<_>>()
        .map(RelPath::new)
}

/// A digest as stored: 32 lowercase hexadecimal digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DigestOut(pub Digest);

impl Serialize for DigestOut {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DigestOut {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Digest::parse(&text)
            .map(Self)
            .ok_or_else(|| serde::de::Error::custom("a digest is 32 lowercase hex digits"))
    }
}

impl JsonSchema for DigestOut {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Digest".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "pattern": "^[0-9a-f]{32}$" })
    }
}

/// A file identity as stored: `<volume>:<file>`, in decimal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityOut(pub FileIdentity);

impl Serialize for IdentityOut {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("{}:{}", self.0.volume, self.0.file))
    }
}

impl<'de> Deserialize<'de> for IdentityOut {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        let identity = text
            .split_once(':')
            .filter(|(volume, file)| digits(volume) && digits(file))
            .and_then(|(volume, file)| {
                Some(FileIdentity {
                    volume: volume.parse().ok()?,
                    file: file.parse().ok()?,
                })
            });
        identity
            .map(Self)
            .ok_or_else(|| serde::de::Error::custom("an identity is <volume>:<file>"))
    }
}

impl JsonSchema for IdentityOut {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "FileIdentity".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "pattern": "^[0-9]+:[0-9]+$" })
    }
}

/// Standard base64 with padding.
#[must_use]
pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                let index = (n >> (18 - 6 * i)) & 63;
                out.push(char::from(
                    ALPHABET.get(index as usize).copied().unwrap_or(b'='),
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The bytes `text` holds in standard padded base64, written the one way [`base64`]
/// writes them.
fn unbase64(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|byte| **byte == b'=').count();
        if pad > 2 {
            return None;
        }
        let mut n = 0u32;
        for (i, byte) in chunk.iter().enumerate() {
            let value = if i >= 4 - pad {
                0
            } else {
                ALPHABET.iter().position(|letter| letter == byte)?
            };
            n |= u32::try_from(value).ok()? << (18 - 6 * i);
        }
        let decoded = n.to_be_bytes();
        out.extend(decoded.get(1..4 - pad)?);
    }
    (base64(&out) == text).then_some(out)
}

/// One published schema.
#[derive(Debug, Clone)]
pub struct FormatSchema {
    /// Its file name, without `.json`.
    pub name: &'static str,
    /// The format and version it describes, such as `kipple.journal/v1`.
    pub version: &'static str,
    /// The schema.
    pub schema: schemars::Schema,
}

/// Every published schema of these formats.
#[must_use]
pub fn schemas() -> Vec<FormatSchema> {
    let schema = |name, version, schema| FormatSchema {
        name,
        version,
        schema,
    };
    vec![
        schema(
            "apply-event",
            APPLY_EVENT,
            schemars::schema_for!(ApplyEventOut),
        ),
        schema("history", HISTORY, schemars::schema_for!(HistoryOut)),
        schema("journal", JOURNAL, schemars::schema_for!(journal::LineOut)),
        schema("plan", PLAN, schemars::schema_for!(plan_file::PlanFileOut)),
        schema("receipt", RECEIPT, schemars::schema_for!(ReceiptOut)),
        schema(
            "stored-plan",
            STORED_PLAN,
            schemars::schema_for!(stored_plan::HeaderOut),
        ),
    ]
}

/// `unsigned` as one JSON line that ends with the digest of its own bytes, then a
/// newline: `{…,"digest":"<32 hex>"}`. The digest covers every byte before
/// `,"digest":`.
fn signed(unsigned: &impl Serialize) -> Vec<u8> {
    // Serializing records of strings, numbers and enums can't fail.
    let mut line = serde_json::to_vec(unsigned).unwrap_or_default();
    line.pop();
    let digest = Digest::of_bytes(&line);
    line.extend_from_slice(format!(r#","digest":"{digest}"}}"#).as_bytes());
    line.push(b'\n');
    line
}

/// What a [`signed`] `line`, without its newline, holds, once its digest matches its
/// bytes. The digest is checked before anything is parsed.
fn verified<T: serde::de::DeserializeOwned>(
    line: &[u8],
    format: &'static str,
) -> Result<T, FormatError> {
    const SUFFIX: usize = r#","digest":""#.len() + 32 + r#""}"#.len();
    let split = line
        .len()
        .checked_sub(SUFFIX)
        .ok_or(FormatError::Digest(format))?;
    let (signed, suffix) = line.split_at(split);
    let digest = suffix
        .strip_prefix(br#","digest":""#)
        .and_then(|rest| rest.strip_suffix(br#""}"#))
        .and_then(|hex| std::str::from_utf8(hex).ok())
        .and_then(Digest::parse);
    if digest != Some(Digest::of_bytes(signed)) {
        return Err(FormatError::Digest(format));
    }
    let mut unsigned = signed.to_vec();
    unsigned.push(b'}');
    serde_json::from_slice(&unsigned).map_err(|error| shape(format, &error))
}

/// `error` worded for a [`FormatError::Shape`]: where and what, never the input's text.
fn shape(format: &'static str, error: &serde_json::Error) -> FormatError {
    FormatError::Shape {
        format,
        why: format!(
            "{:?} at line {} column {}",
            error.classify(),
            error.line(),
            error.column()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches a base64 decoder that reads a non-canonical form, which would give one
    /// stored name two encodings.
    #[test]
    fn base64_reads_back_only_what_it_writes() {
        for bytes in [&b""[..], b"k", b"ki", b"kip", b"\xff\x00\xfe\x80"] {
            assert_eq!(unbase64(&base64(bytes)).as_deref(), Some(bytes));
        }
        for bad in ["Zg", "Zh==", "Zg=a", "====", "Z!=="] {
            assert_eq!(unbase64(bad), None, "{bad}");
        }
    }
}
