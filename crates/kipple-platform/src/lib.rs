//! kipple's operating-system facts, probes, native-command runner and confined executor.
//!
//! This is the only crate that may contain OS-specific code, and the only one where
//! reviewed `unsafe` is allowed (see `docs/05-architecture.md`). Everything that differs
//! by OS lives in its `os` module, and in `executor`'s and `trash`'s own OS modules.
//! Everything here is read-only except the atomic write of kipple's own config file, the
//! new plan files `kipple plan -o` writes, the run store ([`FileRunStore`]), which writes only kipple's own runs, the confined
//! executor ([`ConfinedFs`]), the only code that removes files, and the trash
//! ([`ConfinedTrash`]), the only code that moves the user's files.

mod capabilities;
mod config_file;
mod environment;
mod executor;
mod fs_probe;
mod git;
mod host;
mod interrupt;
mod os;
mod process;
mod project_search;
mod roots;
mod runner;
mod runs;
#[cfg(feature = "test-roots")]
mod test_host;
mod trash;
mod typeahead;
mod walk;

pub use capabilities::{ToolProbe, probe_tools};
pub use config_file::{WriteError, read_config, write_config, write_new};
pub use environment::Environment;
pub use executor::{ConfinedFs, OsNames};
pub use fs_probe::NoFollowFs;
pub use git::GitQuery;
pub use host::{SystemClock, clock, processes};
pub use interrupt::cancel_on_interrupt;
pub use process::OsProcesses;
pub use project_search::{ProjectSearch, ProjectsFound, SearchLimits, search_projects};
pub use roots::{
    CONFIG_FILE, KippleDir, KippleDirs, follow_root_links, kipple_dirs, resolve_roots,
    resolve_roots_from,
};
pub use runs::FileRunStore;
pub use trash::ConfinedTrash;
pub use typeahead::discard_typeahead;
pub use walk::Walker;

/// The operating system kipple was built for.
pub const PLATFORM: kipple_core::Platform = os::PLATFORM;

/// The bytes of `path` exactly as the OS holds them, when it isn't valid Unicode.
///
/// They are the bytes on Unix, and the UTF-16 code units as little-endian bytes on
/// Windows. `None` when it is valid Unicode, so its display string holds it whole.
#[must_use]
pub fn raw_path(path: &std::path::Path) -> Option<Vec<u8>> {
    path.to_str()
        .is_none()
        .then(|| os::raw_bytes(path.as_os_str()))
}

/// A hint to show only after a permission failure was actually diagnosed, such as
/// macOS Full Disk Access. `None` where the OS has no such setting.
#[must_use]
pub const fn permission_hint() -> Option<&'static str> {
    os::PERMISSION_HINT
}
