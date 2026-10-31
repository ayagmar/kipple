//! Cargo: project `target/` directories (`docs/06-adapters-and-rules.md` §5). Verified
//! against Cargo 1.99.0, which writes a `CACHEDIR.TAG` into every target directory it
//! creates.

use std::path::Path;

use kipple_core::{
    ActivityEvidence, Consequence, Cost, DirEntryMeta, FindingSink, Integration,
    IntegrationDescriptor, IntegrationError, IntegrationId, Lens, Loss, Platform, Recovery,
    ScanContext,
};

use super::project;

/// The tag's required first line, from the Cache Directory Tagging Specification.
const CACHEDIR_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// A target directory comes back with the next build, from the sources and the
/// dependencies Cargo keeps in its own cache, without the network. Only the incremental
/// state is lost, so that build is slower.
const REBUILT: Consequence = Consequence {
    recovery: Recovery::RebuildLocal,
    loss: &[Loss::CacheWarmth],
    cost: Cost::Unknown,
};

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
            owner_files: &[],
            verified: &["Cargo 1.99.0"],
            consequences: &[REBUILT],
            revision: 1,
        }
    }

    fn activity_evidence(&self) -> ActivityEvidence {
        // What runs from a target directory is a program built into it.
        ActivityEvidence::BuiltExecutables
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        project::add_artifacts(cx, sink, cachedir_tag, REBUILT)
    }
}

/// `CACHEDIR.TAG`, when it starts with the specification's signature.
fn cachedir_tag(cx: &ScanContext<'_>, dir: &Path, _: &[DirEntryMeta]) -> Option<String> {
    let tag = "CACHEDIR.TAG";
    let limit = CACHEDIR_SIGNATURE.len() as u64;
    let head = cx.read_file(&dir.join(tag), limit).ok()?;
    (head == CACHEDIR_SIGNATURE).then(|| tag.to_owned())
}
