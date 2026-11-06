//! Cursor (`docs/06-adapters-and-rules.md` §5): its descriptor and its roots, all closed whole.
//! Nothing of its layout is verified, so it opens nothing, and a path that only protects costs
//! nothing when it is wrong.

use kipple_core::{
    FindingSink, Integration, IntegrationDescriptor, IntegrationError, IntegrationId,
    IntegrationSelector, Lens, Platform, ScanContext, StateDeclaration, SymbolicRoot, Unresolved,
};

const SELECTORS: &[IntegrationSelector] = &[IntegrationSelector {
    name: "cursor-app-data",
    meaning: "the application data directory, as a whole",
}];

const fn closed(root: SymbolicRoot) -> StateDeclaration {
    StateDeclaration {
        root,
        protected: &[],
        open: &[],
        delegates: &[],
        unresolved: Unresolved::HoldOwn,
    }
}

const STATE: &[StateDeclaration] = &[
    closed(SymbolicRoot::CursorConfig),
    closed(SymbolicRoot::CursorAppData),
    closed(SymbolicRoot::CursorCli),
    closed(SymbolicRoot::CursorServer),
];

/// The Cursor integration.
#[derive(Debug)]
pub(crate) struct Cursor;

impl Integration for Cursor {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("cursor"),
            lens: Lens::Agents,
            platforms: Platform::ALL,
            roots: &[],
            selectors: SELECTORS,
            artifacts: &[],
            owner_files: &[],
            verified: &[],
            consequences: &[],
            facts: &[],
            state: STATE,
            evidences: &[],
            revision: 1,
        }
    }

    fn discover(
        &self,
        _: &ScanContext<'_>,
        _: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        Ok(())
    }
}
