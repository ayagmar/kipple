//! The hidden test-roots override (`docs/11-delivery-plan.md`, M1): an end-to-end test
//! describes the whole host in one file, so a scan resolves nothing of the real account,
//! reads no real process and runs at a fixed time. Compiled only with the `test-roots`
//! feature, so release builds can't use it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kipple_core::{
    Clock, Inventory, ProbeError, ProcessProbe, ProcessSnapshot, ResolvedRoot, RootSource, Roots,
    SymbolicRoot, UnresolvedRoot,
};

use crate::environment::Environment;

/// The variable naming the file.
const VAR: &str = "KIPPLE_TEST_ROOTS";

/// The file the override names, when it is set.
pub(crate) fn file(env: &Environment) -> Option<&Path> {
    env.get(VAR).map(Path::new)
}

/// The lines of `file` as `name = value` pairs, none when it can't be read.
fn lines(file: &Path) -> Vec<(String, String)> {
    let text = fs::read_to_string(file).unwrap_or_default();
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

/// The roots in `file`, one `name = path` per line. A root that isn't listed is absent,
/// and so is every root when the file can't be read. A line that doesn't name a root is
/// ignored; a relative path leaves its root unknown.
pub(crate) fn roots(file: &Path) -> Roots {
    let mut roots = Roots::default();
    for (name, value) in lines(file) {
        let Some(root) = SymbolicRoot::from_name(&name) else {
            continue;
        };
        let path = PathBuf::from(&value);
        let resolved = if path.is_absolute() {
            Ok(ResolvedRoot::new(root, path, RootSource::Env(VAR)))
        } else {
            Err(UnresolvedRoot::NotAbsolute { var: VAR, value })
        };
        roots.add(root, resolved);
    }
    roots
}

/// The time a `now = <seconds since the Unix epoch>` line of `file` fixes, if any.
pub(crate) fn now(file: &Path) -> Option<SystemTime> {
    let (_, value) = lines(file).into_iter().find(|(name, _)| name == "now")?;
    let seconds = value.parse().ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

/// A clock that always says the same time.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FixedClock(pub(crate) SystemTime);

impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        self.0
    }
}

/// A host that runs nothing: every process was listed, and none runs anything.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NoProcesses;

impl ProcessProbe for NoProcesses {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        Ok(ProcessSnapshot {
            processes: Vec::new(),
            inventory: Inventory::Complete,
        })
    }
}
