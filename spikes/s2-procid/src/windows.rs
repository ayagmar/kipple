//! Windows: `K32EnumProcesses`, `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` and
//! `QueryFullProcessImageNameW`. The identity comes from opening the returned path and
//! reading its volume serial and file index, so it is only as good as the path.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::windows::ffi::OsStringExt as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, STILL_ACTIVE,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, GetFileInformationByHandle,
};
use windows_sys::Win32::System::ProcessStatus::K32EnumProcesses;
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};

use crate::{FileId, Outcome};

#[expect(unsafe_code, reason = "S2 spike: windows-sys exposes raw Win32 calls")]
pub(crate) fn list_pids() -> io::Result<Vec<u32>> {
    let mut pids = vec![0_u32; 1024];
    loop {
        let bytes = u32::try_from(pids.len() * 4).map_err(io::Error::other)?;
        let mut needed = 0;
        // SAFETY: the buffer is valid for `bytes` bytes.
        if unsafe { K32EnumProcesses(pids.as_mut_ptr(), bytes, &raw mut needed) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if needed < bytes {
            pids.truncate(needed as usize / 4);
            return Ok(pids);
        }
        pids.resize(pids.len() * 2, 0);
    }
}

/// Closes the process handle on drop.
struct Process(HANDLE);

impl Drop for Process {
    #[expect(unsafe_code, reason = "S2 spike: closing a handle we opened")]
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

#[expect(unsafe_code, reason = "S2 spike: windows-sys exposes raw Win32 calls")]
pub(crate) fn probe(pid: u32) -> Outcome {
    if pid == 0 {
        return Outcome::NoExecutable("System Idle Process");
    }
    // SAFETY: plain call; a null handle reports failure.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return classify(io::Error::last_os_error());
    }
    let process = Process(handle);
    let mut code = 0;
    // SAFETY: the handle is open.
    if unsafe { GetExitCodeProcess(process.0, &raw mut code) } != 0
        && i32::try_from(code).ok() != Some(STILL_ACTIVE)
    {
        return Outcome::Exited("handle still open");
    }
    let mut buf = vec![0_u16; 32 * 1024];
    let mut len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: the buffer holds `len` UTF-16 units.
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
        if pid == 4 && matches!(outcome, Outcome::Other(_)) {
            return Outcome::NoExecutable("System");
        }
        return outcome;
    }
    buf.truncate(len as usize);
    let path = PathBuf::from(OsString::from_wide(&buf));
    match file_id(&path) {
        Ok(id) => Outcome::Identified {
            path,
            id,
            deleted: false,
        },
        Err(error) => Outcome::PathOnly { path, error },
    }
}

fn classify(error: io::Error) -> Outcome {
    let code = error.raw_os_error().and_then(|c| u32::try_from(c).ok());
    match code {
        Some(ERROR_ACCESS_DENIED) => Outcome::Denied(error),
        Some(ERROR_INVALID_PARAMETER) => Outcome::Gone,
        _ => Outcome::Other(error),
    }
}

#[expect(unsafe_code, reason = "S2 spike: windows-sys exposes raw Win32 calls")]
pub(crate) fn file_id(path: &Path) -> io::Result<FileId> {
    let file = fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    // SAFETY: zeroed is a valid value for this plain C struct.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open and `info` is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileId {
        volume: u64::from(info.dwVolumeSerialNumber),
        file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

pub(crate) const fn inventory_note() -> Option<String> {
    None
}

pub(crate) fn privileged_pids() -> Vec<(u32, &'static str)> {
    vec![(4, "pid 4, System")]
}

pub(crate) const fn diagnostics() -> Option<String> {
    None
}
