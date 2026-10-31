//! Everything that differs by operating system. `#[cfg(target_os)]` appears only in this
//! crate, and outside this module only to pick a test that needs one OS.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux::*;
#[cfg(target_os = "macos")]
pub(crate) use macos::*;
#[cfg(unix)]
pub(crate) use unix::{create_private_dir, create_private_file, own_process_group, sync_dir};
#[cfg(windows)]
pub(crate) use windows::*;

/// A file with more than one name: its identity, and how many names it has.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HardLink {
    pub(crate) identity: kipple_core::FileIdentity,
    pub(crate) names: u64,
}
