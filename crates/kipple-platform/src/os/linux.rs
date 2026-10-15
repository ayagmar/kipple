//! Linux: `/proc/<pid>/exe` (D-035). `readlink` gives the path, with the kernel's
//! ` (deleted)` marker, and `stat` through the magic link gives the identity and link
//! count of the file the process is actually running, even after it was replaced. The
//! rest of `/proc/<pid>` gives its owner and what else it can be seen using.

use std::ffi::{CStr, OsStr};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::{mem, ptr};

use kipple_core::{
    Capabilities, Inventory, MountNamespace, Observer, Platform, ProbeError, Probed,
    ProcessOutcome, ProcessOwner, ResolvedRoot, RootSource, Roots, SettingProblem, SymbolicRoot,
    UnresolvedRoot,
};

pub(crate) use super::unix::{
    KIPPLE_DIRS, allocated, device, dir_access, env_name_matches, hard_link, identity,
    is_executable, physical, process_env, raw_bytes,
};
use super::unix::{env_paths, identity_of, nul_separated, owner, path_args, unread, xdg_roots};
use crate::environment::Environment;
use crate::process::Context;
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
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"));
        return denied(error, stat.as_deref().ok());
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

/// A process whose `exe` was refused. A reader who isn't root is refused a kernel
/// thread's like any root process's, though it runs no program; its `stat`, which anyone
/// can read, tells them apart. Anything short of the kernel-thread flag stays denied.
fn denied(error: io::Error, stat: Option<&str>) -> ProcessOutcome {
    match stat.and_then(stat_state) {
        Some(StatState::KernelThread) => ProcessOutcome::NoExecutable,
        _ => ProcessOutcome::Denied(Arc::new(error)),
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

/// Who kipple runs as, to tell the current user's processes from others'.
pub(crate) struct Owners {
    current: Option<u32>,
}

impl Owners {
    pub(crate) fn read() -> Self {
        let status = fs::read_to_string("/proc/self/status").ok();
        Self {
            current: status.as_deref().and_then(single_uid),
        }
    }
}

/// How kipple itself sees files: its own mount namespace and root directory. Linux shows
/// every user's command line in `/proc/<pid>/cmdline`.
pub(crate) fn observer() -> Observer {
    Observer {
        mount_namespace: fs::read_link("/proc/self/ns/mnt")
            .map_err(unread)
            .and_then(|link| mount_namespace(&link)),
        root_dir: fs::metadata("/proc/self/root")
            .map(|meta| identity_of(&meta))
            .map_err(unread),
        command_lines_public: true,
    }
}

/// Everything `/proc/<pid>` shows about a process besides its executable. `cmdline` and
/// `status` are readable even for root's processes; the rest usually only for the
/// current user's.
///
/// The owner comes from the `Uid:` line of `status`, not from the owner of `/proc/<pid>`,
/// which the kernel makes root for a process that isn't dumpable, such as a setuid or
/// setcap program.
pub(crate) fn context(pid: u32, owners: &Owners) -> Context {
    let path = |name: &str| PathBuf::from(format!("/proc/{pid}/{name}"));
    let file = |name: &str| fs::read(path(name)).map_err(unread);
    let status = fs::read_to_string(path("status")).map_err(unread);
    Context {
        owner: status.as_ref().map_or(ProcessOwner::Unknown, |status| {
            status_owner(status, owners.current)
        }),
        cwd: fs::read_link(path("cwd")).map_err(unread),
        argv: file("cmdline").map(|bytes| path_args(&nul_separated(&bytes))),
        env_paths: file("environ").map(|bytes| env_paths(&nul_separated(&bytes))),
        mapped_files: file("maps").map(|bytes| mapped_files(&bytes)),
        mount_namespace: fs::read_link(path("ns/mnt"))
            .map_err(unread)
            .and_then(|link| mount_namespace(&link)),
        root_dir: fs::metadata(path("root"))
            .map(|meta| identity_of(&meta))
            .map_err(unread),
        root_path: fs::read_link(path("root")).map_err(unread),
        root_entry: Ok(path("root")),
        capabilities: status.and_then(|status| effective_caps(&status)),
    }
}

/// The real, effective, saved and filesystem user IDs of a `status`.
fn status_uids(status: &str) -> Option<Vec<u32>> {
    let uids = status_field::<4>(status, "Uid:", 10)?;
    uids.into_iter()
        .map(|uid| u32::try_from(uid).ok())
        .collect()
}

/// The owner a `status` names, as seen by the user whose ID is `current`.
fn status_owner(status: &str, current: Option<u32>) -> ProcessOwner {
    owner(status_uids(status).as_deref(), current)
}

/// The one user ID of a `status` whose four IDs agree.
fn single_uid(status: &str) -> Option<u32> {
    let uids = status_uids(status)?;
    let (first, rest) = uids.split_first()?;
    rest.iter().all(|uid| uid == first).then_some(*first)
}

fn effective_caps(status: &str) -> Probed<Capabilities> {
    status_field::<1>(status, "CapEff:", 16)
        .map(|[caps]| Capabilities(caps))
        .ok_or_else(|| unread(malformed("CapEff")))
}

fn malformed(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("unexpected {what} in /proc"),
    )
}

/// The `N` numbers of a `status` line, in `radix`.
fn status_field<const N: usize>(status: &str, name: &str, radix: u32) -> Option<[u64; N]> {
    let line = status.lines().find_map(|line| line.strip_prefix(name))?;
    let numbers: Vec<u64> = line
        .split_whitespace()
        .map(|n| u64::from_str_radix(n, radix))
        .collect::<Result<_, _>>()
        .ok()?;
    numbers.try_into().ok()
}

/// The namespace an `ns/mnt` link such as `mnt:[4026531841]` names.
fn mount_namespace(link: &Path) -> Probed<MountNamespace> {
    link.to_str()
        .and_then(|link| link.strip_prefix("mnt:[")?.strip_suffix(']')?.parse().ok())
        .map(MountNamespace)
        .ok_or_else(|| unread(malformed("ns/mnt")))
}

/// The files a `/proc/<pid>/maps` lists, sorted, once each. The kernel writes a newline
/// in a path as `\012`; such a path is kept both as written and with the newline.
fn mapped_files(maps: &[u8]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for line in maps.split(|&b| b == b'\n') {
        let Some(path) = map_path(line) else {
            continue;
        };
        if let Some(unescaped) = with_newlines(path) {
            files.push(PathBuf::from(OsStr::from_bytes(&unescaped)));
        }
        files.push(PathBuf::from(OsStr::from_bytes(path)));
    }
    files.sort();
    files.dedup();
    files
}

/// `path` with each `\012` turned back into a newline, if it has any.
fn with_newlines(path: &[u8]) -> Option<Vec<u8>> {
    const ESCAPE: &[u8] = b"\\012";
    let mut out = Vec::with_capacity(path.len());
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix(ESCAPE) {
            out.push(b'\n');
            rest = after;
        } else if let Some((&first, after)) = rest.split_first() {
            out.push(first);
            rest = after;
        }
    }
    (out.len() != path.len()).then_some(out)
}

/// The path of one `maps` line: whatever follows its first five fields, when it is a
/// file rather than a label such as `[heap]`.
fn map_path(line: &[u8]) -> Option<&[u8]> {
    let mut rest = line;
    for _ in 0..5 {
        let start = rest.iter().position(|b| !b.is_ascii_whitespace())?;
        let field = rest.get(start..)?;
        let end = field
            .iter()
            .position(u8::is_ascii_whitespace)
            .unwrap_or(field.len());
        rest = field.get(end..)?;
    }
    let path = rest.trim_ascii_start();
    path.starts_with(b"/").then_some(path)
}

/// The extended attribute holding a POSIX access control list.
const POSIX_ACL: &CStr = c"system.posix_acl_access";

/// The `statfs` magic numbers of network and user-space file systems: NFS, SMB, CIFS,
/// SMB2, AFS, Ceph, FUSE, 9p, Coda and NCP.
const NETWORK_FILE_SYSTEMS: [i64; 10] = [
    0x6969,
    0x517B,
    0xFF53_4D42,
    0xFE53_4D42,
    0x5346_414F,
    0x00C3_6400,
    0x6573_5546,
    0x0102_1997,
    0x7375_7245,
    0x564C,
];

/// Whether the entry at `path` carries a POSIX access control list. Its absence, or a file
/// system without them, means none.
#[expect(unsafe_code, reason = "libc's lgetxattr is a raw call")]
pub(crate) fn grants_acl(path: &CStr) -> io::Result<bool> {
    // SAFETY: both strings are NUL-terminated, and a null buffer of size 0 only asks for
    // the value's size.
    let size = unsafe { libc::lgetxattr(path.as_ptr(), POSIX_ACL.as_ptr(), ptr::null_mut(), 0) };
    if size >= 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENODATA | libc::ENOTSUP) => Ok(false),
        _ => Err(error),
    }
}

/// Whether `path` is on a network or user-space file system.
#[expect(unsafe_code, reason = "libc's statfs is a raw call")]
pub(crate) fn on_network(path: &CStr) -> io::Result<bool> {
    let mut stat = mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the path is NUL-terminated and `stat` is large enough for the result.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `statfs` succeeded, so it filled `stat`.
    let kind = magic(unsafe { stat.assume_init() }.f_type);
    Ok(NETWORK_FILE_SYSTEMS.contains(&kind))
}

/// A `statfs` magic number as its 32 bits, whatever integer type this target gives it.
fn magic(f_type: impl Into<i64>) -> i64 {
    f_type.into() & 0xFFFF_FFFF
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
    use kipple_core::UserId;

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

    /// A `status` with these `Uid:` and `CapEff:` lines, as the kernel writes them.
    fn status(uids: &str, caps: &str) -> String {
        format!(
            "Name:\tping\nUmask:\t0022\nState:\tS (sleeping)\nTgid:\t4242\nPid:\t4242\n\
             PPid:\t1\nUid:\t{uids}\nGid:\t1000\t1000\t1000\t1000\nCapInh:\t0000000000000000\n\
             CapPrm:\t{caps}\nCapEff:\t{caps}\nCapBnd:\t000001ffffffffff\n"
        )
    }

    const NO_CAPS: &str = "0000000000000000";

    #[test]
    fn a_process_owner_is_named_only_when_all_four_user_ids_agree() {
        let me = Some(1000);
        let owner_of = |uids| status_owner(&status(uids, NO_CAPS), me);

        assert_eq!(
            owner_of("1000\t1000\t1000\t1000"),
            ProcessOwner::CurrentUser
        );
        assert_eq!(owner_of("0\t0\t0\t0"), ProcessOwner::Root);
        assert_eq!(
            owner_of("1001\t1001\t1001\t1001"),
            ProcessOwner::OtherUser(UserId::Uid(1001))
        );
        // A setuid-root helper started by the user, such as fusermount3, has root's rights.
        for elevated in ["1000\t0\t0\t0", "0\t0\t1000\t0"] {
            assert_eq!(owner_of(elevated), ProcessOwner::Elevated, "{elevated}");
        }
        // Any other mix: the user's effective ID with root's file system ID, or a third
        // account among them.
        for mixed in ["1000\t1000\t1000\t0", "1001\t0\t0\t0", "1000\t0\t1001\t0"] {
            assert_eq!(owner_of(mixed), ProcessOwner::Unknown, "{mixed}");
        }
        assert_eq!(
            status_owner("Name:\tx\nUid:\t1000\t1000\n", me),
            ProcessOwner::Unknown
        );
        assert_eq!(status_owner("Name:\tx\n", me), ProcessOwner::Unknown);
    }

    #[test]
    fn a_setcap_program_of_the_current_user_is_theirs_though_its_proc_entry_is_roots() {
        // The kernel makes `/proc/<pid>` root's for a process that isn't dumpable, as
        // with this capability-wrapped `ping`. Its status still names the user.
        let ping = status("1000\t1000\t1000\t1000", "0000000000002000");

        assert_eq!(status_owner(&ping, Some(1000)), ProcessOwner::CurrentUser);
        assert_eq!(effective_caps(&ping).unwrap(), Capabilities(0x2000));
        assert!(effective_caps("Name:\tx\n").is_err());
    }

    #[test]
    fn without_a_consistent_current_user_no_owner_is_named() {
        let me = single_uid(&status("1000\t0\t1000\t1000", NO_CAPS));
        assert_eq!(me, None);

        for uids in ["1000\t1000\t1000\t1000", "0\t0\t0\t0"] {
            assert_eq!(
                status_owner(&status(uids, NO_CAPS), me),
                ProcessOwner::Unknown
            );
        }
        assert_eq!(
            single_uid(&status("1000\t1000\t1000\t1000", NO_CAPS)),
            Some(1000)
        );
    }

    #[test]
    fn maps_lists_each_mapped_file_once_with_spaces_kept_and_labels_left_out() {
        let maps = b"\
55d0c0a00000-55d0c0a28000 r--p 00000000 103:02 1311 /home/u/p/.venv/bin/python3.13
7f1c2a000000-7f1c2a021000 rw-p 00000000 00:00 0 
7f1c2a200000-7f1c2a300000 r-xp 00002000 103:02 2290 /home/u/my proj/lib/_x.so
7f1c2a300000-7f1c2a310000 r--p 00102000 103:02 2290 /home/u/my proj/lib/_x.so
7f1c2a400000-7f1c2a410000 r--p 00000000 103:02 2291 /home/u/p/old.so (deleted)
7f1c2a500000-7f1c2a510000 r--p 00000000 103:02 2292 /home/u/odd\\012name.so
7ffd1a1d4000-7ffd1a1f5000 rw-p 00000000 00:00 0                          [stack]
";

        let files = mapped_files(maps);

        let expected = [
            "/home/u/my proj/lib/_x.so",
            "/home/u/odd\nname.so",
            "/home/u/odd\\012name.so",
            "/home/u/p/.venv/bin/python3.13",
            "/home/u/p/old.so (deleted)",
        ];
        assert_eq!(files, expected.map(PathBuf::from));
    }

    #[test]
    fn a_mount_namespace_is_read_from_its_link_and_anything_else_is_unknown() {
        assert_eq!(
            mount_namespace(Path::new("mnt:[4026531841]")).unwrap(),
            MountNamespace(4_026_531_841)
        );
        for odd in ["net:[4026531840]", "mnt:[x]", "mnt:4026531841"] {
            assert!(mount_namespace(Path::new(odd)).is_err(), "{odd}");
        }
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

    /// A refused `exe`, and what `stat` gave.
    fn refused(stat: Option<&str>) -> ProcessOutcome {
        denied(io::ErrorKind::PermissionDenied.into(), stat)
    }

    #[test]
    fn a_kernel_thread_whose_exe_is_refused_runs_no_program() {
        let kworker = "8 (kworker/0:0H-events_highpri) I 2 0 0 0 -1 69238880 0 0 0 0";

        assert!(matches!(
            refused(Some(kworker)),
            ProcessOutcome::NoExecutable
        ));
    }

    #[test]
    fn a_refused_process_stays_denied_unless_its_stat_shows_a_kernel_thread() {
        // A `stat` that couldn't be read.
        assert!(matches!(refused(None), ProcessOutcome::Denied(_)));
        for stat in [
            // A root daemon: its flags lack the kernel-thread bit.
            "812 (sshd) S 1 812 812 0 -1 4194560 0 0 0 0",
            // A name that spells out a kernel thread's fields before the real ones.
            "900 (x) S 0 0 0 0 -1 2129984 ) S 1 900 900 0 -1 4194560 0 0",
            // Cut short before the flags, or with flags that aren't a number.
            "900 (sshd) S 1 812 812 0 -1",
            "900 (sshd) S 1 812 812 0 -1 0x200000 0",
            "",
        ] {
            assert!(
                matches!(refused(Some(stat)), ProcessOutcome::Denied(_)),
                "{stat}"
            );
        }
    }
}
