//! What the project integrations share: their `project-artifact` rules' candidates, each
//! with the markers beside it and what its tool wrote inside.

use std::path::Path;

use kipple_core::{
    DirEntryMeta, EntryKind, Evidence, FindingSink, IntegrationError, ScanContext, Selector,
};

/// How an integration recognises its tool's output inside a candidate, given its entries.
pub(super) type Signature = fn(&ScanContext<'_>, &Path, &[DirEntryMeta]) -> Option<String>;

/// Adds every candidate of the integration's `project-artifact` rules. A candidate whose
/// contents can't be listed gets no layout evidence either way.
pub(super) fn add_artifacts(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    signature: Signature,
) -> Result<(), IntegrationError> {
    let rules = cx
        .rules()
        .iter()
        .filter(|rule| matches!(rule.selector, Selector::ProjectArtifact { .. }));
    for rule in rules {
        for stop in cx.project_artifacts(rule)? {
            let mut evidence: Vec<Evidence> =
                stop.markers.into_iter().map(Evidence::Marker).collect();
            if let Ok(entries) = cx.read_dir(&stop.path) {
                let found = signature(cx, &stop.path, &entries);
                evidence.push(found.map_or(Evidence::NoSignature, Evidence::Signature));
            }
            sink.add(rule, &stop.path, evidence)?;
        }
    }
    Ok(())
}

/// The first of `names` that is a regular file among `entries`.
pub(super) fn file_inside(entries: &[DirEntryMeta], names: &[&str]) -> Option<String> {
    names
        .iter()
        .find(|name| {
            entries
                .iter()
                .any(|entry| entry.kind == EntryKind::File && entry.name == **name)
        })
        .map(|name| (*name).to_owned())
}
