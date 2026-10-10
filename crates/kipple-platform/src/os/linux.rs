//! Linux: `/proc/<pid>/exe` (D-035). `readlink` gives the path, with the kernel's
//! ` (deleted)` marker, and `stat` through the magic link gives the identity and link
//! count of the file the process is actually running, even after it was replaced.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kipple_core::{Inventory, ProbeError, ProcessOutcome, Roots};

pub(crate) use super::unix::{
    KIPPLE_DIRS, allocated, device, env_name_matches, hard_link, identity, is_executable,
    process_env,
};
use super::unix::{home, identity_of, xdg_roots};
use crate::environment::Environment;

pub(crate) const PERMISSION_HINT: Option<&str> = None;

/// `ESRCH`: the process no longer exists.
const ESRCH: i32 = 3;

/// `PF_KTHREAD` in the flags field of `/proc/<pid>/stat`.
const PF_KTHREAD: u64 = 0x0020_0000;

pub(crate) fn resolve_roots(env: &Environment) -> Roots {
    xdg_roots(env, &home(env))
}

pub(crate) fn list_pids() -> Result<Vec<u32>, ProbeError> {
    let proc = Path::new("/proc");
    let entries = fs::read_dir(proc).map_err(|e| ProbeError::io(proc, e))?;
    let mut pids = Vec::new();
    for entry in entries {
        let name = entry.map_err(|e| ProbeError::io(proc, e))?.file_name();
        if let Some(pid) = name.to_str().and_then(|name| name.parse().ok()) {
            pids.push(pid);
        }
    }
    Ok(pids)
}

pub(crate) fn probe_pid(pid: u32) -> ProcessOutcome {
    let link = PathBuf::from(format!("/proc/{pid}/exe"));
    let path = match fs::read_link(&link) {
        Ok(path) => path,
        Err(error) => return classify(pid, error),
    };
    match fs::metadata(&link) {
        Ok(meta) => ProcessOutcome::Identified {
            path,
            identity: identity_of(&meta),
            deleted: meta.nlink() == 0,
        },
        Err(stat_error) => {
            let outcome = classify(pid, stat_error);
            if let ProcessOutcome::Other(error) = outcome {
                ProcessOutcome::PathOnly { path, error }
            } else {
                outcome
            }
        }
    }
}

/// Only a missing process entry means the process went away. Anything else that is not
/// a zombie or a kernel thread stays unknown.
fn classify(pid: u32, error: io::Error) -> ProcessOutcome {
    if error.kind() == io::ErrorKind::PermissionDenied {
        return ProcessOutcome::Denied(Arc::new(error));
    }
    if error.raw_os_error() == Some(ESRCH) {
        return ProcessOutcome::Gone;
    }
    if error.kind() != io::ErrorKind::NotFound {
        return ProcessOutcome::Other(Arc::new(error));
    }
    // `exe` is missing: a kernel thread, a zombie, or the process just went away.
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(stat) if stat.kind() == io::ErrorKind::NotFound => ProcessOutcome::Gone,
        Err(stat) if stat.raw_os_error() == Some(ESRCH) => ProcessOutcome::Gone,
        Err(_) => ProcessOutcome::Other(Arc::new(error)),
        Ok(stat) => match stat_state(&stat) {
            Some(StatState::Exited) => ProcessOutcome::Exited,
            Some(StatState::KernelThread) => ProcessOutcome::NoExecutable,
            None => ProcessOutcome::Other(Arc::new(error)),
        },
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StatState {
    Exited,
    KernelThread,
}

/// A zombie or kernel thread, from `/proc/<pid>/stat`. The command name in parentheses
/// may itself contain `) `, so fields are counted after the last `)`.
fn stat_state(stat: &str) -> Option<StatState> {
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace();
    if matches!(fields.next()?, "Z" | "X") {
        return Some(StatState::Exited);
    }
    // After the state: ppid, pgrp, session, tty_nr, tpgid, then flags.
    let flags: u64 = fields.nth(5)?.parse().ok()?;
    (flags & PF_KTHREAD != 0).then_some(StatState::KernelThread)
}

pub(crate) fn inventory() -> Inventory {
    match fs::read_to_string("/proc/self/mountinfo") {
        Ok(mountinfo) => proc_visibility(&mountinfo),
        Err(error) => Inventory::Partial {
            reason: format!("/proc/self/mountinfo could not be read: {error}"),
        },
    }
}

/// Whether the `/proc` this process sees lists every process. With `hidepid` or `subset`
/// other users' processes can be missing without any error, so the inventory is partial.
fn proc_visibility(mountinfo: &str) -> Inventory {
    // A later mount on /proc hides the earlier ones.
    let Some(line) = mountinfo
        .lines()
        .rfind(|line| line.split_whitespace().nth(4) == Some("/proc"))
    else {
        return Inventory::Partial {
            reason: "no /proc mount was found".to_owned(),
        };
    };
    let super_options = line
        .split_once(" - ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(2))
        .unwrap_or_default();
    super_options
        .split(',')
        .find(|option| hides_processes(option))
        .map_or(Inventory::Complete, |option| Inventory::Partial {
            reason: format!("/proc is mounted with {option}, which can hide processes"),
        })
}

fn hides_processes(option: &str) -> bool {
    match option.split_once('=') {
        Some(("hidepid", value)) => !matches!(value, "0" | "off"),
        Some(("subset", _)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: &str = "\
22 1 0:21 / /sys rw,nosuid shared:7 - sysfs sysfs rw
23 1 0:22 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc rw
24 23 0:23 / /proc/sys/fs/binfmt_misc rw,relatime shared:13 - autofs systemd-1 rw,fd=29
";

    fn remounted(options: &str) -> String {
        format!("{PLAIN}90 1 0:60 / /proc rw,relatime shared:40 - proc proc rw,{options}\n")
    }

    fn partial(inventory: &Inventory) -> bool {
        matches!(inventory, Inventory::Partial { .. })
    }

    #[test]
    fn a_plain_proc_mount_lists_every_process() {
        assert_eq!(proc_visibility(PLAIN), Inventory::Complete);
        assert_eq!(
            proc_visibility(&remounted("hidepid=off")),
            Inventory::Complete
        );
    }

    #[test]
    fn hidepid_or_subset_on_the_visible_proc_mount_makes_the_inventory_partial() {
        for options in [
            "hidepid=invisible",
            "hidepid=2",
            "hidepid=ptraceable",
            "subset=pid",
        ] {
            assert!(partial(&proc_visibility(&remounted(options))), "{options}");
        }
        assert!(partial(&proc_visibility("")));
    }

    #[test]
    fn a_zombie_or_kernel_thread_is_recognised_whatever_its_command_name() {
        let zombie = "4242 (evil) R 1 (x) Z 1 4242 4242 0 -1 4194560 0 0";
        assert_eq!(stat_state(zombie), Some(StatState::Exited));
        let kthread = "2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0";
        assert_eq!(stat_state(kthread), Some(StatState::KernelThread));
        // A process named like a zombie is still running and still unknown.
        let sleeping = "77 (Z) Z) S 1 77 77 0 -1 4194560 0 0";
        assert_eq!(stat_state(sleeping), None);
    }
}
