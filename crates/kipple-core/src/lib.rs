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

mod cancel;
mod capability;
mod discovery;
mod engine;
mod event;
mod integration;
mod probe;
mod process;
mod root;
mod rule;
mod size;

pub use cancel::CancelToken;
pub use capability::{NativeTool, ToolError, ToolStatus, ToolVersion};
pub use discovery::WalkStop;
pub use engine::{
    Engine, EngineError, Finding, FindingId, IntegrationReport, Omission, ScanOutcome, ScanReport,
    ScanRequest,
};
pub use event::{EventRx, EventTx, ScanEvent, Sequenced, event_channel};
pub use integration::{
    Advice, Evidence, FindingSink, Integration, IntegrationDescriptor, IntegrationError,
    IntegrationId, IntegrationSelector, ScanContext,
};
pub use probe::{DirEntryMeta, EntryKind, EntryMeta, FileIdentity, FsProbe, ProbeError, Sizer};
pub use process::{
    Inventory, ProcessClass, ProcessOutcome, ProcessProbe, ProcessRecord, ProcessSnapshot,
};
pub use root::{
    LinkProblem, ResolvedRoot, RootLink, RootSource, Roots, SettingProblem, SymbolicRoot,
    UnresolvedRoot,
};
pub use rule::{
    Bounds, Category, FactKind, FactSpec, GENERIC_FACTS, GENERIC_SELECTORS, Lens, Method, Name,
    OPERATIONS, Operation, Pack, PackId, Pattern, Platform, Predicate, Rule, RuleId, RuleText,
    Selector, Test,
};
pub use size::{Allocated, Apparent, Completeness, SpaceEstimate};
