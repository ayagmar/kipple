//! kipple's domain model, policy and engine.
//!
//! This crate depends on no other kipple crate and does no I/O (see
//! `docs/05-architecture.md`). The platform crate implements its ports, and the
//! binary wires them into an [`Engine`].
#![forbid(unsafe_code)]

mod cancel;
mod capability;
mod engine;
mod event;
mod integration;
mod probe;
mod process;
mod root;
mod size;

pub use cancel::CancelToken;
pub use capability::{NativeTool, ToolError, ToolStatus, ToolVersion};
pub use engine::{
    Engine, Finding, FindingId, IntegrationReport, Omission, ScanOutcome, ScanReport, ScanRequest,
};
pub use event::{EventRx, EventTx, ScanEvent, Sequenced, event_channel};
pub use integration::{
    FindingSink, Integration, IntegrationDescriptor, IntegrationError, IntegrationId, ScanContext,
};
pub use probe::{DirEntryMeta, EntryKind, EntryMeta, FileIdentity, FsProbe, ProbeError, Sizer};
pub use process::{
    Inventory, ProcessClass, ProcessOutcome, ProcessProbe, ProcessRecord, ProcessSnapshot,
};
pub use root::{ResolvedRoot, RootSource, Roots, SymbolicRoot, UnresolvedRoot};
pub use size::{Allocated, Apparent, Completeness, SpaceEstimate};
