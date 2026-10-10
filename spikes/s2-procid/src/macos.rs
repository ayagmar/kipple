//! macOS: `proc_listallpids` and `proc_pidpath`. The identity comes from `stat` on the
//! returned path, so it is only as good as the path still naming the running file.

use std::ffi::{OsStr, c_int, c_void};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use crate::{FileId, Outcome};

#[expect(
    unsafe_code,
    reason = "S2 spike: libc has no safe wrapper for proc_listallpids"
)]
pub(crate) fn list_pids() -> io::Result<Vec<u32>> {
    // SAFETY: a null buffer asks for the number of PIDs.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return Err(io::Error::last_os_error());
    };
    let mut pids: Vec<c_int> = vec![0; count * 2 + 16];
    let bytes = c_int::try_from(pids.len() * size_of::<c_int>()).map_err(io::Error::other)?;
    // SAFETY: the buffer is valid for `bytes` bytes.
    let filled = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast::<c_void>(), bytes) };
    let Ok(filled) = usize::try_from(filled) else {
        return Err(io::Error::last_os_error());
    };
    pids.truncate(filled);
    Ok(pids
        .into_iter()
        .filter_map(|pid| u32::try_from(pid).ok())
        .collect())
}

#[expect(
    unsafe_code,
    reason = "S2 spike: libc has no safe wrapper for proc_pidpath"
)]
pub(crate) fn probe(pid: u32) -> Outcome {
    let Ok(raw_pid) = c_int::try_from(pid) else {
        return Outcome::Other(io::Error::other("pid out of range"));
    };
    let mut buf = vec![0_u8; 4 * 1024];
    let capacity = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: the buffer is valid for `capacity` bytes.
    let len = unsafe { libc::proc_pidpath(raw_pid, buf.as_mut_ptr().cast::<c_void>(), capacity) };
    let Ok(len) = usize::try_from(len) else {
        return classify(pid, io::Error::last_os_error());
    };
    if len == 0 {
        return classify(pid, io::Error::last_os_error());
    }
    buf.truncate(len);
    let path = PathBuf::from(OsStr::from_bytes(&buf));
    match file_id(&path) {
        Ok(id) => Outcome::Identified {
            path,
            id,
            deleted: false,
        },
        Err(error) => Outcome::PathOnly { path, error },
    }
}

fn classify(pid: u32, error: io::Error) -> Outcome {
    if is_zombie(pid) {
        return Outcome::Exited("zombie");
    }
    match error.raw_os_error() {
        Some(libc::ESRCH) => Outcome::Gone,
        Some(libc::EPERM | libc::EACCES) => Outcome::Denied(error),
        _ if pid == 0 => Outcome::NoExecutable("kernel_task"),
        _ => Outcome::Other(error),
    }
}

#[expect(
    unsafe_code,
    reason = "S2 spike: libc has no safe wrapper for proc_pidinfo"
)]
fn is_zombie(pid: u32) -> bool {
    let Ok(raw_pid) = c_int::try_from(pid) else {
        return false;
    };
    // SAFETY: zeroed is a valid value for this plain C struct.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = c_int::try_from(size_of::<libc::proc_bsdinfo>()).unwrap_or(c_int::MAX);
    // SAFETY: `info` is writable for `size` bytes.
    let filled = unsafe {
        libc::proc_pidinfo(
            raw_pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast::<c_void>(),
            size,
        )
    };
    filled == size && info.pbi_status == libc::SZOMB
}

pub(crate) fn file_id(path: &Path) -> io::Result<FileId> {
    fs::metadata(path).map(|meta| FileId {
        volume: meta.dev(),
        file: meta.ino(),
    })
}

pub(crate) const fn inventory_note() -> Option<String> {
    None
}

pub(crate) fn privileged_pids() -> Vec<(u32, &'static str)> {
    vec![(0, "pid 0, kernel_task"), (1, "pid 1, launchd")]
}
