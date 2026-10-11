//! Python: project `.venv` directories and `__pycache__` bytecode caches
//! (`docs/06-adapters-and-rules.md` §5). Verified against Python 3.14.8's `venv` and uv,
//! which both write `pyvenv.cfg`, and the interpreter's `__pycache__/<module>.cpython-314.pyc`.

use std::path::Path;

use kipple_core::{
    EntryKind, Evidence, FindingSink, Integration, IntegrationDescriptor, IntegrationError,
    IntegrationId, IntegrationSelector, Lens, Platform, ScanContext, Selector,
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
            // A virtual environment holds installed packages, never a project.
            artifacts: &["__pycache__", ".venv"],
            verified: &["Python 3.14.8 venv", "uv 0.12.23"],
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        project::add_artifacts(cx, sink, |cx, dir| {
            project::file_inside(cx, dir, &["pyvenv.cfg"])
        })?;
        let rules = cx
            .rules()
            .iter()
            .filter(|rule| rule.selector == Selector::Integration(BYTECODE_CACHES));
        for rule in rules {
            let caches = cx.walk_stops(rule.root)?.into_iter();
            let bytecode =
                caches.filter(|stop| stop.is_named("__pycache__") && only_bytecode(cx, &stop.path));
            for stop in bytecode {
                let evidence = vec![Evidence::Signature("*.pyc".to_owned())];
                sink.add(rule, &stop.path, evidence)?;
            }
        }
        Ok(())
    }
}

/// Whether `dir` holds compiled files and nothing else. Anything else in it may be
/// someone's data, so the directory isn't proposed.
fn only_bytecode(cx: &ScanContext<'_>, dir: &Path) -> bool {
    cx.read_dir(dir).is_ok_and(|entries| {
        !entries.is_empty()
            && entries.iter().all(|entry| {
                entry.kind == EntryKind::File
                    && Path::new(&entry.name)
                        .extension()
                        .is_some_and(|ext| ext == "pyc")
            })
    })
}
