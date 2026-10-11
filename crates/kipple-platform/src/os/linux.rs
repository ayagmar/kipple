//! Linux: `/proc/<pid>/exe` (D-035). `readlink` gives the path, with the kernel's
//! ` (deleted)` marker, and `stat` through the magic link gives the identity and link
//! count of the file the process is actually running, even after it was replaced.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use kipple_core::{
    Inventory, Platform, ProbeError, ProcessOutcome, ResolvedRoot, RootSource, Roots,
    SettingProblem, SymbolicRoot, UnresolvedRoot,
};

pub(crate) use super::unix::{
    KIPPLE_DIRS, allocated, device, env_name_matches, hard_link, identity, is_executable, physical,
    process_env, raw_bytes,
};
use super::unix::{identity_of, xdg_roots};
use crate::environment::Environment;
use crate::roots::{in_physical_parent, under};

pub(crate) const PLATFORM: Platform = Platform::Linux;

pub(crate) const PERMISSION_HINT: Option<&str> = None;

/// pacman's config file, which may set `CacheDir`.
const PACMAN_CONF: &str = "/etc/pacman.conf";

/// pacman's package cache when `CacheDir` is not set.
const PACMAN_CACHE: &str = "/var/cache/pacman/pkg";

/// systemd-journald's persistent journal.
const JOURNAL: &str = "/var/log/journal";

/// `ESRCH`: the process no longer exists.
const ESRCH: i32 = 3;

/// `PF_KTHREAD` in the flags field of `/proc/<pid>/stat`.
const PF_KTHREAD: u64 = 0x0020_0000;

/// The XDG roots, the freedesktop home trash, pacman's cache and the journal.
pub(crate) fn resolve_roots(env: &Environment) -> Roots {
    let mut roots = xdg_roots(env);
    // The freedesktop home trash keeps the trashed files themselves in `files`, beside
    // `info` and the size cache, which it keeps even when emptied.
    let trash = under(
        &roots,
        SymbolicRoot::XdgData,
        SymbolicRoot::Trash,
        "Trash/files",
    );
    roots.insert(SymbolicRoot::Trash, trash);
    let conf = fs::read_to_string(env.system_path(Path::new(PACMAN_CONF)));
    for resolved in pacman_cache(env, conf) {
        roots.add(SymbolicRoot::PacmanCache, resolved);
    }
    let journal = env.system_path(Path::new(JOURNAL));
    let journal = ResolvedRoot::new(SymbolicRoot::Journal, journal, RootSource::Fixed);
    roots.insert(SymbolicRoot::Journal, Ok(journal));
    roots
}

/// pacman's cache directories, from the `CacheDir` settings in `conf`. None when there
/// is no `pacman.conf`, since then there is no pacman.
fn pacman_cache(
    env: &Environment,
    conf: io::Result<String>,
) -> Vec<Result<ResolvedRoot, UnresolvedRoot>> {
    let root = SymbolicRoot::PacmanCache;
    let unusable = |problem| {
        vec![Err(UnresolvedRoot::Setting {
            file: PACMAN_CONF,
            problem,
        })]
    };
    let text = match conf {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => return unusable(SettingProblem::Unreadable(Arc::new(error))),
    };
    let Some(dirs) = cache_dirs(&text) else {
        return unusable(SettingProblem::Includes);
    };
    if dirs.is_empty() {
        let path = env.system_path(Path::new(PACMAN_CACHE));
        return vec![Ok(ResolvedRoot::new(root, path, RootSource::Fixed))];
    }
    let source = RootSource::Setting {
        file: PACMAN_CONF,
        key: "CacheDir",
    };
    dirs.into_iter()
        .map(|dir| {
            let path = Path::new(dir);
            if path.is_absolute() {
                let path = without_parent_components(env.system_path(path))?;
                Ok(ResolvedRoot::new(root, path, source))
            } else {
                Err(UnresolvedRoot::NotAbsolute {
                    var: "CacheDir",
                    value: dir.to_owned(),
                })
            }
        })
        .collect()
}

/// `path`, placed in the directory its parent names when it climbs with `..`, since
/// nothing below a root may have a `..` component, like a `--root`.
fn without_parent_components(path: PathBuf) -> Result<PathBuf, UnresolvedRoot> {
    if !path.components().any(|c| c == Component::ParentDir) {
        return Ok(path);
    }
    in_physical_parent(&path).map_err(|e| UnresolvedRoot::Unresolvable {
        path,
        source: Arc::new(e),
    })
}

/// Every `CacheDir` value in the `[options]` section, in order. `None` when that section
/// includes another file, which may set it too.
fn cache_dirs(text: &str) -> Option<Vec<&str>> {
    let mut section = "";
    let mut dirs = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim();
            continue;
        }
        let (key, value) = line.split_once('=').unwrap_or((line, ""));
        match (section, key.trim()) {
            ("options", "CacheDir") => dirs.extend(value.split_whitespace()),
            ("options", "Include") => return None,
            _ => {}
        }
    }
    Some(dirs)
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

    fn cache(conf: &str) -> Vec<String> {
        pacman_cache(&Environment::default(), Ok(conf.to_owned()))
            .into_iter()
            .map(|resolved| match resolved {
                Ok(resolved) => resolved.path.display().to_string(),
                Err(unresolved) => unresolved.to_string(),
            })
            .collect()
    }

    #[test]
    fn pacman_uses_every_cachedir_of_its_options_and_its_default_without_one() {
        let set = "[options]\nCacheDir = /srv/pkg/ /mnt/pkg # two\n#CacheDir = /old\n\
                   CacheDir=/third\n[core]\nCacheDir = /not-an-option\n";
        assert_eq!(cache(set), ["/srv/pkg", "/mnt/pkg", "/third"]);
        assert_eq!(cache("[options]\nHoldPkg = pacman\n"), [PACMAN_CACHE]);
        assert!(
            pacman_cache(&Environment::default(), Err(io::ErrorKind::NotFound.into())).is_empty()
        );
    }

    #[test]
    fn an_include_in_pacman_options_leaves_the_cache_unknown_rather_than_guessed() {
        let included = "[options]\nInclude = /etc/pacman.d/options\n";
        let [unknown] = cache(included).try_into().unwrap();
        assert!(unknown.contains("includes other files"), "{unknown}");
        let [denied] = pacman_cache(
            &Environment::default(),
            Err(io::ErrorKind::PermissionDenied.into()),
        )
        .try_into()
        .unwrap();
        assert!(denied.is_err());
    }

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
