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
mod cancel;
mod capability;
mod discovery;
mod engine;
mod event;
mod facts;
mod integration;
mod policy;
mod probe;
mod process;
mod root;
mod rule;
mod size;
mod suggest;

pub use activity::{ProcessCensus, census};
pub use cancel::CancelToken;
pub use capability::{NativeTool, ToolError, ToolStatus, ToolVersion};
pub use discovery::WalkStop;
pub use engine::{
    Engine, EngineError, Finding, FindingId, IntegrationReport, Omission, Ports, ScanOutcome,
    ScanReport, ScanRequest,
};
pub use event::{EventChannel, EventRx, EventTx, ScanEvent, Sequenced, event_channel};
pub use integration::{
    ActivityEvidence, Advice, Evidence, FindingSink, Integration, IntegrationDescriptor,
    IntegrationError, IntegrationId, IntegrationSelector, ScanContext,
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
    Capabilities, EnvPath, Inventory, MountNamespace, Observer, Probed, ProcessClass,
    ProcessOutcome, ProcessOwner, ProcessProbe, ProcessRecord, ProcessSnapshot, Unread, UserId,
};
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
