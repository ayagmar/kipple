//! `kipple.journal/v1`: one JSON line per record, each ending with the digest of its own
//! bytes.
//!
//! ```text
//! {"schema":"kipple.journal/v1","record":{"type":"intent",...},"digest":"<32 hex>"}\n
//! ```
//!
//! The digest is of every byte before `,"digest":`, so it is checked on the bytes as
//! stored, before anything is parsed. A line whose digest doesn't match isn't trusted.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::outcome::{ErrorOut, FoundOut, OutcomeOut, SlotOut};
use super::{DigestOut, FormatError, JOURNAL, signed, verified};
use crate::frozen::NameCodec;
use crate::journal::{Intent, Record, RestoreResult};
use crate::plan::ItemId;
use crate::rule::Method;

/// One journal line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LineOut {
    /// `kipple.journal/v1`.
    pub schema: String,
    /// What it records.
    pub record: RecordOut,
    /// The digest of every byte of the line before `,"digest":`.
    pub digest: DigestOut,
}

/// A line without its digest: what is written first, and what the digest covers.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Unsigned {
    schema: String,
    record: RecordOut,
}

/// One record of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum RecordOut {
    /// The run's plan was stored. Always the first line.
    Plan {
        /// The plan's digest as it was written.
        digest: DigestOut,
    },
    /// An item's run is about to change something.
    Intent {
        /// The stored plan's digest.
        plan: DigestOut,
        /// The item.
        item: u32,
        /// How it is removed.
        method: Method,
        /// Its stored tree's digest.
        reviewed: DigestOut,
        /// The digest of that tree with what revalidation left of it.
        authorized: DigestOut,
        /// How many entries the run may remove.
        entries: u64,
        /// For a trash item, where it goes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trash: Option<SlotOut>,
    },
    /// How an item ended.
    Outcome {
        /// The item.
        item: u32,
        /// How it ended.
        outcome: OutcomeOut,
    },
    /// A restore is about to move a trashed item back.
    RestoreIntent {
        /// The item.
        item: u32,
        /// Where it is in the trash.
        slot: SlotOut,
    },
    /// How a restore ended.
    RestoreOutcome {
        /// The item.
        item: u32,
        /// How it ended.
        result: RestoreOut,
    },
    /// Reconciliation removed the trash record of a move that never happened.
    TrashRecordRemoved {
        /// The item.
        item: u32,
        /// The slot the record named.
        slot: SlotOut,
    },
}

/// How a restore ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RestoreOut {
    /// The item is back.
    Restored,
    /// It was refused or failed.
    Failed {
        /// Why.
        error: ErrorOut,
    },
    /// A crash interrupted it.
    Indeterminate {
        /// What reconciliation found.
        found: FoundOut,
    },
}

impl RestoreOut {
    pub(crate) fn of(result: &RestoreResult, codec: &dyn NameCodec) -> Self {
        match result {
            RestoreResult::Restored => Self::Restored,
            RestoreResult::Failed(error) => Self::Failed {
                error: ErrorOut::of(error, codec),
            },
            RestoreResult::Indeterminate(found) => Self::Indeterminate {
                found: FoundOut::of(found, codec),
            },
        }
    }
}

impl RecordOut {
    fn of(record: &Record, codec: &dyn NameCodec) -> Self {
        match record {
            Record::Plan { digest } => Self::Plan {
                digest: DigestOut(*digest),
            },
            Record::Intent(intent) => Self::Intent {
                plan: DigestOut(intent.plan),
                item: intent.item.0,
                method: intent.method,
                reviewed: DigestOut(intent.reviewed),
                authorized: DigestOut(intent.authorized),
                entries: intent.entries,
                trash: intent.trash.as_ref().map(|slot| SlotOut::of(slot, codec)),
            },
            Record::Outcome { item, outcome } => Self::Outcome {
                item: item.0,
                outcome: OutcomeOut::of(outcome, codec),
            },
            Record::RestoreIntent { item, slot } => Self::RestoreIntent {
                item: item.0,
                slot: SlotOut::of(slot, codec),
            },
            Record::RestoreOutcome { item, result } => Self::RestoreOutcome {
                item: item.0,
                result: RestoreOut::of(result, codec),
            },
            Record::TrashRecordRemoved { item, slot } => Self::TrashRecordRemoved {
                item: item.0,
                slot: SlotOut::of(slot, codec),
            },
        }
    }

    fn read(&self, codec: &dyn NameCodec) -> Result<Record, FormatError> {
        Ok(match self {
            Self::Plan { digest } => Record::Plan { digest: digest.0 },
            Self::Intent {
                plan,
                item,
                method,
                reviewed,
                authorized,
                entries,
                trash,
            } => Record::Intent(Intent {
                plan: plan.0,
                item: ItemId(*item),
                method: *method,
                reviewed: reviewed.0,
                authorized: authorized.0,
                entries: *entries,
                trash: match trash {
                    Some(slot) => Some(slot.read(codec)?),
                    None => None,
                },
            }),
            Self::Outcome { item, outcome } => Record::Outcome {
                item: ItemId(*item),
                outcome: outcome.read(codec)?,
            },
            Self::RestoreIntent { item, slot } => Record::RestoreIntent {
                item: ItemId(*item),
                slot: slot.read(codec)?,
            },
            Self::RestoreOutcome { item, result } => Record::RestoreOutcome {
                item: ItemId(*item),
                result: match result {
                    RestoreOut::Restored => RestoreResult::Restored,
                    RestoreOut::Failed { error } => RestoreResult::Failed(error.read(codec)?),
                    RestoreOut::Indeterminate { found } => {
                        RestoreResult::Indeterminate(found.read(codec)?)
                    }
                },
            },
            Self::TrashRecordRemoved { item, slot } => Record::TrashRecordRemoved {
                item: ItemId(*item),
                slot: slot.read(codec)?,
            },
        })
    }
}

/// `record` as one journal line, with its digest and the newline that ends it.
#[must_use]
pub fn encode_line(record: &Record, codec: &dyn NameCodec) -> Vec<u8> {
    signed(&Unsigned {
        schema: JOURNAL.to_owned(),
        record: RecordOut::of(record, codec),
    })
}

/// The record one journal line stores, without its newline.
///
/// # Errors
/// When its digest doesn't match its bytes, which is how a torn or corrupt line shows,
/// or it isn't a valid `kipple.journal/v1` record.
pub fn decode_line(line: &[u8], codec: &dyn NameCodec) -> Result<Record, FormatError> {
    let parsed: Unsigned = verified(line, JOURNAL)?;
    if parsed.schema != JOURNAL {
        return Err(FormatError::Schema {
            format: JOURNAL,
            found: parsed.schema,
        });
    }
    parsed.record.read(codec)
}
