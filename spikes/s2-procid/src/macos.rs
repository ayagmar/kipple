//! macOS: `proc_listallpids` and `proc_pidpath`. The identity comes from `stat` on the
//! returned path, so it is only as good as the path still naming the running file.

use std::ffi::{OsStr, c_int, c_void};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// `PROC_PIDREGIONPATHINFO` and its result, from XNU's `sys/proc_info.h` (not in libc).
const PROC_PIDREGIONPATHINFO: c_int = 8;

#[repr(C)]
struct ProcRegionInfo {
    protection: u32,
    max_protection: u32,
    inheritance: u32,
    flags: u32,
    offset: u64,
    behavior: u32,
    user_wired_count: u32,
    user_tag: u32,
    pages_resident: u32,
    pages_shared_now_private: u32,
    pages_swapped_out: u32,
    pages_dirtied: u32,
    ref_count: u32,
    shadow_depth: u32,
    share_mode: u32,
    private_pages_resident: u32,
    shared_pages_resident: u32,
    obj_id: u32,
    depth: u32,
    address: u64,
    size: u64,
}

#[repr(C)]
struct ProcRegionWithPathInfo {
    region: ProcRegionInfo,
    vnode: libc::vnode_info_path,
}

/// Processes whose first file-backed region is not the file `proc_pidpath` names.
static REGION_PATH_MISMATCHES: AtomicUsize = AtomicUsize::new(0);

/// The running image: the path from `proc_pidpath`, the identity from the vnode behind
/// the process's first file-backed memory region (its main executable's `__TEXT`). The
/// vnode survives deletion and replacement of the path; a later `stat(path)` does not.
pub(crate) fn probe(pid: u32) -> Outcome {
    let Ok(raw_pid) = c_int::try_from(pid) else {
        return Outcome::Other(io::Error::other("pid out of range"));
    };
    let path = pid_path(raw_pid);
    match (region_vnode(raw_pid), path) {
        // The first file-backed region is only trusted when it is the file `proc_pidpath`
        // names; anything else (an earlier mapping, a failed lookup) stays unknown.
        (Ok(Some((id, deleted, region_path))), Ok(path)) => {
            if region_path == path {
                return Outcome::Identified { path, id, deleted };
            }
            REGION_PATH_MISMATCHES.fetch_add(1, Ordering::Relaxed);
            Outcome::PathOnly {
                path,
                error: io::Error::other(format!(
                    "first mapped file is {}, not the executable",
                    region_path.display()
                )),
            }
        }
        (Ok(Some((_, _, region_path))), Err(path_error)) => {
            let outcome = classify(pid, path_error);
            if let Outcome::Other(error) = outcome {
                Outcome::PathOnly {
                    path: region_path,
                    error,
                }
            } else {
                outcome
            }
        }
        (Ok(None), Ok(path)) => Outcome::PathOnly {
            path,
            error: io::Error::other("no file-backed region"),
        },
        (Err(error), Ok(path)) => Outcome::PathOnly {
            path,
            error: io::Error::other(format!("region info: {error}")),
        },
        (Ok(None) | Err(_), Err(error)) => classify(pid, error),
    }
}

#[expect(
    unsafe_code,
    reason = "S2 spike: libc has no safe wrapper for proc_pidpath"
)]
fn pid_path(raw_pid: c_int) -> io::Result<PathBuf> {
    let mut buf = vec![0_u8; 4 * 1024];
    let capacity = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: the buffer is valid for `capacity` bytes.
    let len = unsafe { libc::proc_pidpath(raw_pid, buf.as_mut_ptr().cast::<c_void>(), capacity) };
    match usize::try_from(len) {
        Ok(len) if len > 0 => {
            buf.truncate(len);
            Ok(PathBuf::from(OsStr::from_bytes(&buf)))
        }
        _ => Err(io::Error::last_os_error()),
    }
}

#[expect(
    unsafe_code,
    reason = "S2 spike: libc has no safe wrapper for proc_pidinfo"
)]
fn region_vnode(raw_pid: c_int) -> io::Result<Option<(FileId, bool, PathBuf)>> {
    let size = c_int::try_from(size_of::<ProcRegionWithPathInfo>()).map_err(io::Error::other)?;
    let mut address = 0_u64;
    for _ in 0..256 {
        // SAFETY: zeroed is a valid value for these plain C structs.
        let mut info: ProcRegionWithPathInfo = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is writable for `size` bytes.
        let filled = unsafe {
            libc::proc_pidinfo(
                raw_pid,
                PROC_PIDREGIONPATHINFO,
                address,
                (&raw mut info).cast::<c_void>(),
                size,
            )
        };
        if filled != size {
            let error = io::Error::last_os_error();
            return if address == 0 { Err(error) } else { Ok(None) };
        }
        let stat = &info.vnode.vip_vi.vi_stat;
        if stat.vst_ino != 0 {
            let bytes: Vec<u8> = info
                .vnode
                .vip_path
                .iter()
                .flatten()
                .map_while(|&c| u8::try_from(c).ok().filter(|&b| b != 0))
                .collect();
            let id = FileId {
                volume: u64::from(stat.vst_dev),
                file: stat.vst_ino,
            };
            return Ok(Some((
                id,
                stat.vst_nlink == 0,
                PathBuf::from(OsStr::from_bytes(&bytes)),
            )));
        }
        address = info.region.address.saturating_add(info.region.size);
    }
    Ok(None)
}

#[expect(clippy::unnecessary_wraps, reason = "the other OSes return None")]
pub(crate) fn diagnostics() -> Option<String> {
    Some(format!(
        "first mapped file differs from proc_pidpath for {} processes",
        REGION_PATH_MISMATCHES.load(Ordering::Relaxed)
    ))
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
