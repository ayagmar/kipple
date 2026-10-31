//! kipple's domain model, policy and engine.
//!
//! This crate depends on no other kipple crate and does no I/O (see
//! `docs/05-architecture.md`). The platform crate implements its ports, and the
//! binary wires them into an [`Engine`].
#![forbid(unsafe_code)]

/// Generates an enum of named values with `name`, `ALL` and `from_name`.
macro_rules! named {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $text:literal,)+ }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)+
        }

        impl $name {
            /// Every value, in a fixed order.
            pub const ALL: &[Self] = &[$(Self::$variant,)+];

            /// The name documents and reports use.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }

            /// The value called `name`, if there is one.
            #[must_use]
            pub fn from_name(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|value| value.name() == name)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.pad(self.name())
            }
        }

        /// Stored and shown as its name.
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.name())
            }
        }

        /// Read from its name; any other string is refused.
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let name = String::deserialize(deserializer)?;
                Self::from_name(&name).ok_or_else(|| {
                    serde::de::Error::custom(format_args!(
                        "{name:?} isn't a {}",
                        stringify!($name)
                    ))
                })
            }
        }

        /// A string, one of the names.
        impl schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                let names: Vec<&str> = Self::ALL.iter().map(|value| value.name()).collect();
                schemars::json_schema!({ "type": "string", "enum": names })
            }
        }
    };
}

mod activity;
mod apply;
mod authorize;
mod cancel;
mod capability;
mod digest;
mod discovery;
mod engine;
mod event;
mod facts;
pub mod format;
mod frozen;
mod imported;
mod integration;
mod journal;
mod mutation;
mod outcome;
mod plan;
mod planner;
mod policy;
mod probe;
mod process;
pub mod profile;
mod protect;
mod root;
mod rule;
mod run;
mod session;
mod size;
mod store;
mod suggest;
mod tracked;
mod trash;

pub use activity::{ProcessCensus, census};
pub use apply::{
    ApplyPorts, ApplyRequest, ItemEvent, ItemSink, Reconciled, Recorders, apply, reconcile,
};
pub use authorize::{Authorization, Authorized, AuthorizedOp, DeleteOp, ReviewedPlan, TrashOp};
pub use cancel::{CancelToken, Waited};
pub use capability::{NativeTool, ToolError, ToolStatus, ToolVersion};
pub use digest::{Digest, Sha256};
pub use discovery::WalkStop;
pub use engine::{
    Engine, EngineError, EngineRescan, FRESH, Finding, FindingId, IntegrationReport, Omission,
    OpenedRoot, Ports, Rescan, ScanOutcome, ScanReport, ScanRequest,
};
pub use event::{EventChannel, EventRx, EventTx, ScanEvent, Sequenced, event_channel};
pub use frozen::{
    BadName, Component, DecodeError, Drift, Entry, EntrySnapshot, Fingerprint, FrozenTree, Mark,
    Marks, NameCodec, NodeId, RelPath, Scope, ShapeError, Slot, Stamp, TreeBuilder, Unauthorized,
    Volume,
};
pub use imported::{Dropped, DroppedItem, ImportedPlan, ReplanRequest, Replanned, replan};
pub use integration::{
    ActivityEvidence, Advice, Evidence, FindingSink, Integration, IntegrationDescriptor,
    IntegrationError, IntegrationId, IntegrationSelector, ScanContext,
};
pub use journal::{Intent, Journal, Record, RestoreResult};
pub use mutation::{
    EffectSink, Executor, FileSystemClass, Finish, Freeze, Freezer, LocalFileSystem, Unsupported,
};
pub use outcome::{
    ApplyError, Effect, ErrorCode, Found, Freed, ItemOutcome, Left, LeftCount, LeftReason, OsError,
    OsErrorKind, Receipt, ReceiptItem, Removed, SAMPLE, Skip, Status, Stop, Tally, Totals, Trashed,
    TrashedTotal,
};
pub use plan::{
    Anchor, ItemId, Judgement, JudgementField, MethodChoice, MethodRefusal, Plan, PlanEntry,
    Selection, Standing, Total, VerifiedRoot,
};
pub use planner::{
    Conflict, ConflictKind, Judge, Merged, Methods, MutationPorts, PlanMode, PlanRequest, Planned,
    Refused, Unjudged, plan,
};
pub use policy::{
    Activity, ActivityGap, Bucket, Caution, Consequence, Cost, Facts, HeldPath, Hold, Level,
    Lineage, Loss, Modified, POLICY_VERSION, Policy, Proposal, Recovery, RuleSettings, Seen,
    Tracked, Verdict, WalkGap,
};
pub use probe::{
    Clock, DirAccess, DirEntryMeta, EntryKind, EntryMeta, FileIdentity, FileSystem, FsProbe,
    ProbeError, Sizer,
};
pub use process::{
    Capabilities, CommandLines, EnvPath, Inventory, MountNamespace, Observer, Probed, ProcessClass,
    ProcessOutcome, ProcessOwner, ProcessProbe, ProcessRecord, ProcessSnapshot, Unread, UserId,
};
pub use protect::{Protection, Protections, STATE_ROOTS};
pub use root::{
    LinkProblem, Resolution, ResolvedRoot, RootLink, RootSource, Roots, SettingProblem,
    SymbolicRoot, UnresolvedRoot,
};
pub use rule::{
    Bounds, Category, FactKind, FactSpec, GENERIC_FACTS, GENERIC_SELECTORS, Lens, Method, Name,
    OPERATIONS, Operation, Pack, PackId, Pattern, Platform, Predicate, Rule, RuleId, RuleText,
    Selector, Test,
};
pub use run::{
    Depth, ItemState, PendingRestore, RunRead, RunState, RunSummary, StoredItem, StoredPlan,
    StoredRun, Unreadable, history, read_run,
};
pub use session::{
    Answer, ApplyEvent, ApplyFile, CleanRequest, Confirm, Confirmed, Consent, Ending, EventSink,
    Finished, Prepared, RefusedItem, RestoreAsk, RestoreFinished, Restored, Restoring, Review,
    Session, prepare_clean, run_apply, run_clean, run_prepared, run_reconcile, run_restore,
};
pub use size::{Allocated, Apparent, Completeness, Measurement, SpaceEstimate};
pub use store::{
    HEADER_LIMIT, JOURNAL_LIMIT, LOCK_WAIT, LockState, Locked, RunId, RunStore, StoreRefusal,
};
pub use suggest::{Suggestion, is_within, suggest};
pub use tracked::{GitGap, TrackedQuery};
pub use trash::{Forgotten, RestoreRequest, SlotState, TrashBackend, TrashDir, TrashSlot};
