//! macOS: `proc_listallpids` and `proc_pidpath` for the path (D-035). The identity is the
//! vnode behind the process's first file-backed memory region (its main executable's
//! `__TEXT`), trusted only when that region's path is the path `proc_pidpath` returns.
//! The vnode survives the path being deleted or replaced; a later `stat(path)` does not.
//! `proc_pidinfo` and `KERN_PROCARGS2` give its owner and what else it can be seen using.

use std::ffi::{CStr, OsStr, c_char, c_int, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kipple_core::{
    FileIdentity, Inventory, Observer, Platform, ProbeError, Probed, ProcessOutcome, Roots,
    SymbolicRoot, Unread,
};

pub(crate) use super::unix::{
    INHERITED_VARS, KIPPLE_DIRS, allocated, device, dir_access, env_name_matches, hard_link,
    identity, is_executable, physical, raw_bytes,
};
use super::unix::{env_paths, nul_separated, owner, path_args, unread, xdg_roots};
use crate::environment::Environment;
use crate::process::Context;
use crate::roots::under;

pub(crate) const PLATFORM: Platform = Platform::Macos;

/// Whether Finder hides the entry: its `UF_HIDDEN` flag, as `~/Library` has.
pub(crate) fn hidden(meta: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt as _;
    meta.st_flags() & libc::UF_HIDDEN != 0
}

pub(crate) const PERMISSION_HINT: Option<&str> = Some(
    "macOS may be blocking access: give the terminal that runs kipple Full Disk Access in \
     System Settings > Privacy & Security > Full Disk Access.",
);

/// The XDG roots, which CLI tools use on macOS too, plus `~/Library/Caches` and the
/// trash, `~/.Trash`.
pub(crate) fn resolve_roots(env: &Environment) -> Roots {
    let mut roots = xdg_roots(env);
    let home = SymbolicRoot::Home;
    let caches = under(&roots, home, SymbolicRoot::MacosCaches, "Library/Caches");
    roots.insert(SymbolicRoot::MacosCaches, caches);
    let trash = under(&roots, home, SymbolicRoot::Trash, ".Trash");
    roots.insert(SymbolicRoot::Trash, trash);
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
    loop {
        let bytes = c_int::try_from(pids.len() * size_of::<c_int>())
            .map_err(|e| ProbeError::io(Path::new("proc_listallpids"), io::Error::other(e)))?;
        // SAFETY: the buffer is valid for `bytes` bytes.
        let filled =
            listed(unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast::<c_void>(), bytes) })?;
        // A full buffer may have been too small: grow it and ask again.
        if filled < pids.len() {
            pids.truncate(filled);
            return Ok(pids
                .into_iter()
                .filter_map(|pid| u32::try_from(pid).ok())
                .collect());
        }
        pids.resize(pids.len() * 2, 0);
    }
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

fn region_vnode(raw_pid: c_int) -> io::Result<Option<Region>> {
    let mut address = 0_u64;
    for _ in 0..MAX_REGIONS {
        let Some(info) = region_at(raw_pid, address)? else {
            return Ok(None);
        };
        let stat = &info.vnode.vip_vi.vi_stat;
        if stat.vst_ino != 0 {
            return Ok(Some(Region {
                path: vnode_path(&info.vnode),
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

/// The most regions to step through when listing mapped files. A process with more is
/// left unknown rather than listed in part.
const MAX_MAPPED_REGIONS: usize = 1 << 16;

/// Every file mapped into a process's memory, sorted, once each.
fn mapped_files(raw_pid: c_int) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut address = 0_u64;
    for _ in 0..MAX_MAPPED_REGIONS {
        let Some(info) = region_at(raw_pid, address)? else {
            files.sort();
            files.dedup();
            return Ok(files);
        };
        if info.vnode.vip_vi.vi_stat.vst_ino != 0 {
            files.push(vnode_path(&info.vnode));
        }
        let next = info.region.address.saturating_add(info.region.size);
        if next <= address {
            return Err(io::Error::other("the memory regions did not advance"));
        }
        address = next;
    }
    Err(io::Error::other("too many memory regions"))
}

/// The region at or after `address`, or `None` past the last one. XNU says there is none
/// with `EINVAL`: `fill_procregioninfo` (`osfmk/vm/bsd_vm.c`) finds no map entry at or
/// after the address, and `proc_pidregionpathinfo` (`bsd/kern/proc_info.c`) returns
/// `EINVAL` for that. On the first region, or with any other failure or a short record,
/// the list is unknown, so it is an error.
#[expect(unsafe_code, reason = "libc has no safe wrapper for proc_pidinfo")]
fn region_at(raw_pid: c_int, address: u64) -> io::Result<Option<ProcRegionWithPathInfo>> {
    let size = c_int::try_from(size_of::<ProcRegionWithPathInfo>()).map_err(io::Error::other)?;
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
    if filled == size {
        return Ok(Some(info));
    }
    if filled > 0 {
        return Err(io::Error::other("a memory region record was cut short"));
    }
    let error = io::Error::last_os_error();
    if address == 0 || error.raw_os_error() != Some(libc::EINVAL) {
        return Err(error);
    }
    Ok(None)
}

/// The path in a vnode record.
fn vnode_path(vnode: &libc::vnode_info_path) -> PathBuf {
    let bytes: Vec<u8> = vnode
        .vip_path
        .iter()
        .flatten()
        // `c_char` is signed here: reinterpret the byte, don't convert the value.
        .map(|&c| c.to_ne_bytes()[0])
        .take_while(|&b| b != 0)
        .collect();
    PathBuf::from(OsStr::from_bytes(&bytes))
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

#[expect(
    unsafe_code,
    reason = "libc doesn't declare the ACL functions of libSystem"
)]
unsafe extern "C" {
    fn acl_get_link_np(path: *const c_char, kind: c_int) -> *mut c_void;
    fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
    fn acl_get_tag_type(entry: *mut c_void, tag: *mut c_int) -> c_int;
    fn acl_free(object: *mut c_void) -> c_int;
}

/// `ACL_TYPE_EXTENDED`, the only kind of ACL macOS has.
const ACL_TYPE_EXTENDED: c_int = 0x100;
const ACL_FIRST_ENTRY: c_int = 0;
const ACL_NEXT_ENTRY: c_int = -1;
/// `ACL_EXTENDED_DENY`: an entry that only takes rights away.
const ACL_EXTENDED_DENY: c_int = 2;

/// Whether the entry at `path` carries an ACL with an entry that grants something. An
/// ACL made only of deny entries, such as the `everyone deny delete` of a home directory,
/// grants nothing.
#[expect(unsafe_code, reason = "the ACL functions are raw calls")]
pub(crate) fn grants_acl(path: &CStr) -> io::Result<bool> {
    // SAFETY: the path is NUL-terminated.
    let acl = unsafe { acl_get_link_np(path.as_ptr(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        // Without an ACL the call fails with ENOENT.
        return match error.raw_os_error() {
            Some(libc::ENOENT | libc::ENOTSUP) => Ok(false),
            _ => Err(error),
        };
    }
    let mut tags = Vec::new();
    let mut entry: *mut c_void = std::ptr::null_mut();
    let mut which = ACL_FIRST_ENTRY;
    // SAFETY: `acl` is a valid ACL until it is freed below, and each entry it hands out
    // is valid while it is.
    while unsafe { acl_get_entry(acl, which, &raw mut entry) } == 0 {
        which = ACL_NEXT_ENTRY;
        let mut tag: c_int = -1;
        // SAFETY: `entry` was just handed out by `acl_get_entry`.
        let read = unsafe { acl_get_tag_type(entry, &raw mut tag) };
        tags.push((read == 0).then_some(tag));
    }
    // SAFETY: `acl` came from `acl_get_link_np` and is freed once.
    unsafe { acl_free(acl) };
    Ok(grants(&tags))
}

/// Whether ACL entries with these tags grant anything: any entry that isn't known to
/// be a deny entry might.
fn grants(tags: &[Option<c_int>]) -> bool {
    tags.iter().any(|&tag| tag != Some(ACL_EXTENDED_DENY))
}

/// Whether `path` is on a file system macOS doesn't mark local, such as NFS, SMB or a
/// user-space one.
#[expect(unsafe_code, reason = "libc's statfs is a raw call")]
pub(crate) fn on_network(path: &CStr) -> io::Result<bool> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the path is NUL-terminated and `stat` is large enough for the result.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `statfs` succeeded, so it filled `stat`.
    let flags = unsafe { stat.assume_init() }.f_flags;
    Ok(flags & libc::MNT_LOCAL.cast_unsigned() == 0)
}

/// Whether a process's paths besides its executable are read: through `proc_pidinfo` and
/// `KERN_PROCARGS2`.
pub(crate) const PROCESS_PATHS: bool = true;

/// No mount namespaces or chroots to compare, and other users' command lines are hidden.
pub(crate) const fn observer() -> Observer {
    Observer {
        process_paths: PROCESS_PATHS,
        ..Observer::PLAIN
    }
}

/// Who kipple runs as, to tell the current user's processes from others'.
pub(crate) struct Owners {
    current: Option<u32>,
}

impl Owners {
    #[expect(unsafe_code, reason = "libc's getuid and geteuid are raw calls")]
    pub(crate) fn read() -> Self {
        // SAFETY: both always succeed.
        let (real, effective) = unsafe { (libc::getuid(), libc::geteuid()) };
        Self {
            current: (real == effective).then_some(real),
        }
    }
}

/// Everything macOS shows about a process besides its executable. Only the owner is
/// readable for every process; the rest usually only for the current user's. The mount
/// namespace, root directory and capabilities are Linux facts.
pub(crate) fn context(pid: u32, owners: &Owners) -> Context {
    let raw_pid = || c_int::try_from(pid).map_err(io::Error::other);
    let uids = raw_pid().ok().and_then(short_uids);
    let (argv, env_paths) = match raw_pid().and_then(procargs) {
        Ok(buf) => match split_procargs(&buf) {
            Ok(found) => (Ok(path_args(&found.args)), Ok(env_paths(&found.env))),
            Err(error) => both_unread(error),
        },
        Err(error) => both_unread(error),
    };
    Context {
        owner: owner(uids.as_ref().map(<[u32; 3]>::as_slice), owners.current),
        cwd: raw_pid().and_then(cwd).map_err(unread),
        argv,
        env_paths,
        mapped_files: raw_pid().and_then(mapped_files).map_err(unread),
        mount_namespace: Err(Unread::Unsupported),
        root_dir: Err(Unread::Unsupported),
        root_path: Err(Unread::Unsupported),
        root_entry: Err(Unread::Unsupported),
        capabilities: Err(Unread::Unsupported),
    }
}

fn both_unread<A, B>(error: io::Error) -> (Probed<A>, Probed<B>) {
    let error = Arc::new(error);
    (
        Err(Unread::Failed(Arc::clone(&error))),
        Err(Unread::Failed(error)),
    )
}

/// The real, effective and saved user IDs, which macOS gives for any user's process.
#[expect(unsafe_code, reason = "libc has no safe wrapper for proc_pidinfo")]
fn short_uids(raw_pid: c_int) -> Option<[u32; 3]> {
    // SAFETY: zeroed is a valid value for this plain C struct.
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = c_int::try_from(size_of::<libc::proc_bsdshortinfo>()).ok()?;
    // SAFETY: `info` is writable for `size` bytes.
    let filled = unsafe {
        libc::proc_pidinfo(
            raw_pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&raw mut info).cast::<c_void>(),
            size,
        )
    };
    (filled == size).then_some([info.pbsi_ruid, info.pbsi_uid, info.pbsi_svuid])
}

/// The working directory.
#[expect(unsafe_code, reason = "libc has no safe wrapper for proc_pidinfo")]
fn cwd(raw_pid: c_int) -> io::Result<PathBuf> {
    // SAFETY: zeroed is a valid value for this plain C struct.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = c_int::try_from(size_of::<libc::proc_vnodepathinfo>()).map_err(io::Error::other)?;
    // SAFETY: `info` is writable for `size` bytes.
    let filled = unsafe {
        libc::proc_pidinfo(
            raw_pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&raw mut info).cast::<c_void>(),
            size,
        )
    };
    if filled != size {
        return Err(io::Error::last_os_error());
    }
    let path = vnode_path(&info.pvi_cdir);
    if path.as_os_str().is_empty() {
        return Err(io::Error::other("no working directory path"));
    }
    Ok(path)
}

/// The raw `KERN_PROCARGS2` record: the argument count, the executable path, the
/// arguments and the environment.
fn procargs(raw_pid: c_int) -> io::Result<Vec<u8>> {
    let mut argmax = [0_u8; size_of::<c_int>()];
    sysctl(&mut [libc::CTL_KERN, libc::KERN_ARGMAX], &mut argmax)?;
    let argmax = usize::try_from(c_int::from_ne_bytes(argmax)).map_err(io::Error::other)?;
    let mut buf = vec![0_u8; argmax];
    let filled = sysctl(
        &mut [libc::CTL_KERN, libc::KERN_PROCARGS2, raw_pid],
        &mut buf,
    )?;
    buf.truncate(filled);
    Ok(buf)
}

/// Reads the `sysctl` value `mib` names into `buf`, and says how much it filled.
#[expect(unsafe_code, reason = "libc's sysctl is a raw call")]
fn sysctl(mib: &mut [c_int], buf: &mut [u8]) -> io::Result<usize> {
    let depth = u32::try_from(mib.len()).map_err(io::Error::other)?;
    let mut len = buf.len();
    // SAFETY: `mib` holds `depth` names, `buf` is writable for `len` bytes, and no new
    // value is set.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            depth,
            buf.as_mut_ptr().cast::<c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(len)
}

/// The arguments and environment entries of a `KERN_PROCARGS2` record.
#[derive(Debug)]
struct ProcArgs<'buf> {
    args: Vec<&'buf [u8]>,
    env: Vec<&'buf [u8]>,
}

/// After the argument count and the executable path come NUL padding, then the
/// arguments, then the environment up to an empty string.
fn split_procargs(buf: &[u8]) -> io::Result<ProcArgs<'_>> {
    let truncated = || io::Error::new(io::ErrorKind::InvalidData, "a truncated argument record");
    let (count, rest) = buf.split_first_chunk::<4>().ok_or_else(truncated)?;
    let count = usize::try_from(c_int::from_ne_bytes(*count)).map_err(|_| truncated())?;
    let mut strings = nul_separated(rest).into_iter();
    strings.next().ok_or_else(truncated)?;
    let mut strings = strings.skip_while(|s| s.is_empty());
    let args: Vec<&[u8]> = strings.by_ref().take(count).collect();
    if args.len() != count {
        return Err(truncated());
    }
    let env = strings.take_while(|s| !s.is_empty()).collect();
    Ok(ProcArgs { args, env })
}

/// macOS lists every process to every user.
pub(crate) const fn inventory() -> Inventory {
    Inventory::Complete
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_acl_of_deny_entries_grants_nothing() {
        let allow = 1;
        assert!(!grants(&[]));
        assert!(!grants(&[Some(ACL_EXTENDED_DENY)]));
        assert!(!grants(&[Some(ACL_EXTENDED_DENY), Some(ACL_EXTENDED_DENY)]));
        assert!(grants(&[Some(ACL_EXTENDED_DENY), Some(allow)]));
        // An entry whose tag couldn't be read might grant something.
        assert!(grants(&[Some(ACL_EXTENDED_DENY), None]));
    }

    fn record(argc: c_int, rest: &[u8]) -> Vec<u8> {
        let mut buf = argc.to_ne_bytes().to_vec();
        buf.extend_from_slice(rest);
        buf
    }

    #[test]
    fn procargs_splits_arguments_from_the_environment_after_the_padding() {
        let buf = record(
            2,
            b"/usr/bin/python3\0\0\0\0python3\0./manage.py\0VIRTUAL_ENV=/p/.venv\0\0junk",
        );

        let found = split_procargs(&buf).unwrap();

        assert_eq!(found.args, [&b"python3"[..], b"./manage.py"]);
        assert_eq!(found.env, [&b"VIRTUAL_ENV=/p/.venv"[..]]);
    }

    #[test]
    fn a_procargs_record_with_fewer_arguments_than_it_counts_is_unknown() {
        assert!(split_procargs(&record(3, b"/bin/x\0\0x\0")).is_err());
        assert!(split_procargs(&[1, 0]).is_err());
    }
}
