//! Cargo: project `target/` directories (`docs/06-adapters-and-rules.md` §5). Verified
//! against Cargo 1.99.0, which writes a `CACHEDIR.TAG` into every target directory it
//! creates.

use std::path::Path;

use kipple_core::{
    FindingSink, Integration, IntegrationDescriptor, IntegrationError, IntegrationId, Lens,
    Platform, ScanContext,
};

use super::project;

/// The tag's required first line, from the Cache Directory Tagging Specification.
const CACHEDIR_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// The Cargo integration.
#[derive(Debug)]
pub(crate) struct Cargo;

impl Integration for Cargo {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("cargo"),
            lens: Lens::Dev,
            platforms: Platform::ALL,
            roots: &[],
            selectors: &[],
            artifacts: &[],
            verified: &["Cargo 1.99.0"],
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        project::add_artifacts(cx, sink, cachedir_tag)
    }
}

/// `CACHEDIR.TAG`, when it starts with the specification's signature.
fn cachedir_tag(cx: &ScanContext<'_>, dir: &Path) -> Option<String> {
    let tag = "CACHEDIR.TAG";
    let limit = CACHEDIR_SIGNATURE.len() as u64;
    let head = cx.read_file(&dir.join(tag), limit).ok()?;
    (head == CACHEDIR_SIGNATURE).then(|| tag.to_owned())
}
