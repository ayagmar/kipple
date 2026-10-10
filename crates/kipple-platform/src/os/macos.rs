//! macOS: `proc_listallpids` and `proc_pidpath` for the path (D-035). The identity is the
//! vnode behind the process's first file-backed memory region (its main executable's
//! `__TEXT`), trusted only when that region's path is the path `proc_pidpath` returns.
//! The vnode survives the path being deleted or replaced; a later `stat(path)` does not.

use std::ffi::{OsStr, c_int, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kipple_core::{FileIdentity, Inventory, ProbeError, ProcessOutcome, Roots, SymbolicRoot};

pub(crate) use super::unix::{
    KIPPLE_DIRS, allocated, device, env_name_matches, hard_link, identity, is_executable,
    process_env,
};
use super::unix::{home, under, xdg_roots};
use crate::environment::Environment;

pub(crate) const PERMISSION_HINT: Option<&str> = Some(
    "macOS may be blocking access: give the terminal that runs kipple Full Disk Access in \
     System Settings > Privacy & Security > Full Disk Access.",
);

/// The XDG roots, which CLI tools use on macOS too, plus `~/Library/Caches`.
pub(crate) fn resolve_roots(env: &Environment) -> Roots {
    let home = home(env);
    let mut roots = xdg_roots(env, &home);
    let caches = under(&home, SymbolicRoot::MacosCaches, "Library/Caches");
    roots.insert(SymbolicRoot::MacosCaches, caches);
    roots
}

#[expect(unsafe_code, reason = "libc has no safe wrapper for proc_listallpids")]
pub(crate) fn list_pids() -> Result<Vec<u32>, ProbeError> {
    // It returns 0, not -1, when the listing fails. An empty listing is never real: at
    // least kipple itself is running.
    let failed = || ProbeError::io(Path::new("proc_listallpids"), io::Error::last_os_error());
    let listed = |n: c_int| {
        usize::try_from(n)
            .ok()
            .filter(|&n| n > 0)
            .ok_or_else(failed)
    };
    // SAFETY: a null buffer asks for the number of PIDs.
    let count = listed(unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) })?;
    // Room for processes started between the two calls.
    let mut pids: Vec<c_int> = vec![0; count * 2 + 16];
    let bytes = c_int::try_from(pids.len() * size_of::<c_int>())
        .map_err(|e| ProbeError::io(Path::new("proc_listallpids"), io::Error::other(e)))?;
    // SAFETY: the buffer is valid for `bytes` bytes.
    let filled =
        listed(unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast::<c_void>(), bytes) })?;
    pids.truncate(filled);
    Ok(pids
        .into_iter()
        .filter_map(|pid| u32::try_from(pid).ok())
        .collect())
}

pub(crate) fn probe_pid(pid: u32) -> ProcessOutcome {
    let Ok(raw_pid) = c_int::try_from(pid) else {
        return ProcessOutcome::Other(Arc::new(io::Error::other("pid out of range")));
    };
    let path = match pid_path(raw_pid) {
        Ok(path) => path,
        Err(error) => return classify(raw_pid, error),
    };
    match region_vnode(raw_pid) {
        Ok(Some(region)) if region.path == path => ProcessOutcome::Identified {
            path,
            identity: region.identity,
            deleted: region.deleted,
        },
        Ok(Some(region)) => path_only(
            path,
            format!(
                "the first mapped file is {}, not the executable",
                region.path.display()
            ),
        ),
        Ok(None) => path_only(path, "no file-backed memory region".to_owned()),
        Err(error) => path_only(path, format!("region info: {error}")),
    }
}

fn path_only(path: PathBuf, why: String) -> ProcessOutcome {
    ProcessOutcome::PathOnly {
        path,
        error: Arc::new(io::Error::other(why)),
    }
}

#[expect(unsafe_code, reason = "libc has no safe wrapper for proc_pidpath")]
fn pid_path(raw_pid: c_int) -> io::Result<PathBuf> {
    let mut buf = vec![0_u8; PROC_PIDPATHINFO_MAXSIZE];
    let capacity = u32::try_from(buf.len()).map_err(io::Error::other)?;
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

/// `PROC_PIDPATHINFO_MAXSIZE` from `libproc.h`: four times `MAXPATHLEN`.
const PROC_PIDPATHINFO_MAXSIZE: usize = 4 * 1024;

/// `PROC_PIDREGIONPATHINFO` and its result, from XNU's `sys/proc_info.h` (not in libc).
const PROC_PIDREGIONPATHINFO: c_int = 8;

/// The most regions to step through before giving up on finding a file-backed one.
const MAX_REGIONS: usize = 256;

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

/// The file behind a process's first file-backed memory region.
struct Region {
    path: PathBuf,
    identity: FileIdentity,
    deleted: bool,
}

#[expect(unsafe_code, reason = "libc has no safe wrapper for proc_pidinfo")]
fn region_vnode(raw_pid: c_int) -> io::Result<Option<Region>> {
    let size = c_int::try_from(size_of::<ProcRegionWithPathInfo>()).map_err(io::Error::other)?;
    let mut address = 0_u64;
    for _ in 0..MAX_REGIONS {
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
                // `c_char` is signed here: reinterpret the byte, don't convert the value.
                .map(|&c| c.to_ne_bytes()[0])
                .take_while(|&b| b != 0)
                .collect();
            return Ok(Some(Region {
                path: PathBuf::from(OsStr::from_bytes(&bytes)),
                identity: FileIdentity {
                    volume: u64::from(stat.vst_dev),
                    file: stat.vst_ino,
                },
                deleted: stat.vst_nlink == 0,
            }));
        }
        address = info.region.address.saturating_add(info.region.size);
    }
    Ok(None)
}

fn classify(raw_pid: c_int, error: io::Error) -> ProcessOutcome {
    if is_zombie(raw_pid) {
        return ProcessOutcome::Exited;
    }
    match error.raw_os_error() {
        Some(libc::ESRCH) => ProcessOutcome::Gone,
        Some(libc::EPERM | libc::EACCES) => ProcessOutcome::Denied(Arc::new(error)),
        _ if raw_pid == 0 => ProcessOutcome::NoExecutable,
        _ => ProcessOutcome::Other(Arc::new(error)),
    }
}

#[expect(unsafe_code, reason = "libc has no safe wrapper for proc_pidinfo")]
fn is_zombie(raw_pid: c_int) -> bool {
    // SAFETY: zeroed is a valid value for this plain C struct.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let Ok(size) = c_int::try_from(size_of::<libc::proc_bsdinfo>()) else {
        return false;
    };
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

/// macOS lists every process to every user.
pub(crate) const fn inventory() -> Inventory {
    Inventory::Complete
}
