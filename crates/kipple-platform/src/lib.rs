//! kipple's operating-system facts, probes and native-command runner.
//!
//! This is the only crate that may contain OS-specific code, and the only one where
//! reviewed `unsafe` is allowed (see `docs/05-architecture.md`). Everything that differs
//! by OS lives in its `os` module. Everything here is read-only: the confined executor arrives
//! with spike S1.

mod capabilities;
mod environment;
mod fs_probe;
mod os;
mod process;
mod roots;
mod runner;
mod walk;

pub use capabilities::probe_tools;
pub use environment::Environment;
pub use fs_probe::NoFollowFs;
pub use process::OsProcesses;
pub use roots::{CONFIG_FILE, KippleDir, KippleDirs, kipple_dirs, resolve_roots};
pub use walk::Walker;

/// A hint to show only after a permission failure was actually diagnosed, such as
/// macOS Full Disk Access. `None` where the OS has no such setting.
#[must_use]
pub const fn permission_hint() -> Option<&'static str> {
    os::PERMISSION_HINT
}
