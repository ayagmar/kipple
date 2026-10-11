//! Python: project `.venv` directories and `__pycache__` bytecode caches
//! (`docs/06-adapters-and-rules.md` §5). Verified against Python 3.14.8's `venv` and uv,
//! which both write `pyvenv.cfg`, and the interpreter's `__pycache__/<module>.cpython-314.pyc`.

use std::path::Path;

use kipple_core::{
    DirEntryMeta, EntryKind, Evidence, FindingSink, Integration, IntegrationDescriptor,
    IntegrationError, IntegrationId, IntegrationSelector, Lens, Platform, Rule, ScanContext,
    Selector,
};

use super::project;

const BYTECODE_CACHES: &str = "bytecode-caches";

/// The Python integration.
#[derive(Debug)]
pub(crate) struct Python;

impl Integration for Python {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("python"),
            lens: Lens::Dev,
            platforms: Platform::ALL,
            roots: &[],
            selectors: &[IntegrationSelector {
                name: BYTECODE_CACHES,
                meaning: "`__pycache__` directories that hold only compiled `.pyc` files",
            }],
            // A virtual environment holds installed packages, never a project, whatever
            // its directory is called.
            artifacts: &["__pycache__", ".venv"],
            owner_files: &["pyvenv.cfg"],
            verified: &["Python 3.14.8 venv", "uv 0.12.23"],
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        project::add_artifacts(cx, sink, |_, _, entries| {
            project::file_inside(entries, &["pyvenv.cfg"])
        })?;
        let rules = cx
            .rules()
            .iter()
            .filter(|rule| rule.selector == Selector::Integration(BYTECODE_CACHES));
        for rule in rules {
            add_bytecode_caches(cx, sink, rule)?;
        }
        Ok(())
    }
}

/// Adds every `__pycache__` the shared walk of `rule`'s root stopped at that holds only
/// bytecode, or can't be listed.
fn add_bytecode_caches(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    rule: &Rule,
) -> Result<(), IntegrationError> {
    let caches = cx.walk_stops(rule.root)?;
    for cache in caches.iter().filter(|stop| stop.is_named("__pycache__")) {
        if let Some(evidence) = bytecode_evidence(cx, &cache.path) {
            sink.add(rule, &cache.path, evidence)?;
        }
    }
    Ok(())
}

/// The evidence that the cache at `dir` holds only bytecode, or `None` when it holds
/// anything else. One that can't be listed gets no layout evidence, like a project
/// artifact, so it is reported without ever being eligible.
fn bytecode_evidence(cx: &ScanContext<'_>, dir: &Path) -> Option<Vec<Evidence>> {
    match cx.read_dir(dir) {
        Ok(entries) if only_bytecode(&entries) => {
            Some(vec![Evidence::Signature("*.pyc".to_owned())])
        }
        Ok(_) => None,
        Err(_) => Some(Vec::new()),
    }
}

/// Whether `entries` are compiled files and nothing else. Anything else may be someone's
/// data, so the directory isn't proposed.
fn only_bytecode(entries: &[DirEntryMeta]) -> bool {
    !entries.is_empty()
        && entries.iter().all(|entry| {
            entry.kind == EntryKind::File
                && Path::new(&entry.name)
                    .extension()
                    .is_some_and(|ext| ext == "pyc")
        })
}
