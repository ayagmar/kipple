//! npm the tool, apart from Node's project artifacts (`docs/06-adapters-and-rules.md` §5): its
//! descriptor and its closed roots. It reads nothing yet, so it stays unsupported and its
//! cache stays closed to everyone.

use kipple_core::{
    FindingSink, Integration, IntegrationDescriptor, IntegrationError, IntegrationId,
    IntegrationSelector, Lens, Platform, ScanContext, StateDeclaration, SymbolicRoot, Unresolved,
};

const SELECTORS: &[IntegrationSelector] = &[
    IntegrationSelector {
        name: "npm-download-cache",
        meaning: "the `_cacache` directory of a verified cache root",
    },
    IntegrationSelector {
        name: "npm-cache-entries",
        meaning: "entries of the cache root that are reported and never removed",
    },
];

const STATE: &[StateDeclaration] = &[
    StateDeclaration {
        root: SymbolicRoot::NpmCache,
        protected: &[],
        open: &[],
        delegates: &[],
        unresolved: Unresolved::HoldOwn,
    },
    StateDeclaration {
        root: SymbolicRoot::NpmDefaultCache,
        protected: &[],
        open: &[],
        delegates: &[SymbolicRoot::NpmCache],
        unresolved: Unresolved::HoldOwn,
    },
];

/// The npm integration.
#[derive(Debug)]
pub(crate) struct Npm;

impl Integration for Npm {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("npm"),
            lens: Lens::Dev,
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
