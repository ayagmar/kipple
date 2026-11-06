//! Codex (`docs/06-adapters-and-rules.md` §4): its descriptor and its closed roots. It reads
//! nothing yet, so it stays unsupported and its home stays closed to everyone.

use kipple_core::{
    FindingSink, Integration, IntegrationDescriptor, IntegrationError, IntegrationId,
    IntegrationSelector, Lens, Platform, ScanContext, StateDeclaration, SymbolicRoot, Unresolved,
};

const SELECTORS: &[IntegrationSelector] = &[
    IntegrationSelector {
        name: "verified-release-directories",
        meaning: "inactive release directories of a verified store",
    },
    IntegrationSelector {
        name: "codex-release-entries",
        meaning: "everything in the release store that is reported and never removed",
    },
    IntegrationSelector {
        name: "codex-legacy-tui-log",
        meaning: "the TUI log in the default log directory",
    },
    IntegrationSelector {
        name: "codex-plugin-catalog-files",
        meaning: "cached remote plugin catalog files",
    },
    IntegrationSelector {
        name: "codex-store-entries",
        meaning: "entries of the home that are reported and never removed",
    },
];

/// `$CODEX_HOME` may hold credentials anywhere, so one that can't be used holds everything.
const STATE: &[StateDeclaration] = &[
    StateDeclaration {
        root: SymbolicRoot::CodexHome,
        protected: &[],
        open: &[],
        delegates: &[],
        unresolved: Unresolved::HoldAll,
    },
    StateDeclaration {
        root: SymbolicRoot::CodexDefaultHome,
        protected: &[],
        open: &[],
        delegates: &[SymbolicRoot::CodexHome],
        unresolved: Unresolved::HoldOwn,
    },
    StateDeclaration {
        root: SymbolicRoot::CodexSqliteHome,
        protected: &[],
        open: &[],
        delegates: &[],
        unresolved: Unresolved::HoldOwn,
    },
];

/// The Codex integration.
#[derive(Debug)]
pub(crate) struct Codex;

impl Integration for Codex {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("codex"),
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
