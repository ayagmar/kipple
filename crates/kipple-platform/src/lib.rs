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
pub use roots::{
    CONFIG_FILE, KippleDir, KippleDirs, follow_root_links, kipple_dirs, resolve_roots,
};
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
