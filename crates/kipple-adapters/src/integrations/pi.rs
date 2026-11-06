//! The Pi coding agent (`docs/06-adapters-and-rules.md` §5): its descriptor and its closed
//! roots. It reads nothing yet, so it stays unsupported and its agent directory stays closed
//! to everyone.

use kipple_core::{
    FindingSink, Integration, IntegrationDescriptor, IntegrationError, IntegrationId,
    IntegrationSelector, Lens, Platform, ScanContext, StateDeclaration, SymbolicRoot, Unresolved,
};

const SELECTORS: &[IntegrationSelector] = &[
    IntegrationSelector {
        name: "pi-closed-logs",
        meaning: "the diagnostic logs a verified store names, each by its exact name",
    },
    IntegrationSelector {
        name: "pi-store-entries",
        meaning: "entries of the agent directory that are reported and never removed",
    },
];

/// `$PI_CODING_AGENT_DIR` may hold credentials anywhere, so one that can't be used holds
/// everything.
const STATE: &[StateDeclaration] = &[
    StateDeclaration {
        root: SymbolicRoot::PiAgent,
        protected: &[],
        open: &[],
        delegates: &[],
        unresolved: Unresolved::HoldAll,
    },
    StateDeclaration {
        root: SymbolicRoot::PiHome,
        protected: &[],
        open: &[],
        delegates: &[SymbolicRoot::PiAgent],
        unresolved: Unresolved::HoldOwn,
    },
    StateDeclaration {
        root: SymbolicRoot::PiSessionDir,
        protected: &[],
        open: &[],
        delegates: &[],
        unresolved: Unresolved::HoldOwn,
    },
];

/// The Pi integration.
#[derive(Debug)]
pub(crate) struct Pi;

impl Integration for Pi {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("pi"),
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
