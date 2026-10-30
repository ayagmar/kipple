//! How an item's outcome is stored and printed: in a journal's outcome records, a
//! receipt and the apply events, one shape for all three.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FormatError, IdentityOut, TextOut, path_out, rel_of, rel_out};
use crate::engine::FindingId;
use crate::frozen::NameCodec;
use crate::mutation::Unsupported;
use crate::outcome::{
    ApplyError, Effect, ErrorCode, Found, ItemOutcome, Left, LeftCount, LeftReason, OsError,
    OsErrorKind, Removed, SAMPLE, Skip, Status, Stop, Trashed,
};
use crate::plan::JudgementField;
use crate::planner::{Conflict, ConflictKind, Unjudged};
use crate::policy::{Caution, Hold};
use crate::protect::Protection;
use crate::rule::Method;
use crate::size::{Allocated, Apparent, Completeness, SpaceEstimate};
use crate::tracked::GitGap;
use crate::trash::{TrashDir, TrashSlot};

/// What went wrong: kipple's code and the OS error, never the OS's message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorOut {
    /// kipple's code.
    pub code: ErrorCode,
    /// The OS error's kind, if one caused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_kind: Option<OsErrorKind>,
    /// The OS's own number for it, where it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_code: Option<i32>,
    /// Where below the target, as components, if it is about one entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Vec<TextOut>>,
}

impl ErrorOut {
    pub(crate) fn of(error: &ApplyError, codec: &dyn NameCodec) -> Self {
        Self {
            code: error.code,
            os_kind: error.os.map(|os| os.kind),
            os_code: error.os.and_then(|os| os.code),
            at: error.at.as_ref().map(|at| rel_out(at, codec)),
        }
    }

    pub(crate) fn read(&self, codec: &dyn NameCodec) -> Result<ApplyError, FormatError> {
        let os = match (self.os_kind, self.os_code) {
            (Some(kind), code) => Some(OsError { kind, code }),
            (None, None) => None,
            (None, Some(_)) => return Err(invalid("OS error number without its kind")),
        };
        let at = match &self.at {
            Some(at) => Some(rel_of(at, codec).ok_or_else(|| invalid("error path"))?),
            None => None,
        };
        Ok(ApplyError {
            code: self.code,
            os,
            at,
            detail: None,
        })
    }
}

/// A trash slot: the trash directory and the item's name there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SlotOut {
    /// The trash directory's path.
    pub trash: TextOut,
    /// Its identity.
    pub identity: IdentityOut,
    /// The item's name in it.
    pub name: TextOut,
}

impl SlotOut {
    pub(crate) fn of(slot: &TrashSlot, codec: &dyn NameCodec) -> Self {
        Self {
            trash: path_out(&slot.trash.path, codec),
            identity: IdentityOut(slot.trash.identity),
            name: TextOut::of(slot.name.as_os_str(), codec),
        }
    }

    pub(crate) fn read(&self, codec: &dyn NameCodec) -> Result<TrashSlot, FormatError> {
        Ok(TrashSlot {
            trash: TrashDir {
                path: self
                    .trash
                    .path(codec)
                    .ok_or_else(|| invalid("trash path"))?,
                identity: self.identity.0,
            },
            name: self.name.name(codec).ok_or_else(|| invalid("trash name"))?,
        })
    }
}

/// What an item removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemovedOut {
    /// Names removed, confirmed.
    pub names: u64,
    /// Estimated allocated bytes removed, an upper bound.
    pub estimated_bytes: u64,
    /// Removed names whose file kept another name.
    pub shared: u64,
    /// Removed names whose allocation wasn't known.
    pub unmeasured: u64,
}

impl RemovedOut {
    pub(crate) const fn of(removed: &Removed) -> Self {
        Self {
            names: removed.names,
            estimated_bytes: removed.estimated.0,
            shared: removed.shared,
            unmeasured: removed.unmeasured,
        }
    }

    const fn read(self) -> Removed {
        Removed {
            names: self.names,
            estimated: Allocated(self.estimated_bytes),
            shared: self.shared,
            unmeasured: self.unmeasured,
        }
    }
}

/// Reviewed entries left for one reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LeftOut {
    /// Why.
    pub reason: LeftReason,
    /// How many.
    pub count: u64,
    /// At most ten of their paths below the target, as components, in canonical order.
    pub sample: Vec<Vec<TextOut>>,
}

fn left_out(left: &Left, codec: &dyn NameCodec) -> Vec<LeftOut> {
    left.0
        .iter()
        .map(|(reason, left)| LeftOut {
            reason: *reason,
            count: left.count,
            sample: left
                .sample
                .iter()
                .map(|path| rel_out(path, codec))
                .collect(),
        })
        .collect()
}

fn read_left(left: &[LeftOut], codec: &dyn NameCodec) -> Result<Left, FormatError> {
    let mut read = BTreeMap::new();
    for one in left {
        if one.sample.len() > SAMPLE || one.sample.len() as u64 > one.count {
            return Err(invalid("left sample"));
        }
        let sample = one
            .sample
            .iter()
            .map(|path| rel_of(path, codec))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| invalid("left path"))?;
        let count = LeftCount {
            count: one.count,
            sample,
        };
        if read.insert(one.reason, count).is_some() {
            return Err(invalid("left reason given twice"));
        }
    }
    Ok(Left(read))
}

/// What a run of an item did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectOut {
    /// What it removed.
    pub removed: RemovedOut,
    /// Where it moved the item, for a trash item that moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed: Option<SlotOut>,
    /// The reviewed entries it didn't remove, by reason.
    pub left: Vec<LeftOut>,
    /// Entries the plan left out from the start, kept as reviewed.
    pub excluded: u64,
}

impl EffectOut {
    pub(crate) fn of(effect: &Effect, codec: &dyn NameCodec) -> Self {
        Self {
            removed: RemovedOut::of(&effect.removed),
            trashed: effect
                .trashed
                .as_ref()
                .map(|trashed| SlotOut::of(&trashed.slot, codec)),
            left: left_out(&effect.left, codec),
            excluded: effect.excluded,
        }
    }

    fn read(&self, codec: &dyn NameCodec) -> Result<Effect, FormatError> {
        Ok(Effect {
            removed: self.removed.read(),
            trashed: match &self.trashed {
                Some(slot) => Some(Trashed {
                    slot: slot.read(codec)?,
                }),
                None => None,
            },
            left: read_left(&self.left, codec)?,
            excluded: self.excluded,
        })
    }
}

/// Why kipple doesn't change a file system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum UnsupportedOut {
    /// A network file system.
    Network,
    /// A user-space file system.
    UserSpace,
    /// One without POSIX delete semantics.
    NoPosixDelete,
    /// One kipple doesn't recognise.
    Unrecognised {
        /// What the OS calls it: its file system type name, letters, digits and `._+-`
        /// only, at most 64 of them.
        #[schemars(regex(pattern = r"^[A-Za-z0-9._+-]{0,64}$"))]
        name: String,
    },
}

/// What a file system type name is stored as: never free text.
fn fs_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        .take(64)
        .collect()
}

impl UnsupportedOut {
    fn of(why: &Unsupported) -> Self {
        match why {
            Unsupported::Network => Self::Network,
            Unsupported::UserSpace => Self::UserSpace,
            Unsupported::NoPosixDelete => Self::NoPosixDelete,
            Unsupported::Unrecognised(name) => Self::Unrecognised {
                name: fs_name(name),
            },
        }
    }

    fn read(&self) -> Result<Unsupported, FormatError> {
        Ok(match self {
            Self::Network => Unsupported::Network,
            Self::UserSpace => Unsupported::UserSpace,
            Self::NoPosixDelete => Unsupported::NoPosixDelete,
            Self::Unrecognised { name } if fs_name(name) == *name => {
                Unsupported::Unrecognised(name.clone())
            }
            Self::Unrecognised { .. } => return Err(invalid("file system name")),
        })
    }
}

/// Why an item was dropped before anything of it was touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "skip", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SkipOut {
    /// The authorization isn't for this plan.
    NotReviewed,
    /// Something it was judged under changed.
    Rejudged {
        /// What.
        field: JudgementField,
    },
    /// It is Protected now.
    Protected {
        /// Why.
        holds: Vec<Hold>,
    },
    /// It carries cautions it wasn't authorized with.
    Riskier {
        /// Which.
        cautions: Vec<Caution>,
    },
    /// Its rule doesn't offer its method any more.
    MethodNotOffered {
        /// The method.
        method: Method,
    },
    /// The fresh freeze failed.
    Freeze {
        /// Why.
        error: ErrorOut,
    },
    /// The executor or the trash refused it before touching anything.
    Refused {
        /// Why.
        error: ErrorOut,
    },
    /// It is on a file system kipple doesn't change.
    FileSystem {
        /// Why not.
        why: UnsupportedOut,
    },
    /// Revalidation dropped every reviewed entry.
    AllDropped {
        /// What it dropped, by reason.
        left: Vec<LeftOut>,
    },
    /// A trash move needs a whole-directory review automation doesn't have.
    WholeNotReviewed,
    /// A trash move needs every entry still authorized.
    NotWhole {
        /// What revalidation dropped, by reason.
        left: Vec<LeftOut>,
        /// How many entries the plan left out.
        excluded: u64,
    },
    /// The apply stopped before it started.
    NotStarted {
        /// Why.
        stop: Stop,
    },
    /// Its root isn't the directory the scan opened.
    RootChanged,
    /// It can't be judged now.
    Unjudged {
        /// Why.
        why: Unjudged,
    },
    /// The host protects it.
    HostProtected {
        /// What protects it.
        protection: Protection,
    },
    /// It holds another finding that is Protected or unmeasured.
    HoldsProtected {
        /// That finding.
        path: TextOut,
        /// What holds it; none when it wasn't measured.
        holds: Vec<Hold>,
    },
    /// Its rule is a third-party rule that isn't granted.
    NotGranted,
    /// It conflicts with another selected item.
    Conflict {
        /// The other item's finding, by its ID in the scan planned from.
        with: u64,
        /// How.
        kind: ConflictKind,
    },
    /// No trash can take it.
    TrashUnavailable {
        /// Why.
        error: ErrorOut,
    },
    /// git couldn't say in full what it tracks inside it.
    Git {
        /// Why.
        gap: GitGap,
    },
}

impl SkipOut {
    pub(crate) fn of(skip: &Skip, codec: &dyn NameCodec) -> Self {
        let error = |error: &ApplyError| ErrorOut::of(error, codec);
        match skip {
            Skip::NotReviewed => Self::NotReviewed,
            Skip::Rejudged(field) => Self::Rejudged { field: *field },
            Skip::Protected(holds) => Self::Protected {
                holds: holds.clone(),
            },
            Skip::Riskier(cautions) => Self::Riskier {
                cautions: cautions.clone(),
            },
            Skip::MethodNotOffered(method) => Self::MethodNotOffered { method: *method },
            Skip::Freeze(e) => Self::Freeze { error: error(e) },
            Skip::Refused(e) => Self::Refused { error: error(e) },
            Skip::FileSystem(why) => Self::FileSystem {
                why: UnsupportedOut::of(why),
            },
            Skip::AllDropped(left) => Self::AllDropped {
                left: left_out(left, codec),
            },
            Skip::WholeNotReviewed => Self::WholeNotReviewed,
            Skip::NotWhole { left, excluded } => Self::NotWhole {
                left: left_out(left, codec),
                excluded: *excluded,
            },
            Skip::NotStarted(stop) => Self::NotStarted { stop: *stop },
            Skip::RootChanged => Self::RootChanged,
            Skip::Unjudged(why) => Self::Unjudged { why: *why },
            Skip::HostProtected(protection) => Self::HostProtected {
                protection: *protection,
            },
            Skip::HoldsProtected { path, holds } => Self::HoldsProtected {
                path: path_out(path, codec),
                holds: holds.clone(),
            },
            Skip::NotGranted => Self::NotGranted,
            Skip::Conflict(conflict) => Self::Conflict {
                with: conflict.with.0,
                kind: conflict.kind,
            },
            Skip::TrashUnavailable(e) => Self::TrashUnavailable { error: error(e) },
            Skip::Git(gap) => Self::Git { gap: *gap },
        }
    }

    pub(crate) fn read(&self, codec: &dyn NameCodec) -> Result<Skip, FormatError> {
        let error = |error: &ErrorOut| error.read(codec);
        Ok(match self {
            Self::NotReviewed => Skip::NotReviewed,
            Self::Rejudged { field } => Skip::Rejudged(*field),
            Self::Protected { holds } => Skip::Protected(holds.clone()),
            Self::Riskier { cautions } => Skip::Riskier(cautions.clone()),
            Self::MethodNotOffered { method } => Skip::MethodNotOffered(*method),
            Self::Freeze { error: e } => Skip::Freeze(error(e)?),
            Self::Refused { error: e } => Skip::Refused(error(e)?),
            Self::FileSystem { why } => Skip::FileSystem(why.read()?),
            Self::AllDropped { left } => Skip::AllDropped(read_left(left, codec)?),
            Self::WholeNotReviewed => Skip::WholeNotReviewed,
            Self::NotWhole { left, excluded } => Skip::NotWhole {
                left: read_left(left, codec)?,
                excluded: *excluded,
            },
            Self::NotStarted { stop } => Skip::NotStarted(*stop),
            Self::RootChanged => Skip::RootChanged,
            Self::Unjudged { why } => Skip::Unjudged(*why),
            Self::HostProtected { protection } => Skip::HostProtected(*protection),
            Self::HoldsProtected { path, holds } => Skip::HoldsProtected {
                path: path.path(codec).ok_or_else(|| invalid("protected path"))?,
                holds: holds.clone(),
            },
            Self::NotGranted => Skip::NotGranted,
            Self::Conflict { with, kind } => Skip::Conflict(Conflict {
                with: FindingId(*with),
                kind: *kind,
            }),
            Self::TrashUnavailable { error: e } => Skip::TrashUnavailable(error(e)?),
            Self::Git { gap } => Skip::Git(*gap),
        })
    }
}

/// What reconciliation found of an item a crash interrupted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "found", rename_all = "kebab-case", deny_unknown_fields)]
pub enum FoundOut {
    /// Nothing authorized was removed.
    Unchanged,
    /// The target is gone.
    Gone,
    /// Another file is at the target.
    Replaced,
    /// Some of its entries are gone.
    PartlyGone {
        /// How many reviewed entries are gone.
        gone: u64,
        /// Of how many.
        of: u64,
    },
    /// It is in the trash, at its journalled slot.
    Trashed {
        /// Where.
        slot: SlotOut,
    },
    /// It couldn't be checked.
    Unreadable {
        /// Why.
        error: ErrorOut,
    },
    /// The run's change couldn't be made durable.
    Unsynced {
        /// Why.
        error: ErrorOut,
    },
    /// Not reconciled yet: running, or interrupted.
    Pending,
}

impl FoundOut {
    pub(crate) fn of(found: &Found, codec: &dyn NameCodec) -> Self {
        match found {
            Found::Unchanged => Self::Unchanged,
            Found::Gone => Self::Gone,
            Found::Replaced => Self::Replaced,
            Found::PartlyGone { gone, of } => Self::PartlyGone {
                gone: *gone,
                of: *of,
            },
            Found::Trashed(trashed) => Self::Trashed {
                slot: SlotOut::of(&trashed.slot, codec),
            },
            Found::Unreadable(error) => Self::Unreadable {
                error: ErrorOut::of(error, codec),
            },
            Found::Unsynced(error) => Self::Unsynced {
                error: ErrorOut::of(error, codec),
            },
            Found::Pending => Self::Pending,
        }
    }

    pub(crate) fn read(&self, codec: &dyn NameCodec) -> Result<Found, FormatError> {
        Ok(match self {
            Self::Unchanged => Found::Unchanged,
            Self::Gone => Found::Gone,
            Self::Replaced => Found::Replaced,
            Self::PartlyGone { gone, of } if gone <= of => Found::PartlyGone {
                gone: *gone,
                of: *of,
            },
            Self::PartlyGone { .. } => return Err(invalid("partly gone count")),
            Self::Trashed { slot } => Found::Trashed(Trashed {
                slot: slot.read(codec)?,
            }),
            Self::Unreadable { error } => Found::Unreadable(error.read(codec)?),
            Self::Unsynced { error } => Found::Unsynced(error.read(codec)?),
            Self::Pending => Found::Pending,
        })
    }
}

/// What happened to one item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum OutcomeOut {
    /// Every reviewed entry was removed, or the item moved to the trash.
    Done {
        /// What it did.
        effect: EffectOut,
    },
    /// It ran to the end but left reviewed entries.
    Partial {
        /// What it did.
        effect: EffectOut,
    },
    /// It was dropped before anything of it was touched.
    Skipped {
        /// Why.
        skip: SkipOut,
    },
    /// It stopped on an error; what it removed until then counts.
    Failed {
        /// The error.
        error: ErrorOut,
        /// What it did until then.
        effect: EffectOut,
    },
    /// The apply was cancelled during it.
    Cancelled {
        /// What it did until then.
        effect: EffectOut,
    },
    /// A crash interrupted it.
    Indeterminate {
        /// What reconciliation found.
        found: FoundOut,
    },
}

impl OutcomeOut {
    /// `outcome` as stored.
    #[must_use]
    pub fn of(outcome: &ItemOutcome, codec: &dyn NameCodec) -> Self {
        let effect = |effect: &Effect| EffectOut::of(effect, codec);
        match outcome {
            ItemOutcome::Done(e) => Self::Done { effect: effect(e) },
            ItemOutcome::Partial(e) => Self::Partial { effect: effect(e) },
            ItemOutcome::Skipped(skip) => Self::Skipped {
                skip: SkipOut::of(skip, codec),
            },
            ItemOutcome::Failed { error, effect: e } => Self::Failed {
                error: ErrorOut::of(error, codec),
                effect: effect(e),
            },
            ItemOutcome::Cancelled(e) => Self::Cancelled { effect: effect(e) },
            ItemOutcome::Indeterminate(found) => Self::Indeterminate {
                found: FoundOut::of(found, codec),
            },
        }
    }

    /// The outcome it stores.
    ///
    /// # Errors
    /// When a value in it isn't valid.
    pub fn read(&self, codec: &dyn NameCodec) -> Result<ItemOutcome, FormatError> {
        Ok(match self {
            Self::Done { effect } => ItemOutcome::Done(effect.read(codec)?),
            Self::Partial { effect } => ItemOutcome::Partial(effect.read(codec)?),
            Self::Skipped { skip } => ItemOutcome::Skipped(skip.read(codec)?),
            Self::Failed { error, effect } => ItemOutcome::Failed {
                error: error.read(codec)?,
                effect: effect.read(codec)?,
            },
            Self::Cancelled { effect } => ItemOutcome::Cancelled(effect.read(codec)?),
            Self::Indeterminate { found } => ItemOutcome::Indeterminate(found.read(codec)?),
        })
    }

    /// Its status.
    #[must_use]
    pub const fn status(&self) -> Status {
        match self {
            Self::Done { .. } => Status::Done,
            Self::Partial { .. } => Status::Partial,
            Self::Skipped { .. } => Status::Skipped,
            Self::Failed { .. } => Status::Failed,
            Self::Cancelled { .. } => Status::Cancelled,
            Self::Indeterminate { .. } => Status::Indeterminate,
        }
    }
}

/// How a finding's size was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CompletenessOut {
    /// Every entry was read.
    Complete,
    /// Some weren't.
    Incomplete {
        /// Entries that couldn't be read.
        unreadable: u64,
        /// Mounts inside that weren't entered.
        skipped_mounts: u64,
    },
    /// The measurement was cancelled.
    Cancelled,
}

/// What the scan estimated removing an item frees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EstimateOut {
    /// Total file length.
    pub apparent_bytes: u64,
    /// Allocated storage, where the platform reports it.
    pub allocated_bytes: Option<u64>,
    /// Allocated storage only this item holds.
    pub unique_bytes: Option<u64>,
    /// How much was measured.
    pub completeness: CompletenessOut,
}

impl EstimateOut {
    pub(crate) const fn of(estimate: &SpaceEstimate) -> Self {
        Self {
            apparent_bytes: estimate.apparent.0,
            allocated_bytes: match estimate.allocated {
                Some(bytes) => Some(bytes.0),
                None => None,
            },
            unique_bytes: match estimate.unique_reclaim {
                Some(bytes) => Some(bytes.0),
                None => None,
            },
            completeness: match estimate.completeness {
                Completeness::Complete => CompletenessOut::Complete,
                Completeness::Incomplete {
                    unreadable,
                    skipped_mounts,
                } => CompletenessOut::Incomplete {
                    unreadable,
                    skipped_mounts,
                },
                Completeness::Cancelled => CompletenessOut::Cancelled,
            },
        }
    }

    pub(crate) const fn read(&self) -> SpaceEstimate {
        SpaceEstimate {
            apparent: Apparent(self.apparent_bytes),
            allocated: match self.allocated_bytes {
                Some(bytes) => Some(Allocated(bytes)),
                None => None,
            },
            unique_reclaim: match self.unique_bytes {
                Some(bytes) => Some(Allocated(bytes)),
                None => None,
            },
            completeness: match self.completeness {
                CompletenessOut::Complete => Completeness::Complete,
                CompletenessOut::Incomplete {
                    unreadable,
                    skipped_mounts,
                } => Completeness::Incomplete {
                    unreadable,
                    skipped_mounts,
                },
                CompletenessOut::Cancelled => Completeness::Cancelled,
            },
        }
    }
}

/// A [`FormatError::Invalid`] of a stored outcome.
const fn invalid(what: &'static str) -> FormatError {
    FormatError::invalid(super::JOURNAL, what)
}
