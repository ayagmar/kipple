//! What an integration may do during a scan (`docs/05-architecture.md` §2).
//!
//! An integration gets read-only probes confined to the roots it declared, and reports
//! what it found through a [`FindingSink`]. It never sees anything that can mutate.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cancel::CancelToken;
use crate::engine::{Finding, FindingId};
use crate::event::{EventTx, ScanEvent, Stopped};
use crate::probe::{DirEntryMeta, EntryKind, EntryMeta, FsProbe, ProbeError};
use crate::root::{ResolvedRoot, SymbolicRoot};

/// A built-in integration's ID, such as `cargo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IntegrationId(pub &'static str);

impl fmt::Display for IntegrationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// What an integration declares about itself before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationDescriptor {
    /// Its ID.
    pub id: IntegrationId,
    /// The roots it reads. It may read nothing else.
    pub roots: Vec<SymbolicRoot>,
}

/// Why an integration stopped before it finished.
#[derive(Debug, Clone, thiserror::Error)]
pub enum IntegrationError {
    /// The scan was cancelled or its consumer went away. Not a failure of the integration.
    #[error("the scan stopped")]
    Stopped,
    /// A probe it depended on failed.
    #[error(transparent)]
    Probe(#[from] ProbeError),
}

/// A tool integration. It proposes; the core decides.
pub trait Integration: fmt::Debug + Send + Sync {
    /// Its ID and the roots it reads.
    fn descriptor(&self) -> IntegrationDescriptor;

    /// Finds candidates under its roots and adds each one to `sink`.
    ///
    /// # Errors
    /// [`IntegrationError::Stopped`] when `sink` refuses a finding, so the integration
    /// stops early, or the probe failure that kept it from finishing.
    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError>;
}

/// The read-only view an integration gets during a scan.
#[derive(Debug)]
pub struct ScanContext<'a> {
    pub(crate) roots: &'a [ResolvedRoot],
    pub(crate) fs: &'a dyn FsProbe,
    pub(crate) cancel: &'a CancelToken,
}

impl ScanContext<'_> {
    /// The integration's declared roots that exist and are directories.
    #[must_use]
    pub const fn roots(&self) -> &[ResolvedRoot] {
        self.roots
    }

    /// Whether the scan was cancelled. Long loops should check it.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// What `path` itself is, without following a link.
    ///
    /// # Errors
    /// When `path` is outside the integration's roots, or can't be read.
    pub fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        self.fs.metadata(self.confined(path)?)
    }

    /// The entries of the directory at `dir`, without following links.
    ///
    /// # Errors
    /// When `dir` is outside the integration's roots, or can't be read.
    pub fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        self.fs.read_dir(self.confined(dir)?)
    }

    /// `path`, if it lies inside one of the roots. A path with a `..` component is
    /// refused rather than normalised, so it can't climb out of a root. So is a path
    /// through a link below the root: the OS follows a link in the middle of a path even
    /// when the probe itself doesn't follow the last one.
    fn confined<'p>(&self, path: &'p Path) -> Result<&'p Path, ProbeError> {
        let plain = path.components().all(|c| c != Component::ParentDir);
        let root = self.roots.iter().find(|root| path.starts_with(&root.path));
        let (true, Some(root)) = (plain, root) else {
            return Err(ProbeError::OutsideRoots {
                path: path.to_owned(),
            });
        };
        let between = path.ancestors().skip(1).take_while(|dir| *dir != root.path);
        for dir in between {
            if self.fs.metadata(dir)?.kind == EntryKind::Symlink {
                return Err(ProbeError::ThroughLink {
                    link: dir.to_owned(),
                });
            }
        }
        Ok(path)
    }
}

/// Where an integration reports what it found. Every finding is streamed to the
/// frontend as it is added, waiting while the stream is full.
#[derive(Debug)]
pub struct FindingSink<'a> {
    pub(crate) integration: IntegrationId,
    pub(crate) cx: &'a ScanContext<'a>,
    pub(crate) events: &'a EventTx,
    pub(crate) next_id: &'a AtomicU64,
    pub(crate) found: Vec<Finding>,
}

impl FindingSink<'_> {
    /// Adds a finding at `path`, which must lie inside the integration's roots.
    ///
    /// # Errors
    /// [`IntegrationError::Stopped`] when the scan was cancelled or its consumer went
    /// away, and [`IntegrationError::Probe`] when `path` is outside the roots. The
    /// integration should return either.
    pub fn add(&mut self, path: PathBuf) -> Result<(), IntegrationError> {
        self.cx.confined(&path)?;
        let id = FindingId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let event = ScanEvent::FindingAdded {
            id,
            integration: self.integration,
            path: path.clone(),
        };
        self.events
            .send(event, self.cx.cancel)
            .map_err(|_: Stopped| IntegrationError::Stopped)?;
        self.found.push(Finding {
            id,
            integration: self.integration,
            path,
            size: None,
        });
        Ok(())
    }
}
