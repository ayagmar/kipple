//! Node: project `node_modules` directories (`docs/06-adapters-and-rules.md` §5).
//! Verified against npm 11.19.0, which records the install in `.package-lock.json`, and
//! pnpm 12.9.1, which records it in `.modules.yaml`. Bun 1.4.2 writes no such file, so
//! its installs have no layout evidence.

use std::path::Path;

use kipple_core::{
    FindingSink, Integration, IntegrationDescriptor, IntegrationError, IntegrationId, Lens,
    Platform, ScanContext,
};

use super::project;

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
            verified: &["npm 11.19.0", "pnpm 12.9.1", "Bun 1.4.2"],
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        project::add_artifacts(cx, sink, install_record)
    }
}

fn install_record(cx: &ScanContext<'_>, dir: &Path) -> Option<String> {
    project::file_inside(cx, dir, &[".package-lock.json", ".modules.yaml"])
}
