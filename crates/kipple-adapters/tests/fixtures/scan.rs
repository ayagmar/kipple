//! Scanning a fixture tree with the built-in integrations, through the real file
//! system read without following links and no processes, git or sizes: what the
//! integration tests and the integration revision golden run.

use std::fs;
use std::io::{self, Read as _};
use std::path::Path;
use std::time::SystemTime;

use super::demo_home;
use super::tree::FixtureTree;
use kipple_adapters::builtin;
use kipple_core::{
    Anchor, Apparent, CancelToken, Clock, Completeness, DirEntryMeta, Engine, EntryKind, EntryMeta,
    EventChannel, FrozenTree, FsProbe, GitGap, Inventory, Measurement, NodeId, Observer, Ports,
    ProbeError, ProcessProbe, ProcessSnapshot, ResolvedRoot, RootSource, Roots, ScanReport,
    ScanRequest, Sizer, SpaceEstimate, SymbolicRoot, TrackedQuery, event_channel,
};

/// The real file system, read without following links: enough to run the integrations
/// over a fixture in a temp dir.
#[derive(Debug)]
struct TempFs;

fn kind(file_type: fs::FileType) -> EntryKind {
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

impl FsProbe for TempFs {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        let meta = fs::symlink_metadata(path).map_err(|e| ProbeError::io(path, e))?;
        Ok(EntryMeta {
            kind: kind(meta.file_type()),
            identity: None,
        })
    }

    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        let read = || -> io::Result<Vec<DirEntryMeta>> {
            fs::read_dir(dir)?
                .map(|entry| {
                    let entry = entry?;
                    Ok(DirEntryMeta {
                        name: entry.file_name(),
                        kind: kind(entry.file_type()?),
                    })
                })
                .collect()
        };
        read().map_err(|e| ProbeError::io(dir, e))
    }

    fn open_dir(&self, dir: &Path) -> Result<(), ProbeError> {
        fs::read_dir(dir)
            .map(drop)
            .map_err(|e| ProbeError::io(dir, e))
    }

    fn read_file(&self, path: &Path, limit: u64) -> Result<Vec<u8>, ProbeError> {
        let mut bytes = Vec::new();
        fs::File::open(path)
            .and_then(|file| file.take(limit).read_to_end(&mut bytes))
            .map_err(|e| ProbeError::io(path, e))?;
        Ok(bytes)
    }
}

/// Sizes nothing: these tests are about what is found, not how big it is or when it
/// changed.
#[derive(Debug)]
struct NoSizer;

impl Sizer for NoSizer {
    fn size(&self, _: &Path, _: &CancelToken) -> Result<Measurement, ProbeError> {
        let size = SpaceEstimate {
            apparent: Apparent(0),
            allocated: None,
            unique_reclaim: None,
            completeness: Completeness::Complete,
        };
        Ok(Measurement {
            size,
            modified: None,
            repository: None,
        })
    }
}

/// A host that runs nothing.
#[derive(Debug)]
struct NoProcesses;

/// No git: the fixtures hold no repository, so it is never asked.
#[derive(Debug)]
struct NoGit;

impl TrackedQuery for NoGit {
    fn count(&self, _: &Path, _: &Path, _: &CancelToken) -> Result<u64, GitGap> {
        Err(GitGap::Unsupported)
    }

    fn entries(
        &self,
        _: &Path,
        _: &Anchor,
        _: &FrozenTree,
        _: &CancelToken,
    ) -> Result<Vec<NodeId>, GitGap> {
        Err(GitGap::Unsupported)
    }
}

impl ProcessProbe for NoProcesses {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        Ok(ProcessSnapshot {
            processes: Vec::new(),
            inventory: Inventory::Complete,
            observer: Observer::PLAIN,
        })
    }
}

/// A clock that never moves.
#[derive(Debug)]
struct Epoch;

impl Clock for Epoch {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
}

/// A fixture tree built in a temp dir, and what scanning it found.
pub(crate) struct Scanned {
    /// Where the tree is; dropping it removes the tree.
    pub temp: tempfile::TempDir,
    /// What the scan found.
    pub report: ScanReport,
}

/// Builds `tree` in a temp dir and scans it with every built-in integration and rule,
/// each root of `demo_home::ROOTS` placed inside it.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
pub(crate) fn scan(tree: &FixtureTree) -> Scanned {
    let temp = tempfile::tempdir().unwrap();
    tree.build_in(temp.path()).unwrap();
    scan_built(temp)
}

/// Scans the tree already built in `temp`, like [`scan`].
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
pub(crate) fn scan_built(temp: tempfile::TempDir) -> Scanned {
    let mut roots = Roots::default();
    for (name, relative) in demo_home::ROOTS {
        let root = SymbolicRoot::from_name(name).unwrap();
        let resolved = ResolvedRoot::new(root, temp.path().join(relative), RootSource::Fixed);
        roots.insert(root, Ok(resolved));
    }
    let packs = builtin::packs(&semver::Version::new(0, 0, 0)).unwrap();
    let rules = packs.into_iter().flat_map(|pack| pack.rules).collect();
    let ports = Ports {
        fs: Box::new(TempFs),
        sizer: Box::new(NoSizer),
        processes: Box::new(NoProcesses),
        clock: Box::new(Epoch),
        tracked: Box::new(NoGit),
    };
    let engine = Engine::new(ports, builtin::integrations(), rules).unwrap();
    let cancel = CancelToken::new();
    let EventChannel { tx: events, rx } = event_channel(4096, &cancel);
    let report = engine.scan(
        &ScanRequest {
            roots,
            lens: None,
            exclude: Vec::new(),
        },
        &events,
        &cancel,
    );
    drop(rx);
    Scanned { temp, report }
}
