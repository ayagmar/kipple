//! The shared walk of a root for `project-artifact` rules (`docs/05-architecture.md` §3).
//!
//! Each root is walked once per scan, however many integrations ask for it. The walk never
//! descends into a candidate artifact of any rule, nor into a directory a registered tool
//! owns (by its name, or by a file in it such as `pyvenv.cfg`), so nested projects inside
//! `node_modules`, `target` or a virtual environment are never looked for.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::cancel::CancelToken;
use crate::integration::IntegrationDescriptor;
use crate::policy::HeldPath;
use crate::probe::{DirEntryMeta, EntryKind, FsProbe, ProbeError};
use crate::rule::{Rule, Selector};

/// A directory the walk stopped at instead of descending into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkStop {
    /// Where it is.
    pub path: PathBuf,
    /// How many directories below the root it is: 1 for a child of the root.
    pub depth: u32,
    /// The project markers among the regular files beside it.
    pub markers: BTreeSet<String>,
}

impl WalkStop {
    /// Whether it is a directory called `name`.
    #[must_use]
    pub fn is_named(&self, name: &str) -> bool {
        self.path.file_name() == Some(OsStr::new(name))
    }
}

/// What a walk of one root found.
#[derive(Debug, Default)]
pub(crate) struct Walk {
    pub(crate) stops: Vec<WalkStop>,
    /// Directories below the root that could not be listed, so were not covered.
    pub(crate) unreadable: Vec<ProbeError>,
    /// Directories `exclude.paths` names, which the walk didn't enter.
    pub(crate) excluded: Vec<PathBuf>,
    /// Set when the scan was cancelled before the walk finished.
    pub(crate) cancelled: bool,
}

/// What every walk of one engine stops at and how deep it goes, from all of its rules and
/// integrations.
#[derive(Debug, Default)]
pub(crate) struct WalkPlan {
    /// Directory names a registered tool always owns.
    always: BTreeSet<String>,
    /// File names that make the directory holding them a registered tool's.
    owner_files: BTreeSet<String>,
    /// Each artifact name, with every marker that makes it a candidate.
    artifacts: BTreeMap<String, BTreeSet<String>>,
    /// The deepest `max_depth` of any rule.
    max_depth: u32,
}

impl WalkPlan {
    pub(crate) fn new<'a>(
        descriptors: impl IntoIterator<Item = &'a IntegrationDescriptor>,
        rules: impl IntoIterator<Item = &'a Rule>,
    ) -> Self {
        let mut plan = Self::default();
        for descriptor in descriptors {
            let names = descriptor.artifacts.iter().map(|&name| name.to_owned());
            plan.always.extend(names);
            let files = descriptor.owner_files.iter().map(|&name| name.to_owned());
            plan.owner_files.extend(files);
        }
        for rule in rules {
            if let Selector::ProjectArtifact {
                markers,
                artifact,
                max_depth,
            } = &rule.selector
            {
                let entry = plan.artifacts.entry(artifact.0.clone()).or_default();
                entry.extend(markers.iter().map(|marker| marker.0.clone()));
                plan.max_depth = plan.max_depth.max(*max_depth);
            }
        }
        plan
    }

    /// Whether the walk stops at the directory `name`, whose parent holds `files`.
    fn stops_at(&self, name: &str, files: &BTreeSet<String>) -> bool {
        self.always.contains(name)
            || self
                .artifacts
                .get(name)
                .is_some_and(|markers| !markers.is_disjoint(files))
    }

    /// The project markers among `files`.
    fn markers(&self, files: &BTreeSet<String>) -> BTreeSet<String> {
        self.artifacts
            .values()
            .flat_map(|markers| markers.intersection(files))
            .cloned()
            .collect()
    }

    fn walk(
        &self,
        fs: &dyn FsProbe,
        root: &Path,
        exclude: &[HeldPath],
        cancel: &CancelToken,
    ) -> Walk {
        let exclude = Exclusions { fs, paths: exclude };
        let mut walk = Walk::default();
        if exclude.covers(root) {
            walk.excluded.push(root.to_owned());
            return walk;
        }
        let mut pending = vec![(root.to_owned(), 0)];
        while let Some(at) = pending.pop() {
            if cancel.is_cancelled() {
                walk.cancelled = true;
                break;
            }
            match fs.read_dir(&at.0) {
                Ok(entries) => {
                    let below = self.visit(&exclude, &at, &entries, &mut walk);
                    pending.extend(below.into_iter().map(|path| (path, at.1 + 1)));
                }
                Err(error) => walk.unreadable.push(error),
            }
        }
        walk.stops.sort_by(|a, b| a.path.cmp(&b.path));
        walk.unreadable.sort_by(|a, b| a.path().cmp(b.path()));
        walk.excluded.sort();
        walk
    }

    /// Records where the walk stops among the `entries` of `dir`, `depth` directories
    /// below the root, and returns the subdirectories to descend into. An excluded
    /// directory is neither.
    fn visit(
        &self,
        exclude: &Exclusions<'_>,
        (dir, depth): &(PathBuf, u32),
        entries: &[DirEntryMeta],
        walk: &mut Walk,
    ) -> Vec<PathBuf> {
        let depth = *depth;
        let files: BTreeSet<String> = entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::File)
            .filter_map(|entry| entry.name.to_str().map(str::to_owned))
            .collect();
        // A directory a tool owns holds no projects and no artifacts of anyone else.
        if !self.owner_files.is_disjoint(&files) {
            return Vec::new();
        }
        let mut below = Vec::new();
        for entry in entries.iter().filter(|entry| entry.kind == EntryKind::Dir) {
            let path = dir.join(&entry.name);
            // A directory whose name isn't UTF-8 can't be an artifact, but may hold one.
            let stop = entry
                .name
                .to_str()
                .is_some_and(|name| self.stops_at(name, &files));
            if !stop && depth + 1 >= self.max_depth {
                continue;
            }
            if exclude.covers(&path) {
                walk.excluded.push(path);
            } else if stop {
                let markers = self.markers(&files);
                walk.stops.push(WalkStop {
                    path,
                    depth: depth + 1,
                    markers,
                });
            } else {
                below.push(path);
            }
        }
        below
    }
}

/// The paths `exclude.paths` names, as the walk checks them.
struct Exclusions<'scan> {
    fs: &'scan dyn FsProbe,
    paths: &'scan [HeldPath],
}

impl Exclusions<'_> {
    /// Whether the directory at `path` is excluded: an excluded path or inside one, by
    /// its path or by its identity.
    fn covers(&self, path: &Path) -> bool {
        if self.paths.is_empty() {
            return false;
        }
        if self.paths.iter().any(|excluded| excluded.covers(path)) {
            return true;
        }
        let identities: Vec<_> = self.paths.iter().filter_map(|p| p.lineage.own()).collect();
        if identities.is_empty() {
            return false;
        }
        self.fs
            .metadata(path)
            .ok()
            .and_then(|meta| meta.identity)
            .is_some_and(|identity| identities.contains(&identity))
    }
}

/// The walks of one scan, each run once by whichever integration asks first, none
/// entering an excluded path.
#[derive(Debug, Default)]
pub(crate) struct Walks {
    cells: Mutex<HashMap<PathBuf, Arc<OnceLock<Arc<Walk>>>>>,
    exclude: Vec<HeldPath>,
}

impl Walks {
    pub(crate) fn new(exclude: &[HeldPath]) -> Self {
        Self {
            cells: Mutex::default(),
            exclude: exclude.to_vec(),
        }
    }

    /// The walk of `root`, running it now unless another integration already did. Waits
    /// while another integration is running it.
    pub(crate) fn get(
        &self,
        plan: &WalkPlan,
        fs: &dyn FsProbe,
        root: &Path,
        cancel: &CancelToken,
    ) -> Arc<Walk> {
        let cell = {
            let mut cells = self.cells.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(cells.entry(root.to_owned()).or_default())
        };
        Arc::clone(cell.get_or_init(|| Arc::new(plan.walk(fs, root, &self.exclude, cancel))))
    }
}
