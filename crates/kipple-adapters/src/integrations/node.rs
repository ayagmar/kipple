//! Node: project `node_modules` directories (`docs/06-adapters-and-rules.md` §5).
//! Verified against npm 11.19.0, which records the install in `.package-lock.json`, and
//! pnpm 12.9.1, which records it in `.modules.yaml`. Bun 1.4.2 writes no such file, so
//! its installs have no layout evidence.

use std::path::Path;

use kipple_core::{
    ActivityEvidence, Consequence, Cost, DirEntryMeta, FindingSink, Integration,
    IntegrationDescriptor, IntegrationError, IntegrationId, Lens, Loss, Platform, Recovery,
    ScanContext,
};

use super::project;

/// The package manager installs the dependencies again from the lockfile, downloading
/// whatever its own cache doesn't hold, so it may need the network.
const REINSTALLED: Consequence = Consequence {
    recovery: Recovery::Reinstall,
    loss: &[Loss::OfflineAbility],
    cost: Cost::Unknown,
};

/// The Node integration.
#[derive(Debug)]
pub(crate) struct Node;

impl Integration for Node {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("node"),
            lens: Lens::Dev,
            platforms: Platform::ALL,
            roots: &[],
            selectors: &[],
            // Packages inside hold their own `package.json` and `node_modules`.
            artifacts: &["node_modules"],
            owner_files: &[],
            verified: &["npm 11.19.0", "pnpm 12.9.1", "Bun 1.4.2"],
        }
    }

    fn activity_evidence(&self) -> ActivityEvidence {
        // `node`, `bun` or `deno` run the packages from outside `node_modules`.
        ActivityEvidence::Interpreted {
            runtimes: &["node", "bun", "deno"],
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        project::add_artifacts(cx, sink, install_record, REINSTALLED)
    }
}

fn install_record(_: &ScanContext<'_>, _: &Path, entries: &[DirEntryMeta]) -> Option<String> {
    project::file_inside(entries, &[".package-lock.json", ".modules.yaml"])
}
