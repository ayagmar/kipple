//! What an integration may do during a scan (`docs/05-architecture.md` §2).
//!
//! An integration gets read-only probes confined to the roots it declared, and reports
//! what it found through a [`FindingSink`]. It never sees anything that can mutate.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cancel::CancelToken;
use crate::discovery::{Walk, WalkPlan, WalkStop, Walks};
use crate::engine::{Finding, FindingId, Found};
use crate::event::{EventTx, ScanEvent, Stopped};
use crate::policy::Consequence;
use crate::probe::{DirEntryMeta, EntryKind, EntryMeta, FsProbe, ProbeError};
use crate::root::{ResolvedRoot, SymbolicRoot};
use crate::rule::{Lens, Platform, Rule, Selector};

/// A built-in integration's ID, such as `cargo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IntegrationId(pub &'static str);

impl IntegrationId {
    /// The owner of third-party rules, which use only generic selectors and facts.
    pub const GENERIC: Self = Self("generic");
}

impl fmt::Display for IntegrationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.0)
    }
}

/// What an integration declares about itself before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationDescriptor {
    /// Its ID.
    pub id: IntegrationId,
    /// Which home-screen lens its findings belong to.
    pub lens: Lens,
    /// The operating systems it supports.
    pub platforms: &'static [Platform],
    /// Roots it reads besides its rules' roots. It may read nothing else.
    pub roots: &'static [SymbolicRoot],
    /// Selectors it publishes, which only its own first-party rules may use.
    pub selectors: &'static [IntegrationSelector],
    /// Directory names that are always artifacts of its tool: no walk descends into them.
    pub artifacts: &'static [&'static str],
    /// File names that make the directory holding them its tool's, such as a virtual
    /// environment's `pyvenv.cfg`: no walk looks for anything inside that directory.
    pub owner_files: &'static [&'static str],
    /// The upstream versions whose layouts it was verified against, with a fixture each.
    pub verified: &'static [&'static str],
}

/// A selector an integration publishes, with what it selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegrationSelector {
    /// Its name, unique among all selectors.
    pub name: &'static str,
    /// What it selects, for the generated catalog.
    pub meaning: &'static str,
}

/// Why an integration thinks a finding is what its rule says. Shown to people; it never
/// authorizes anything.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Evidence {
    /// A file beside it marks a project, such as `Cargo.toml`.
    Marker(String),
    /// An entry inside it is one its tool writes, such as `CACHEDIR.TAG`.
    Signature(String),
    /// None of the entries its tool writes is inside, so its layout is unknown.
    NoSignature,
    /// It is the root itself, such as the trash.
    Root(SymbolicRoot),
}

/// A native setting or command an integration suggests. Shown, never run by kipple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advice {
    /// A short title.
    pub title: String,
    /// What it does and what to watch out for.
    pub text: String,
    /// Exact commands or settings to copy.
    pub snippets: Vec<String>,
    /// Upstream documentation (HTTPS URLs).
    pub references: Vec<String>,
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
    pub(crate) rules: &'a [Rule],
    pub(crate) fs: &'a dyn FsProbe,
    pub(crate) cancel: &'a CancelToken,
    pub(crate) plan: &'a WalkPlan,
    pub(crate) walks: &'a Walks,
    /// The walks this integration used, whose gaps are its omissions.
    pub(crate) walked: RefCell<BTreeMap<PathBuf, Arc<Walk>>>,
}

impl ScanContext<'_> {
    /// The integration's declared roots and its rules' roots that exist and are
    /// directories.
    #[must_use]
    pub const fn roots(&self) -> &[ResolvedRoot] {
        self.roots
    }

    /// The paths of `root` that opened. [`SymbolicRoot::ProjectRoots`] may have several.
    pub fn roots_of(&self, root: SymbolicRoot) -> impl Iterator<Item = &ResolvedRoot> {
        self.roots
            .iter()
            .filter(move |resolved| resolved.root == root)
    }

    /// The integration's own rules that apply on this platform.
    #[must_use]
    pub const fn rules(&self) -> &[Rule] {
        self.rules
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
        self.fs.metadata(&self.confined(path)?)
    }

    /// The entries of the directory at `dir`, without following links.
    ///
    /// # Errors
    /// When `dir` is outside the integration's roots, or can't be read.
    pub fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        self.fs.read_dir(&self.confined(dir)?)
    }

    /// The first `limit` bytes of the regular file at `path`, without following a link.
    ///
    /// # Errors
    /// When `path` is outside the integration's roots, is not a regular file or can't be
    /// read.
    pub fn read_file(&self, path: &Path, limit: u64) -> Result<Vec<u8>, ProbeError> {
        self.fs.read_file(&self.confined(path)?, limit)
    }

    /// Where the shared walk of each path of `root` stopped: at every candidate artifact
    /// of any rule, and at every directory a registered tool always owns. Directories it
    /// could not list become the integration's omissions.
    ///
    /// # Errors
    /// [`IntegrationError::Stopped`] when the scan was cancelled during the walk.
    pub fn walk_stops(&self, root: SymbolicRoot) -> Result<Vec<WalkStop>, IntegrationError> {
        // A directory under two paths of the root, given twice or one inside the other,
        // is one stop, as near its root as it is anywhere.
        let mut stops: BTreeMap<PathBuf, WalkStop> = BTreeMap::new();
        for resolved in self.roots_of(root) {
            let walk = self
                .walks
                .get(self.plan, self.fs, &resolved.path, self.cancel);
            if walk.cancelled {
                return Err(IntegrationError::Stopped);
            }
            keep_nearest(&mut stops, &walk.stops);
            self.walked.borrow_mut().insert(resolved.path.clone(), walk);
        }
        Ok(stops.into_values().collect())
    }

    /// The candidates of a `project-artifact` rule under its root: directories named like
    /// its artifact, within its depth, with one of its markers beside them. Each one's
    /// markers are narrowed to the rule's. Empty for any other selector.
    ///
    /// # Errors
    /// [`IntegrationError::Stopped`] when the scan was cancelled during the walk.
    pub fn project_artifacts(&self, rule: &Rule) -> Result<Vec<WalkStop>, IntegrationError> {
        let Selector::ProjectArtifact {
            markers,
            artifact,
            max_depth,
        } = &rule.selector
        else {
            return Ok(Vec::new());
        };
        let candidates = self
            .walk_stops(rule.root)?
            .into_iter()
            .filter(|stop| stop.is_named(&artifact.0) && stop.depth <= *max_depth)
            .map(|stop| {
                let own = markers
                    .iter()
                    .filter(|marker| stop.markers.contains(&marker.0))
                    .map(|marker| marker.0.clone())
                    .collect();
                WalkStop {
                    markers: own,
                    ..stop
                }
            })
            .filter(|stop| !stop.markers.is_empty())
            .collect();
        Ok(candidates)
    }

    /// `path` without a trailing separator or `.` component, if it lies inside one of the
    /// roots. Those would make the OS follow a link in the last component. A path with a
    /// `..` component is refused rather than normalised, so it can't climb out of a root.
    /// So is a path through a link below the root, which the OS would follow too. Each
    /// directory below the root is checked from the top, so nothing beyond a link is
    /// touched.
    fn confined(&self, path: &Path) -> Result<PathBuf, ProbeError> {
        let outside = || ProbeError::OutsideRoots {
            path: path.to_owned(),
        };
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(outside());
        }
        let path: PathBuf = path.components().collect();
        let (root, below) = self
            .roots
            .iter()
            .find_map(|root| Some((root, path.strip_prefix(&root.path).ok()?)))
            .ok_or_else(outside)?;
        let mut dir = root.path.clone();
        let mut between = below.components();
        between.next_back();
        for part in between {
            dir.push(part);
            if self.fs.metadata(&dir)?.kind == EntryKind::Symlink {
                return Err(ProbeError::ThroughLink { link: dir });
            }
        }
        Ok(path)
    }
}

/// Adds `found` to `stops`, keeping each directory where it is nearest its root.
fn keep_nearest(stops: &mut BTreeMap<PathBuf, WalkStop>, found: &[WalkStop]) {
    for stop in found {
        let nearer = stops
            .get(&stop.path)
            .is_none_or(|seen| stop.depth < seen.depth);
        if nearer {
            stops.insert(stop.path.clone(), stop.clone());
        }
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
    pub(crate) found: Vec<Found>,
    pub(crate) advice: Vec<Advice>,
}

impl FindingSink<'_> {
    /// Adds a finding of `rule` at `path`, which must lie inside the integration's roots,
    /// with what removing it costs when the integration verified that. Report-only
    /// findings and unrecognised layouts have no consequence.
    ///
    /// # Errors
    /// [`IntegrationError::Stopped`] when the scan was cancelled or its consumer went
    /// away, and [`IntegrationError::Probe`] when `path` is outside the roots. The
    /// integration should return either.
    pub fn add(
        &mut self,
        rule: &Rule,
        path: &Path,
        evidence: Vec<Evidence>,
        consequence: Option<Consequence>,
    ) -> Result<(), IntegrationError> {
        let finding = Finding {
            id: FindingId(self.next_id.fetch_add(1, Ordering::Relaxed)),
            integration: self.integration,
            rule: rule.id.clone(),
            category: rule.category,
            path: self.cx.confined(path)?,
            evidence,
            size: None,
            facts: None,
        };
        let event = ScanEvent::FindingAdded {
            finding: finding.clone(),
        };
        self.events
            .send(event, self.cx.cancel)
            .map_err(|_: Stopped| IntegrationError::Stopped)?;
        self.found.push(Found {
            finding,
            consequence,
        });
        Ok(())
    }

    /// Adds advice, which the report shows with the integration.
    pub fn advise(&mut self, advice: Advice) {
        self.advice.push(advice);
    }
}
