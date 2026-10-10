//! Linux: `/proc/<pid>/exe`. `readlink` gives the path (with the kernel's " (deleted)"
//! marker) and `stat` through the magic link gives the identity and link count of the
//! file the process is actually running, even after it was deleted or replaced.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use crate::{FileId, Outcome};

/// `PF_KTHREAD` in `/proc/<pid>/stat` field 9.
const PF_KTHREAD: u64 = 0x0020_0000;
const ESRCH: i32 = 3;

pub(crate) fn list_pids() -> io::Result<Vec<u32>> {
    let mut pids = Vec::new();
    for entry in fs::read_dir("/proc")? {
        if let Some(pid) = entry?.file_name().to_str().and_then(|n| n.parse().ok()) {
            pids.push(pid);
        }
    }
    Ok(pids)
}

pub(crate) fn probe(pid: u32) -> Outcome {
    let link = PathBuf::from(format!("/proc/{pid}/exe"));
    let path = match fs::read_link(&link) {
        Ok(path) => path,
        Err(error) => return classify(pid, error),
    };
    match fs::metadata(&link) {
        Ok(meta) => Outcome::Identified {
            deleted: meta.nlink() == 0,
            path,
            id: id_of(&meta),
        },
        Err(stat_error) => {
            let outcome = classify(pid, stat_error);
            if let Outcome::Other(error) = outcome {
                Outcome::PathOnly { path, error }
            } else {
                outcome
            }
        }
    }
}

fn classify(pid: u32, error: io::Error) -> Outcome {
    if error.kind() == io::ErrorKind::PermissionDenied {
        return Outcome::Denied(error);
    }
    if error.raw_os_error() == Some(ESRCH) {
        return Outcome::Gone;
    }
    if error.kind() != io::ErrorKind::NotFound {
        return Outcome::Other(error);
    }
    // `exe` is missing: a kernel thread, a zombie, or the process just went away.
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return Outcome::Gone;
    };
    // Fields after the parenthesised command name: state is first, flags is seventh.
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map_or(Vec::new(), |(_, rest)| rest.split_whitespace().collect());
    let flags = fields.get(6).and_then(|f| f.parse::<u64>().ok());
    match (fields.first().copied(), flags) {
        (Some("Z" | "X"), _) => Outcome::Exited("zombie"),
        (_, Some(flags)) if flags & PF_KTHREAD != 0 => Outcome::NoExecutable("kernel thread"),
        _ => Outcome::Other(error),
    }
}

fn id_of(meta: &fs::Metadata) -> FileId {
    FileId {
        volume: meta.dev(),
        file: meta.ino(),
    }
}

pub(crate) fn file_id(path: &Path) -> io::Result<FileId> {
    fs::metadata(path).map(|meta| id_of(&meta))
}

/// Whether `/proc` hides other users' processes, which makes the inventory partial.
pub(crate) fn inventory_note() -> Option<String> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").ok()?;
    let proc_line = mountinfo
        .lines()
        .find(|line| line.split_whitespace().nth(4) == Some("/proc"))?;
    let options = proc_line.rsplit(' ').next().unwrap_or_default();
    let hiding = options
        .split(',')
        .find(|o| o.starts_with("hidepid=") || o.starts_with("subset="));
    Some(hiding.map_or_else(
        || format!("/proc mount options {options}: all processes visible"),
        |option| format!("/proc mounted with {option}: other users' processes may be invisible"),
    ))
}

pub(crate) fn privileged_pids() -> Vec<(u32, &'static str)> {
    vec![(1, "pid 1, init")]
}
