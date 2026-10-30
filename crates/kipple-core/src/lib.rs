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
mod frozen;
mod integration;
mod journal;
mod mutation;
mod outcome;
mod plan;
mod planner;
mod policy;
mod probe;
mod process;
mod protect;
mod root;
mod rule;
mod size;
mod suggest;
mod trash;

pub use activity::{ProcessCensus, census};
pub use apply::{ApplyRequest, Pending, Reconciled, apply, reconcile};
pub use authorize::{Authorization, Authorized, AuthorizedOp, DeleteOp, TrashOp};
pub use cancel::CancelToken;
pub use capability::{NativeTool, ToolError, ToolStatus, ToolVersion};
pub use digest::Digest;
pub use discovery::WalkStop;
pub use engine::{
    Engine, EngineError, Finding, FindingId, IntegrationReport, Omission, OpenedRoot, Ports,
    ScanOutcome, ScanReport, ScanRequest,
};
pub use event::{EventChannel, EventRx, EventTx, ScanEvent, Sequenced, event_channel};
pub use frozen::{
    BadName, Component, DecodeError, Drift, Entry, EntrySnapshot, Fingerprint, FrozenTree, Mark,
    Marks, NameCodec, NodeId, RelPath, Scope, ShapeError, Slot, Stamp, TreeBuilder, Unauthorized,
    Volume,
};
pub use integration::{
    ActivityEvidence, Advice, Evidence, FindingSink, Integration, IntegrationDescriptor,
    IntegrationError, IntegrationId, IntegrationSelector, ScanContext,
};
pub use journal::{Intent, Journal};
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
    Selection, Standing, VerifiedRoot,
};
pub use planner::{
    Conflict, ConflictKind, Judge, Merged, Methods, MutationPorts, PlanMode, PlanRequest, Planned,
    Refused, Unjudged, plan,
};
pub use policy::{
    Activity, ActivityGap, Bucket, Caution, Consequence, Cost, Facts, HeldPath, Hold, Level,
    Lineage, Loss, Modified, Policy, Proposal, Recovery, RuleSettings, Seen, Tracked, Verdict,
    WalkGap,
};
pub use probe::{
    Clock, DirAccess, DirEntryMeta, EntryKind, EntryMeta, FileIdentity, FileSystem, FsProbe,
    ProbeError, Sizer,
};
pub use process::{
    Capabilities, CommandLines, EnvPath, Inventory, MountNamespace, Observer, Probed, ProcessClass,
    ProcessOutcome, ProcessOwner, ProcessProbe, ProcessRecord, ProcessSnapshot, Unread, UserId,
};
pub use protect::{Protection, Protections};
pub use root::{
    LinkProblem, Resolution, ResolvedRoot, RootLink, RootSource, Roots, SettingProblem,
    SymbolicRoot, UnresolvedRoot,
};
pub use rule::{
    Bounds, Category, FactKind, FactSpec, GENERIC_FACTS, GENERIC_SELECTORS, Lens, Method, Name,
    OPERATIONS, Operation, Pack, PackId, Pattern, Platform, Predicate, Rule, RuleId, RuleText,
    Selector, Test,
};
pub use size::{Allocated, Apparent, Completeness, Measurement, SpaceEstimate};
pub use suggest::{Suggestion, is_within, suggest};
pub use trash::{RestoreRequest, SlotState, TrashBackend, TrashDir, TrashSlot};
