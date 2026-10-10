//! Windows: Known Folders for roots, and `K32EnumProcesses`,
//! `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` and `QueryFullProcessImageNameW` for
//! processes (D-035). File identity is the volume serial and file index of an open handle.

use std::ffi::{OsStr, OsString, c_void};
use std::fs;
use std::io;
use std::os::windows::ffi::OsStringExt as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kipple_core::{
    FileIdentity, Inventory, ProbeError, ProcessOutcome, ResolvedRoot, RootSource, Roots,
    SymbolicRoot, UnresolvedRoot,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, STILL_ACTIVE,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    GetFileInformationByHandle,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::ProcessStatus::K32EnumProcesses;
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_Profile, FOLDERID_RoamingAppData, SHGetKnownFolderPath,
};
use windows_sys::core::{GUID, PWSTR};

use crate::environment::Environment;

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

pub(crate) fn env_name_matches(key: &OsStr, name: &str) -> bool {
    key.eq_ignore_ascii_case(name)
}

pub(crate) fn resolve_roots(_: &Environment) -> Roots {
    let mut roots = Roots::default();
    for (root, id, folder) in KNOWN_FOLDERS {
        let resolved = known_folder(id)
            .map(|path| ResolvedRoot {
                root,
                path,
                source: RootSource::KnownFolder(folder),
            })
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
        let mut len = 0;
        // SAFETY: on success `raw` is a NUL-terminated UTF-16 string.
        while unsafe { *raw.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `raw` holds `len` initialised units before the NUL.
        let wide = unsafe { std::slice::from_raw_parts(raw, len) };
        Ok(PathBuf::from(OsString::from_wide(wide)))
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
pub(crate) const fn hard_link(_: &fs::Metadata) -> Option<(FileIdentity, u64)> {
    None
}

/// Windows runs a file by its extension, which the search already requires.
pub(crate) const fn is_executable(_: &fs::Metadata) -> bool {
    true
}

/// Processes fail in odd ways without `SystemRoot`.
pub(crate) fn process_env(env: &Environment) -> Vec<(&'static str, OsString)> {
    env.get("SystemRoot")
        .map(|root| ("SystemRoot", root.to_owned()))
        .into_iter()
        .collect()
}

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

/// Closes a process handle on drop.
struct Process(HANDLE);

impl Drop for Process {
    #[expect(unsafe_code, reason = "closing a handle this module opened")]
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess and is closed exactly once.
        unsafe { CloseHandle(self.0) };
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
        let outcome = classify(io::Error::last_os_error());
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

fn classify(error: io::Error) -> ProcessOutcome {
    match error
        .raw_os_error()
        .and_then(|code| u32::try_from(code).ok())
    {
        Some(ERROR_ACCESS_DENIED) => ProcessOutcome::Denied(Arc::new(error)),
        Some(ERROR_INVALID_PARAMETER) => ProcessOutcome::Gone,
        _ => ProcessOutcome::Other(Arc::new(error)),
    }
}

/// `K32EnumProcesses` lists every process to every user.
pub(crate) const fn inventory() -> Inventory {
    Inventory::Complete
}
