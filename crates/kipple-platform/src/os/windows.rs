//! Windows: Known Folders for roots, and `K32EnumProcesses`,
//! `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` and `QueryFullProcessImageNameW` for
//! processes (D-035), and `WTSEnumerateProcessesExW` for who each one runs as. File
//! identity is the volume serial and file index of an open handle.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString, c_void};
use std::fs;
use std::io;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::Arc;

use kipple_core::{
    DirAccess, FileIdentity, Inventory, Observer, Platform, ProbeError, ProcessOutcome,
    ProcessOwner, ResolvedRoot, RootSource, Roots, SymbolicRoot, Unread, UnresolvedRoot, UserId,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, LocalFree, STILL_ACTIVE,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_HIDDEN, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, GetFileInformationByHandle,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::ProcessStatus::K32EnumProcesses;
use windows_sys::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTS_PROCESS_INFO_EXW, WTSEnumerateProcessesExW, WTSFreeMemoryExW,
    WTSTypeProcessInfoLevel1,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_Profile, FOLDERID_RoamingAppData, SHGetKnownFolderPath,
};
use windows_sys::core::{GUID, PWSTR};

use crate::environment::Environment;
use crate::os::HardLink;
use crate::process::Context;

pub(crate) const PLATFORM: Platform = Platform::Windows;

/// Whether Explorer hides the entry: its hidden attribute, as `AppData` has.
pub(crate) fn hidden(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

pub(crate) const PERMISSION_HINT: Option<&str> = None;

/// kipple's own directories under the Known Folders (D-015, D-037).
pub(crate) const KIPPLE_DIRS: [(SymbolicRoot, &str); 3] = [
    (SymbolicRoot::RoamingAppData, r"kipple\config"),
    (SymbolicRoot::RoamingAppData, r"kipple\data"),
    (SymbolicRoot::LocalAppData, r"kipple\cache"),
];

const KNOWN_FOLDERS: [(SymbolicRoot, &GUID, &str); 3] = [
    (SymbolicRoot::Home, &FOLDERID_Profile, "FOLDERID_Profile"),
    (
        SymbolicRoot::LocalAppData,
        &FOLDERID_LocalAppData,
        "FOLDERID_LocalAppData",
    ),
    (
        SymbolicRoot::RoamingAppData,
        &FOLDERID_RoamingAppData,
        "FOLDERID_RoamingAppData",
    ),
];

pub(crate) fn raw_bytes(text: &OsStr) -> Vec<u8> {
    text.encode_wide().flat_map(u16::to_le_bytes).collect()
}

/// The directory an absolute `path` names, with its links and `..` resolved, in its usual
/// `C:\` or `\\server\share` form rather than the `\\?\` one `fs::canonicalize` gives, so
/// a root reached through a link and one given directly are the same path.
pub(crate) fn physical(path: &Path) -> io::Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    let mut components = canonical.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Ok(canonical);
    };
    let mut usual = OsString::new();
    match prefix.kind() {
        Prefix::VerbatimDisk(disk) => usual.push(format!("{}:", char::from(disk))),
        Prefix::VerbatimUNC(server, share) => {
            for part in [OsStr::new(r"\\"), server, OsStr::new(r"\"), share] {
                usual.push(part);
            }
        }
        Prefix::Verbatim(_) | Prefix::DeviceNS(_) | Prefix::UNC(..) | Prefix::Disk(_) => {
            return Ok(canonical);
        }
    }
    Ok(PathBuf::from(usual).join(components.as_path()))
}

/// Creates `dir` and any missing parent. They inherit the access rules of the profile
/// directory they are in.
pub(crate) fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

/// Creates a new file at `path`, failing if anything is there. It inherits the access
/// rules of its directory.
pub(crate) fn create_private_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Nothing to do: Windows can't sync a directory through std, and a rename replaces the
/// file in one step (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`).
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Unix version can fail, and callers share one signature"
)]
pub(crate) const fn sync_dir(_: &Path) -> io::Result<()> {
    Ok(())
}

pub(crate) fn env_name_matches(key: &OsStr, name: &str) -> bool {
    key.eq_ignore_ascii_case(name)
}

pub(crate) fn resolve_roots(_: &Environment) -> Roots {
    let mut roots = Roots::default();
    for (root, id, folder) in KNOWN_FOLDERS {
        let resolved = known_folder(id)
            .map(|path| ResolvedRoot::new(root, path, RootSource::KnownFolder(folder)))
            .map_err(|error| UnresolvedRoot::Os {
                folder,
                source: Arc::new(error),
            });
        roots.insert(root, resolved);
    }
    roots
}

#[expect(
    unsafe_code,
    reason = "windows-sys exposes the Known Folder API as a raw call"
)]
fn known_folder(id: &GUID) -> io::Result<PathBuf> {
    let mut raw: PWSTR = std::ptr::null_mut();
    // SAFETY: `id` is a valid GUID, a null token means the current user, and `raw` is
    // writable. The returned buffer is freed below whatever the result.
    let result = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &raw mut raw) };
    let path = if result == 0 && !raw.is_null() {
        // SAFETY: on success `raw` is a NUL-terminated UTF-16 string.
        Ok(PathBuf::from(unsafe { from_wide_nul(raw) }))
    } else {
        Err(io::Error::other(format!(
            "SHGetKnownFolderPath failed: HRESULT {result:#010x}"
        )))
    };
    // SAFETY: `raw` came from SHGetKnownFolderPath, which allocates it with CoTaskMemAlloc
    // (or leaves it null, which CoTaskMemFree accepts).
    unsafe { CoTaskMemFree(raw.cast::<c_void>().cast_const()) };
    path
}

/// The string a non-null, NUL-terminated UTF-16 pointer holds.
///
/// # Safety
/// `raw` must point to a NUL-terminated UTF-16 string.
#[expect(unsafe_code, reason = "reads a string an OS call returned")]
unsafe fn from_wide_nul(raw: PWSTR) -> OsString {
    let mut len = 0;
    // SAFETY: the caller promises a NUL before the end of the allocation.
    while unsafe { *raw.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `raw` holds `len` initialised units before the NUL.
    OsString::from_wide(unsafe { std::slice::from_raw_parts(raw, len) })
}

/// Who may enter a directory isn't read on Windows, so every other user's process counts.
pub(crate) fn dir_access(_: &Path) -> io::Result<DirAccess> {
    Err(io::ErrorKind::Unsupported.into())
}

/// The identity of the entry at `path` itself, through a handle that does not follow a
/// reparse point.
pub(crate) fn identity(path: &Path, _: &fs::Metadata) -> Option<FileIdentity> {
    file_identity(path).ok()
}

#[expect(
    unsafe_code,
    reason = "windows-sys exposes GetFileInformationByHandle as a raw call"
)]
fn file_identity(path: &Path) -> io::Result<FileIdentity> {
    let file = fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    // SAFETY: zeroed is a valid value for this plain C struct.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open for the lifetime of `file`, and `info` is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileIdentity {
        volume: u64::from(info.dwVolumeSerialNumber),
        file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

/// The walker never crosses a reparse point, so it can't leave the volume either.
pub(crate) const fn device(_: &fs::Metadata) -> Option<u64> {
    None
}

/// Allocation sizes need a handle per file, which a scan can't afford.
pub(crate) const fn allocated(_: &fs::Metadata) -> Option<u64> {
    None
}

/// Link counts need a handle per file, which a scan can't afford.
pub(crate) const fn hard_link(_: &fs::Metadata) -> Option<HardLink> {
    None
}

/// Windows runs a file by its extension, which the search already requires.
pub(crate) const fn is_executable(_: &fs::Metadata) -> bool {
    true
}

/// The variables a process is started with besides `LC_ALL=C`, where they are set.
/// Processes fail in odd ways without `SystemRoot`.
pub(crate) const INHERITED_VARS: &[&str] = &["SystemRoot"];

#[expect(
    unsafe_code,
    reason = "windows-sys exposes K32EnumProcesses as a raw call"
)]
pub(crate) fn list_pids() -> Result<Vec<u32>, ProbeError> {
    let failed = |error| ProbeError::io(Path::new("K32EnumProcesses"), error);
    let mut pids = vec![0_u32; 1024];
    loop {
        let bytes = u32::try_from(pids.len() * 4).map_err(|e| failed(io::Error::other(e)))?;
        let mut needed = 0;
        // SAFETY: the buffer is valid for `bytes` bytes.
        if unsafe { K32EnumProcesses(pids.as_mut_ptr(), bytes, &raw mut needed) } == 0 {
            return Err(failed(io::Error::last_os_error()));
        }
        // A full buffer may have been too small: grow it and ask again.
        if needed < bytes {
            pids.truncate(needed as usize / 4);
            return Ok(pids);
        }
        pids.resize(pids.len() * 2, 0);
    }
}

/// Closes a process or token handle on drop.
struct Process(HANDLE);

impl Drop for Process {
    #[expect(unsafe_code, reason = "closing a handle this module opened")]
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess or OpenProcessToken and is closed
        // exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// `WTS_ANY_SESSION` from `wtsapi32.h`, which windows-sys doesn't define.
const WTS_ANY_SESSION: u32 = 0xFFFF_FFFE;

/// The SID of `SYSTEM`. `LocalService` (S-1-5-19) and `NetworkService` (S-1-5-20) are other
/// users.
const SYSTEM_SID: &str = "S-1-5-18";

/// Whether a process's paths besides its executable are read: not in M2.
pub(crate) const PROCESS_PATHS: bool = false;

/// No mount namespaces or chroots to compare, other users' command lines are hidden, and
/// no process's paths besides its executable are read.
pub(crate) const fn observer() -> Observer {
    Observer {
        process_paths: PROCESS_PATHS,
        ..Observer::PLAIN
    }
}

/// Who every listed process runs as: its user (`TokenUser`, not `TokenOwner`), from one
/// listing that needs no handle to the process. A process missing from it, or listed
/// without a readable SID, is unknown.
pub(crate) struct Owners(HashMap<u32, ProcessOwner>);

impl Owners {
    pub(crate) fn read() -> Self {
        let owners = current_user()
            .and_then(|current| listed_owners(&current))
            .unwrap_or_default();
        Self(owners)
    }
}

/// The current user's SID.
#[expect(
    unsafe_code,
    reason = "windows-sys exposes the token APIs as raw calls"
)]
fn current_user() -> io::Result<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo-handle of this process needs no closing, and `token` is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Process(token);
    let mut needed = 0;
    // SAFETY: a null buffer asks for the size; the call fails by design.
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &raw mut needed) };
    // `u64`s keep the buffer aligned for `TOKEN_USER`.
    let mut buf = vec![0_u64; (needed as usize).div_ceil(8)];
    // SAFETY: the buffer is writable for `needed` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast::<c_void>(),
            needed,
            &raw mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the buffer starts with a `TOKEN_USER` whose SID points into it.
    let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
    sid_string(user.User.Sid)
}

/// The owners of every process in every session.
#[expect(
    unsafe_code,
    reason = "windows-sys exposes the WTS process listing as a raw call"
)]
fn listed_owners(current: &str) -> io::Result<HashMap<u32, ProcessOwner>> {
    let mut level = 1_u32;
    let mut info: PWSTR = std::ptr::null_mut();
    let mut count = 0_u32;
    // SAFETY: every out-pointer is writable. On success `info` holds `count` entries,
    // freed below.
    let ok = unsafe {
        WTSEnumerateProcessesExW(
            WTS_CURRENT_SERVER_HANDLE,
            &raw mut level,
            WTS_ANY_SESSION,
            &raw mut info,
            &raw mut count,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let owners = if level == 1 && !info.is_null() {
        // SAFETY: level 1 fills `count` `WTS_PROCESS_INFO_EXW` entries.
        let entries = unsafe {
            std::slice::from_raw_parts(info.cast::<WTS_PROCESS_INFO_EXW>(), count as usize)
        };
        Ok(entries
            .iter()
            .map(|entry| (entry.ProcessId, owner(entry.pUserSid, current)))
            .collect())
    } else {
        Err(io::Error::other("the process listing was not at level 1"))
    };
    // SAFETY: `info` and `count` came from the call above, at level 1.
    unsafe { WTSFreeMemoryExW(WTSTypeProcessInfoLevel1, info.cast::<c_void>(), count) };
    owners
}

fn owner(sid: PSID, current: &str) -> ProcessOwner {
    match sid_string(sid) {
        Ok(sid) if sid == current => ProcessOwner::CurrentUser,
        Ok(sid) if sid == SYSTEM_SID => ProcessOwner::System,
        Ok(sid) => ProcessOwner::OtherUser(UserId::Sid(sid)),
        Err(_) => ProcessOwner::Unknown,
    }
}

/// The `S-1-…` form of a SID.
#[expect(
    unsafe_code,
    reason = "windows-sys exposes ConvertSidToStringSidW as a raw call"
)]
fn sid_string(sid: PSID) -> io::Result<String> {
    if sid.is_null() {
        return Err(io::Error::other("no SID"));
    }
    let mut raw: PWSTR = std::ptr::null_mut();
    // SAFETY: `sid` is a valid SID the OS returned, and `raw` is writable.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success `raw` is a NUL-terminated string from LocalAlloc.
    let text = unsafe { from_wide_nul(raw) };
    // SAFETY: `raw` was allocated by ConvertSidToStringSidW and is freed once.
    unsafe { LocalFree(raw.cast::<c_void>()) };
    text.into_string()
        .map_err(|_| io::Error::other("a SID that isn't Unicode"))
}

/// Only the owner in M2: the other facts aren't read on Windows, so they are unknown.
pub(crate) fn context(pid: u32, owners: &Owners) -> Context {
    Context {
        owner: owners.0.get(&pid).cloned().unwrap_or(ProcessOwner::Unknown),
        cwd: Err(Unread::Unsupported),
        argv: Err(Unread::Unsupported),
        env_paths: Err(Unread::Unsupported),
        mapped_files: Err(Unread::Unsupported),
        mount_namespace: Err(Unread::Unsupported),
        root_dir: Err(Unread::Unsupported),
        root_path: Err(Unread::Unsupported),
        root_entry: Err(Unread::Unsupported),
        capabilities: Err(Unread::Unsupported),
    }
}

#[expect(
    unsafe_code,
    reason = "windows-sys exposes the process APIs as raw calls"
)]
pub(crate) fn probe_pid(pid: u32) -> ProcessOutcome {
    if pid == 0 {
        return ProcessOutcome::NoExecutable;
    }
    // SAFETY: plain call; a null handle reports failure.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return classify(io::Error::last_os_error());
    }
    let process = Process(handle);
    let mut code = 0;
    // SAFETY: the handle is open and `code` is writable.
    let exited = unsafe { GetExitCodeProcess(process.0, &raw mut code) } != 0
        && i32::try_from(code).ok() != Some(STILL_ACTIVE);
    if exited {
        return ProcessOutcome::Exited;
    }
    let mut buf = vec![0_u16; 32 * 1024];
    let mut len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: the handle is open and the buffer holds `len` UTF-16 units.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            buf.as_mut_ptr(),
            &raw mut len,
        )
    };
    if ok == 0 {
        let outcome = unknown(io::Error::last_os_error());
        // The System process (pid 4) has no image file.
        if pid == 4 && matches!(outcome, ProcessOutcome::Other(_)) {
            return ProcessOutcome::NoExecutable;
        }
        return outcome;
    }
    buf.truncate(len as usize);
    let path = PathBuf::from(OsString::from_wide(&buf));
    // Windows refuses to delete a running image, and the reported path follows it when it
    // is renamed (spike S2), so the file at that path is the file running.
    match file_identity(&path) {
        Ok(identity) => ProcessOutcome::Identified {
            path,
            identity,
            deleted: false,
        },
        Err(error) => ProcessOutcome::PathOnly {
            path,
            error: Arc::new(error),
        },
    }
}

/// The outcome of a failed `OpenProcess`. Only there does an invalid parameter prove that
/// the PID no longer exists (spike S2).
fn classify(error: io::Error) -> ProcessOutcome {
    if os_error(&error) == Some(ERROR_INVALID_PARAMETER) {
        return ProcessOutcome::Gone;
    }
    unknown(error)
}

/// The outcome of a failure on a process that is open and running, which says nothing
/// about whether it is gone.
fn unknown(error: io::Error) -> ProcessOutcome {
    if os_error(&error) == Some(ERROR_ACCESS_DENIED) {
        return ProcessOutcome::Denied(Arc::new(error));
    }
    ProcessOutcome::Other(Arc::new(error))
}

fn os_error(error: &io::Error) -> Option<u32> {
    error
        .raw_os_error()
        .and_then(|code| u32::try_from(code).ok())
}

/// `K32EnumProcesses` lists every process to every user.
pub(crate) const fn inventory() -> Inventory {
    Inventory::Complete
}
