//! Which processes show a project artifact in use, and which count when they can't be
//! inspected (`docs/04-safety-model.md` §4, D-050), through `Engine::scan` with injected
//! process snapshots and file systems.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use kipple_core::{
    Activity, ActivityEvidence, ActivityGap, Apparent, CancelToken, Capabilities, Category, Clock,
    Completeness, Consequence, Cost, DirAccess, DirEntryMeta, Engine, EntryKind, EntryMeta,
    EnvPath, EventChannel, Evidence, FileIdentity, FileSystem, FindingSink, FsProbe, Integration,
    IntegrationDescriptor, IntegrationError, IntegrationId, Inventory, Lens, Measurement, Method,
    MountNamespace, Observer, Operation, Platform, Ports, ProbeError, Probed, ProcessOutcome,
    ProcessOwner, ProcessProbe, ProcessRecord, ProcessSnapshot, Recovery, ResolvedRoot, RootLink,
    RootSource, Roots, Rule, RuleId, RuleText, ScanContext, ScanReport, ScanRequest, Seen,
    Selector, Sizer, SpaceEstimate, SymbolicRoot, Unread, UserId, census, event_channel,
};

/// `path` as the host takes it: one starting with `/` is made absolute, since Windows takes
/// a path without a drive as relative, so each test means the same thing on every OS.
/// Anything else is kept as it is.
fn abs(path: &str) -> PathBuf {
    if path.starts_with('/') && !Path::new("/").is_absolute() {
        PathBuf::from(format!("C:{path}"))
    } else {
        PathBuf::from(path)
    }
}

const HOME: &str = "/home/u";
/// A project root in the home directory.
const CODE: &str = "/home/u/code";
/// A project root outside it.
const SRV: &str = "/srv/code";
/// A project root configured in the home directory as a link to a directory outside it.
const LINKED: &str = "/home/u/linked";
const LINKED_TARGET: &str = "/mnt/data/code";

const VENV: &str = "/home/u/code/app/.venv";
const TARGET: &str = "/home/u/code/app/target";

const ME: u32 = 1000;
const OUR_NAMESPACE: MountNamespace = MountNamespace(4_026_531_841);

const PYTHON: ActivityEvidence = ActivityEvidence::Interpreted {
    runtimes: &["python*"],
};
const NODE: ActivityEvidence = ActivityEvidence::Interpreted {
    runtimes: &["node", "bun", "deno"],
};
const CARGO: ActivityEvidence = ActivityEvidence::BuiltExecutables;

/// A disk where every path is a directory with an identity derived from where it really
/// is: a path below a link is read through the link, as the OS does for every component
/// but the last, and a link itself has its own identity.
#[derive(Debug, Default, Clone)]
struct Disk {
    /// Each link and the absolute path it leads to.
    links: Vec<(PathBuf, PathBuf)>,
    /// Each bind mount and the directory it shows, which it has the identity of.
    binds: Vec<(PathBuf, PathBuf)>,
    /// Who may enter each directory; any other can't be read.
    access: BTreeMap<PathBuf, DirAccess>,
    /// Paths that can't be read, nor anything below them.
    denied: Vec<PathBuf>,
    /// Paths that are links, with nothing below them.
    symlinks: Vec<PathBuf>,
}

impl Disk {
    fn identity(&self, path: &Path) -> FileIdentity {
        let mut hasher = DefaultHasher::new();
        let mut real = path.to_owned();
        for (link, target) in &self.links {
            match path.strip_prefix(link) {
                Ok(rest) if rest.as_os_str().is_empty() => "link".hash(&mut hasher),
                Ok(rest) => real = target.join(rest),
                Err(_) => {}
            }
        }
        for (bind, source) in &self.binds {
            if let Ok(rest) = path.strip_prefix(bind) {
                real = source.join(rest);
            }
        }
        real.hash(&mut hasher);
        FileIdentity {
            volume: 1,
            file: hasher.finish(),
        }
    }

    /// Every directory from `dir` up to the home directory, private to the current user
    /// or open to everyone.
    fn with_home(mut self, private: bool) -> Self {
        for dir in [HOME, CODE, "/home/u/code/app", VENV, TARGET] {
            let access = DirAccess {
                owned_by_current_user: true,
                open_to_others: !(private && dir == HOME),
                acl: false,
                file_system: FileSystem::Local,
            };
            self.access.insert(abs(dir), access);
        }
        self
    }
}

impl FsProbe for Disk {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        if path.ends_with(".git") {
            return Err(ProbeError::io(path, io::ErrorKind::NotFound.into()));
        }
        if self.denied.iter().any(|denied| path.starts_with(denied)) {
            return Err(ProbeError::io(path, io::ErrorKind::PermissionDenied.into()));
        }
        Ok(EntryMeta {
            kind: if self.symlinks.iter().any(|link| link == path) {
                EntryKind::Symlink
            } else {
                EntryKind::Dir
            },
            identity: Some(self.identity(path)),
        })
    }

    fn read_dir(&self, _: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        Ok(Vec::new())
    }

    fn open_dir(&self, _: &Path) -> Result<(), ProbeError> {
        Ok(())
    }

    fn read_file(&self, path: &Path, _: u64) -> Result<Vec<u8>, ProbeError> {
        Err(ProbeError::io(path, io::ErrorKind::NotFound.into()))
    }

    fn access(&self, dir: &Path) -> Result<DirAccess, ProbeError> {
        self.access
            .get(dir)
            .copied()
            .ok_or_else(|| ProbeError::io(dir, io::ErrorKind::PermissionDenied.into()))
    }
}

#[derive(Debug)]
struct CompleteSizer;

impl Sizer for CompleteSizer {
    fn size(&self, _: &Path, _: &CancelToken) -> Result<Measurement, ProbeError> {
        Ok(Measurement {
            size: SpaceEstimate {
                apparent: Apparent(10),
                allocated: None,
                unique_reclaim: None,
                completeness: Completeness::Complete,
            },
            modified: Some(SystemTime::UNIX_EPOCH),
            repository: None,
        })
    }
}

#[derive(Debug)]
struct Epoch;

impl Clock for Epoch {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
}

#[derive(Debug)]
struct Snapshot(ProcessSnapshot);

impl ProcessProbe for Snapshot {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        Ok(self.0.clone())
    }
}

/// Proposes one project artifact, with a marker beside it, under whichever project root
/// holds it.
#[derive(Debug)]
struct Artifact {
    evidence: ActivityEvidence,
    path: PathBuf,
}

fn rule() -> Rule {
    let text = |s: &str| s.to_owned();
    Rule {
        id: RuleId("builtin.test.artifact".to_owned()),
        integration: IntegrationId("test"),
        text: RuleText {
            title: text("Test"),
            description: String::new(),
            why: text("why"),
            loss: text("loss"),
            references: Vec::new(),
        },
        category: Category::Cache,
        platforms: Platform::ALL.to_vec(),
        root: SymbolicRoot::ProjectRoots,
        selector: Selector::Integration("test"),
        requirements: Vec::new(),
        eligibility: Vec::new(),
        operation: Operation::RemoveFiles(vec![Method::Delete]),
        recent_days: None,
    }
}

impl Integration for Artifact {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("test"),
            lens: Lens::Dev,
            platforms: Platform::ALL,
            roots: &[],
            selectors: &[],
            artifacts: &[],
            owner_files: &[],
            verified: &[],
        }
    }

    fn activity_evidence(&self) -> ActivityEvidence {
        self.evidence
    }

    fn discover(
        &self,
        _: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let evidence = vec![
            Evidence::Marker("pyproject.toml".to_owned()),
            Evidence::Signature("TAG".to_owned()),
        ];
        let consequence = Consequence {
            recovery: Recovery::Reinstall,
            loss: &[],
            cost: Cost::Low,
        };
        sink.add(&rule(), &self.path, evidence, Some(consequence))
    }
}

/// One scan's setup: where home is, the disk, and the processes.
#[derive(Debug, Clone)]
struct Host {
    home: Option<&'static str>,
    disk: Disk,
    observer: Observer,
    inventory: Inventory,
    processes: Vec<ProcessRecord>,
}

impl Host {
    /// A Linux-like host with a private home directory, running these processes.
    fn new(processes: Vec<ProcessRecord>) -> Self {
        Self {
            home: Some(HOME),
            disk: Disk::default().with_home(true),
            observer: Observer {
                mount_namespace: Ok(OUR_NAMESPACE),
                root_dir: Ok(Disk::default().identity(&abs("/"))),
                command_lines_public: true,
                process_paths: true,
            },
            inventory: Inventory::Complete,
            processes,
        }
    }

    fn roots(&self) -> Roots {
        let mut roots = Roots::default();
        if let Some(home) = self.home {
            let home = ResolvedRoot::new(SymbolicRoot::Home, abs(home), RootSource::Env("HOME"));
            roots.insert(SymbolicRoot::Home, Ok(home));
        }
        for root in [CODE, SRV] {
            let resolved = ResolvedRoot::new(
                SymbolicRoot::ProjectRoots,
                abs(root),
                RootSource::CommandLine,
            );
            roots.add(SymbolicRoot::ProjectRoots, Ok(resolved));
        }
        let linked = ResolvedRoot {
            link: Some(RootLink {
                configured: abs(LINKED),
                identity: self.disk.identity(&abs(LINKED_TARGET)),
            }),
            ..ResolvedRoot::new(
                SymbolicRoot::ProjectRoots,
                abs(LINKED_TARGET),
                RootSource::CommandLine,
            )
        };
        roots.add(SymbolicRoot::ProjectRoots, Ok(linked));
        roots
    }

    #[expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "a scan that can't be set up fails the test"
    )]
    fn scan(&self, evidence: ActivityEvidence, path: &str) -> (Activity, ScanReport) {
        let ports = Ports {
            fs: Box::new(self.disk.clone()),
            sizer: Box::new(CompleteSizer),
            processes: Box::new(Snapshot(ProcessSnapshot {
                processes: self.processes.clone(),
                inventory: self.inventory.clone(),
                observer: self.observer.clone(),
            })),
            clock: Box::new(Epoch),
        };
        let artifact = Artifact {
            evidence,
            path: abs(path),
        };
        let engine = Engine::new(ports, vec![Box::new(artifact)], vec![rule()]).unwrap();
        let request = ScanRequest {
            roots: self.roots(),
            lens: None,
            exclude: Vec::new(),
        };
        let cancel = CancelToken::new();
        let EventChannel { tx, rx } = event_channel(1024, &cancel);
        let drain = std::thread::spawn(move || rx.into_iter().count());
        let report = engine.scan(&request, &tx, &cancel);
        drop(tx);
        drain.join().unwrap();
        let [finding] = report.findings.as_slice() else {
            panic!("one finding, got {:?}", report.findings);
        };
        let activity = finding.facts.clone().unwrap().activity;
        (activity, report)
    }

    fn activity(&self, evidence: ActivityEvidence, path: &str) -> Activity {
        self.scan(evidence, path).0
    }
}

fn failed() -> Unread {
    Unread::Failed(Arc::new(io::ErrorKind::PermissionDenied.into()))
}

fn os(values: &[&str]) -> Vec<OsString> {
    values
        .iter()
        .map(|value| abs(value).into_os_string())
        .collect()
}

/// A process of `owner` running `executable`, with every fact read: working in `/`, with
/// no path arguments, environment paths or mapped files, in kipple's mount namespace.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn inspected(pid: u32, owner: ProcessOwner, executable: &str) -> ProcessRecord {
    let outcome = ProcessOutcome::Identified {
        path: abs(executable),
        identity: FileIdentity {
            volume: 1,
            file: u64::from(pid),
        },
        deleted: false,
    };
    let name = Path::new(executable).file_name().unwrap();
    ProcessRecord {
        owner,
        cwd: Ok(abs("/")),
        argv: Ok(vec![name.to_owned()]),
        env_paths: Ok(Vec::new()),
        mapped_files: Ok(Vec::new()),
        mount_namespace: Ok(OUR_NAMESPACE),
        root_dir: Ok(Disk::default().identity(&abs("/"))),
        capabilities: Ok(Capabilities(0)),
        ..ProcessRecord::new(pid, outcome)
    }
}

/// A process of `owner` that couldn't be inspected, as Linux shows another user's: its
/// executable, working directory, environment, mapped files and mount namespace are
/// denied, and its command line is `argv`.
fn uninspectable(pid: u32, owner: ProcessOwner, argv: &[&str]) -> ProcessRecord {
    let denied = ProcessOutcome::Denied(Arc::new(io::ErrorKind::PermissionDenied.into()));
    ProcessRecord {
        owner,
        argv: Ok(os(argv)),
        cwd: Err(failed()),
        env_paths: Err(failed()),
        mapped_files: Err(failed()),
        mount_namespace: Err(failed()),
        root_dir: Err(failed()),
        capabilities: Ok(Capabilities(0)),
        ..ProcessRecord::new(pid, denied)
    }
}

const fn other_user() -> ProcessOwner {
    ProcessOwner::OtherUser(UserId::Uid(ME + 1))
}

const fn seen(activity: &Activity) -> Option<&Seen> {
    if let Activity::InUse { seen, .. } = activity {
        Some(seen)
    } else {
        None
    }
}

const fn is_unknown(activity: &Activity) -> bool {
    matches!(activity, Activity::Unknown(_))
}

#[test]
fn an_uninspectable_process_of_the_current_user_is_judged_by_its_command_line_on_linux() {
    // Their own non-dumpable programs, such as ssh-agent, show only their command line.
    let agent = || uninspectable(7, ProcessOwner::CurrentUser, &["/usr/bin/ssh-agent", "-D"]);
    let host = Host::new(vec![agent()]);

    for (evidence, path) in [(CARGO, TARGET), (PYTHON, VENV)] {
        let (activity, report) = host.scan(evidence, path);
        assert!(
            matches!(
                activity,
                Activity::IdleExceptSystem {
                    unchecked: 0,
                    by_command_line: 1
                }
            ),
            "{path}: {activity:?}"
        );
        assert_eq!(report.by_command_line, 1);
    }
    // Outside the home directory every uninspected process still counts.
    assert!(is_unknown(&host.activity(PYTHON, "/srv/code/app/.venv")));
    // macOS doesn't show other users' command lines, so the rule stays off there.
    let mut macos = host;
    macos.observer = Observer::PLAIN;
    assert!(is_unknown(&macos.activity(CARGO, TARGET)));
}

#[test]
fn an_own_process_judged_by_its_command_line_holds_what_it_names() {
    let built = "/home/u/code/app/target/release/x";
    let host = Host::new(vec![uninspectable(7, ProcessOwner::CurrentUser, &[built])]);

    assert_eq!(
        seen(&host.activity(CARGO, TARGET)),
        Some(&Seen::Argument(abs(built)))
    );
}

#[test]
fn an_own_process_that_runs_a_runtime_or_hides_its_command_line_always_counts() {
    // An interpreter runs a project's files without naming them, and a relative path
    // can't be placed while the working directory can't be read.
    let unread = {
        let mut process = uninspectable(7, ProcessOwner::CurrentUser, &[]);
        process.argv = Err(failed());
        process
    };
    let mut elsewhere = uninspectable(7, ProcessOwner::CurrentUser, &["/usr/bin/x"]);
    elsewhere.mount_namespace = Ok(MountNamespace(4_026_532_999));
    let cases = [
        (
            "python3 x.py",
            uninspectable(7, ProcessOwner::CurrentUser, &["python3", "x.py"]),
        ),
        (
            "node",
            uninspectable(7, ProcessOwner::CurrentUser, &["/usr/bin/node"]),
        ),
        (
            "relative",
            uninspectable(7, ProcessOwner::CurrentUser, &["./target/x"]),
        ),
        ("unread", unread),
        ("another mount namespace", elsewhere),
    ];
    for (name, process) in cases {
        let host = Host::new(vec![process]);
        assert!(
            matches!(
                host.activity(CARGO, TARGET),
                Activity::Unknown(ActivityGap::Uninspected(1))
            ),
            "{name}"
        );
    }
}

#[test]
fn an_unread_environment_of_the_current_user_blocks_an_interpreted_layout_only() {
    // `/usr/bin/python3` shows a virtual environment only through what it was given, so
    // each of those must be read; a built executable shows only by being run.
    let mut python = inspected(7, ProcessOwner::CurrentUser, "/usr/bin/python3");
    python.env_paths = Err(failed());
    let host = Host::new(vec![python]);

    assert!(is_unknown(&host.activity(PYTHON, VENV)));
    assert!(matches!(host.activity(CARGO, TARGET), Activity::Idle));
}

#[test]
fn a_venv_is_in_use_through_virtual_env_path_an_argv0_link_or_a_mapped_library() {
    let python = || inspected(7, ProcessOwner::CurrentUser, "/usr/bin/python3.14");
    let env = |var: &str, path: &str| EnvPath {
        var: var.into(),
        path: abs(path),
    };
    let mut virtual_env = python();
    virtual_env.env_paths = Ok(vec![env("VIRTUAL_ENV", VENV)]);
    let mut path = python();
    path.env_paths = Ok(vec![
        env("PATH", "/home/u/code/app/.venv/bin"),
        env("PATH", "/usr/bin"),
    ]);
    // `.venv/bin/python` is a link to the system interpreter, which is what runs.
    let mut argv0 = python();
    argv0.argv = Ok(os(&["/home/u/code/app/.venv/bin/python", "manage.py"]));
    let mut mapped = python();
    let library = "/home/u/code/app/.venv/lib/python3.14/site-packages/_x.so";
    mapped.mapped_files = Ok(vec![abs("/usr/lib/libc.so.6"), abs(library)]);

    let cases = [
        (virtual_env, Seen::Environment(abs(VENV))),
        // The bare program name it was started by is found on its own `PATH` first.
        (
            path,
            Seen::Argument(abs("/home/u/code/app/.venv/bin/python3.14")),
        ),
        (
            argv0,
            Seen::Argument(abs("/home/u/code/app/.venv/bin/python")),
        ),
        (mapped, Seen::MappedFile(abs(library))),
    ];
    for (process, expected) in cases {
        let activity = Host::new(vec![process]).activity(PYTHON, VENV);
        assert_eq!(seen(&activity), Some(&expected));
    }
    assert!(matches!(
        Host::new(vec![python()]).activity(PYTHON, VENV),
        Activity::Idle
    ));
}

#[test]
fn a_node_script_under_node_modules_on_the_command_line_is_in_use_relative_or_not() {
    let modules = "/home/u/code/app/node_modules";
    let node = |argv: &[&str]| {
        let mut node = inspected(7, ProcessOwner::CurrentUser, "/usr/bin/node");
        node.cwd = Ok(abs("/home/u/code/app"));
        node.argv = Ok(os(argv));
        node
    };

    let absolute = node(&["node", "/home/u/code/app/node_modules/.bin/vite"]);
    let relative = node(&["node", "node_modules/.bin/vite"]);

    assert_eq!(
        seen(&Host::new(vec![absolute]).activity(NODE, modules)),
        Some(&Seen::Argument(abs(
            "/home/u/code/app/node_modules/.bin/vite"
        )))
    );
    assert_eq!(
        seen(&Host::new(vec![relative]).activity(NODE, modules)),
        Some(&Seen::Argument(abs(
            "/home/u/code/app/node_modules/.bin/vite"
        )))
    );
}

#[test]
fn a_runtime_working_in_the_project_uses_its_interpreted_layout() {
    // `node server.js` run from the project shows nothing inside `node_modules`.
    let modules = "/home/u/code/app/node_modules";
    let in_project = |executable| {
        let mut process = inspected(7, ProcessOwner::CurrentUser, executable);
        process.cwd = Ok(abs("/home/u/code/app/src"));
        process.argv = Ok(os(&["node", "server.js"]));
        process
    };

    let node = Host::new(vec![in_project("/usr/bin/node")]);
    assert_eq!(
        seen(&node.activity(NODE, modules)),
        Some(&Seen::Runtime(abs("/home/u/code/app/src")))
    );
    // The name only adds holds where a runtime is declared: a shell there uses nothing,
    // and neither does a runtime for a built layout.
    let mut shell = in_project("/usr/bin/bash");
    shell.argv = Ok(os(&["bash"]));
    assert!(matches!(
        Host::new(vec![shell]).activity(NODE, modules),
        Activity::Idle
    ));
    assert!(matches!(node.activity(CARGO, TARGET), Activity::Idle));
}

#[test]
fn an_uninspectable_root_process_is_left_out_and_counted_only_for_an_artifact_in_home() {
    let root = || uninspectable(1, ProcessOwner::Root, &["/usr/lib/systemd/systemd"]);
    let host = Host::new(vec![root()]);

    let (activity, report) = host.scan(PYTHON, VENV);
    assert!(matches!(
        activity,
        Activity::IdleExceptSystem {
            unchecked: 1,
            by_command_line: 0
        }
    ));
    assert_eq!(report.unchecked_system, 1);

    let outside = host.scan(PYTHON, "/srv/code/app/.venv");
    assert!(is_unknown(&outside.0));
    assert_eq!(outside.1.unchecked_system, 0);
    // Configured in the home directory, but the root leads out of it.
    assert!(is_unknown(
        &host.activity(PYTHON, "/mnt/data/code/app/.venv")
    ));
    // `SYSTEM` is left out the same way, on Windows, which shows no command line.
    let mut system = Host::new(vec![uninspectable(4, ProcessOwner::System, &[])]);
    system.observer = Observer::PLAIN;
    system.processes[0].argv = Err(Unread::Unsupported);
    assert!(matches!(
        system.activity(CARGO, TARGET),
        Activity::IdleExceptSystem {
            unchecked: 1,
            by_command_line: 0
        }
    ));
}

#[test]
fn without_a_known_home_below_the_file_system_root_no_root_process_is_left_out() {
    let mut unknown = Host::new(vec![uninspectable(1, ProcessOwner::Root, &["/sbin/init"])]);
    unknown.home = None;
    let mut slash = unknown.clone();
    slash.home = Some("/");

    assert!(is_unknown(&unknown.activity(PYTHON, VENV)));
    assert!(is_unknown(&slash.activity(PYTHON, VENV)));
}

#[test]
fn a_root_command_line_path_inside_the_artifact_marks_it_in_use_even_for_a_built_layout() {
    // `sudo /home/u/code/app/target/release/x`: root runs a program built there.
    let built = "/home/u/code/app/target/release/x";
    let host = Host::new(vec![uninspectable(9, ProcessOwner::Root, &[built])]);

    assert_eq!(
        seen(&host.activity(CARGO, TARGET)),
        Some(&Seen::Argument(abs(built)))
    );
}

#[test]
fn a_relative_root_command_line_path_with_an_unreadable_working_directory_blocks() {
    // `sudo ./target/release/x` keeps the path as typed, and root's working directory
    // can't be read, so it might be inside the artifact.
    let host = Host::new(vec![uninspectable(
        9,
        ProcessOwner::Root,
        &["./target/release/x"],
    )]);

    assert!(matches!(
        host.activity(CARGO, TARGET),
        Activity::Unknown(ActivityGap::Uninspected(1))
    ));
}

#[test]
fn an_unread_root_command_line_blocks_where_the_os_shows_it_and_not_where_it_hides_it() {
    let mut root = uninspectable(1, ProcessOwner::Root, &[]);
    root.argv = Err(failed());
    let linux = Host::new(vec![root]);
    let mut macos = linux.clone();
    macos.observer = Observer::PLAIN;

    assert!(is_unknown(&linux.activity(CARGO, TARGET)));
    assert!(matches!(
        macos.activity(CARGO, TARGET),
        Activity::IdleExceptSystem {
            unchecked: 1,
            by_command_line: 0
        }
    ));
}

#[test]
fn another_users_process_counts_unless_a_private_directory_above_shuts_it_out() {
    let process = || uninspectable(3, other_user(), &["/usr/bin/colord"]);
    let private = Host::new(vec![process()]);
    let mut open = private.clone();
    open.disk = Disk::default().with_home(false);
    let mut acl = private.clone();
    let home = acl.disk.access.get_mut(&abs(HOME)).unwrap();
    home.acl = true;
    let mut remote = private.clone();
    let venv = remote.disk.access.get_mut(&abs(VENV)).unwrap();
    venv.file_system = FileSystem::Remote;
    let mut unreadable = private.clone();
    unreadable.disk.access.remove(&abs(HOME));
    let mut caps_unread = private.clone();
    caps_unread.processes[0].capabilities = Err(failed());

    assert!(matches!(private.activity(PYTHON, VENV), Activity::Idle));
    for (name, host) in [
        ("0750 home", open),
        ("acl", acl),
        ("network", remote),
        ("access unread", unreadable),
        ("caps unread", caps_unread),
    ] {
        assert!(is_unknown(&host.activity(PYTHON, VENV)), "{name}");
    }
    assert!(is_unknown(&private.activity(PYTHON, "/srv/code/app/.venv")));
}

#[test]
fn a_process_of_unknown_owner_or_a_partial_inventory_blocks() {
    // Its user IDs couldn't be read, or mix another account in.
    let mixed = Host::new(vec![uninspectable(
        5,
        ProcessOwner::Unknown,
        &["/usr/bin/fusermount3"],
    )]);
    let mut partial = Host::new(Vec::new());
    partial.inventory = Inventory::Partial {
        reason: "hidepid=2".to_owned(),
    };

    assert!(is_unknown(&mixed.activity(CARGO, TARGET)));
    assert!(matches!(
        partial.activity(CARGO, TARGET),
        Activity::Unknown(ActivityGap::Partial(_))
    ));
}

#[test]
fn a_process_in_another_mount_namespace_whose_root_cant_be_read_is_never_read_as_idle() {
    // A devcontainer names the project `/app`: its paths can't be compared with ours, and
    // its root directory isn't there to read them through.
    let mut container = inspected(7, ProcessOwner::CurrentUser, "/usr/local/bin/python3");
    container.cwd = Ok(abs("/app"));
    container.argv = Ok(os(&["python3", "/app/.venv/bin/flask"]));
    container.mount_namespace = Ok(MountNamespace(4_026_532_999));
    let host = Host::new(vec![container.clone()]);
    let mut unread = container;
    unread.mount_namespace = Err(failed());
    let mut ours_unread = Host::new(vec![inspected(7, ProcessOwner::CurrentUser, "/usr/bin/x")]);
    ours_unread.observer.mount_namespace = Err(failed());

    assert!(is_unknown(&host.activity(PYTHON, VENV)));
    assert!(is_unknown(&host.activity(CARGO, TARGET)));
    assert!(is_unknown(&Host::new(vec![unread]).activity(PYTHON, VENV)));
    assert!(is_unknown(&ours_unread.activity(PYTHON, VENV)));
}

#[test]
fn a_process_path_matches_ignoring_case_and_through_a_linked_parent() {
    // The root was typed `/home/u/Code`, while the process names the real case.
    let mut typed = inspected(7, ProcessOwner::CurrentUser, "/usr/bin/python3");
    typed.env_paths = Ok(vec![EnvPath {
        var: "VIRTUAL_ENV".into(),
        path: abs("/home/u/code/app/.venv"),
    }]);
    let case = Host::new(vec![typed]);
    assert!(seen(&case.activity(PYTHON, "/home/u/code/APP/.venv")).is_some());

    // `/var/home -> home`: the process names the project through the link.
    let mut through_link = inspected(7, ProcessOwner::CurrentUser, "/usr/bin/python3");
    through_link.cwd = Ok(abs("/var/home/u/code/app/.venv/lib"));
    let mut linked = Host::new(vec![through_link]);
    linked.disk.links = vec![(abs("/var/home"), abs("/home"))];
    assert_eq!(
        seen(&linked.activity(PYTHON, VENV)),
        Some(&Seen::WorkingDirectory(abs(
            "/var/home/u/code/app/.venv/lib"
        )))
    );
}

#[test]
fn processes_that_exited_or_run_no_program_file_are_not_counted() {
    let mut host = Host::new(Vec::new());
    for (pid, outcome) in [
        (2, ProcessOutcome::NoExecutable),
        (3, ProcessOutcome::Exited),
        (4, ProcessOutcome::Gone),
    ] {
        host.processes.push(ProcessRecord {
            owner: ProcessOwner::CurrentUser,
            ..ProcessRecord::new(pid, outcome)
        });
    }

    assert!(matches!(host.activity(PYTHON, VENV), Activity::Idle));
}

#[test]
fn an_integration_that_declares_nothing_counts_every_process() {
    let host = Host::new(vec![uninspectable(1, ProcessOwner::Root, &["/sbin/init"])]);

    assert!(is_unknown(
        &host.activity(ActivityEvidence::Everything, VENV)
    ));
}

/// Where kipple reaches the root of the sandboxed process 7, and the directory it is.
const SANDBOX_ENTRY: &str = "/proc/7/root";
const SANDBOX: &str = "/sandbox";

/// A disk with process 7's root at `/sandbox`, and the project bound into it at `/app`.
fn sandbox_disk() -> Disk {
    Disk {
        binds: vec![
            (abs(SANDBOX_ENTRY), abs(SANDBOX)),
            (abs("/proc/7/root/app"), abs("/home/u/code/app")),
        ],
        ..Disk::default().with_home(true)
    }
}

/// Process 7 of the current user, in another mount namespace with its root at
/// `/sandbox`, running `executable` as it names it: every fact read, nothing else shown.
fn sandboxed(disk: &Disk, executable: &str) -> ProcessRecord {
    let below = format!("{SANDBOX_ENTRY}{executable}");
    ProcessRecord {
        outcome: ProcessOutcome::Identified {
            path: abs(executable),
            identity: disk.identity(&abs(&below)),
            deleted: false,
        },
        mount_namespace: Ok(MountNamespace(4_026_532_999)),
        root_dir: Ok(disk.identity(&abs(SANDBOX))),
        root_entry: Ok(abs(SANDBOX_ENTRY)),
        ..inspected(7, ProcessOwner::CurrentUser, executable)
    }
}

fn sandbox_host(process: ProcessRecord, disk: Disk) -> Host {
    Host {
        disk,
        ..Host::new(vec![process])
    }
}

#[test]
fn a_sandboxed_process_shows_the_project_through_its_root_by_identity() {
    let disk = sandbox_disk();
    let python = || sandboxed(&disk, "/usr/bin/python3");
    let mut cwd = python();
    cwd.cwd = Ok(abs("/app/.venv/lib"));
    let mut argument = python();
    argument.argv = Ok(os(&["python3", "/app/.venv/bin/flask"]));
    let mut environment = python();
    environment.env_paths = Ok(vec![EnvPath {
        var: "VIRTUAL_ENV".into(),
        path: abs("/app/.venv"),
    }]);
    let mut mapped = python();
    mapped.mapped_files = Ok(vec![abs("/app/.venv/lib/_x.so")]);
    let executable = sandboxed(&disk, "/app/.venv/bin/python");

    let cases = [
        (cwd, Seen::WorkingDirectory(abs("/app/.venv/lib"))),
        (argument, Seen::Argument(abs("/app/.venv/bin/flask"))),
        (environment, Seen::Environment(abs("/app/.venv"))),
        (mapped, Seen::MappedFile(abs("/app/.venv/lib/_x.so"))),
        (executable, Seen::Executable(abs("/app/.venv/bin/python"))),
    ];
    for (process, expected) in cases {
        let activity = sandbox_host(process, disk.clone()).activity(PYTHON, VENV);
        assert_eq!(seen(&activity), Some(&expected));
    }
    // Everything it shows was resolved there, and none of it is the project's.
    assert!(matches!(
        sandbox_host(python(), disk.clone()).activity(PYTHON, VENV),
        Activity::Idle
    ));
}

#[test]
fn a_sandboxed_path_that_cant_be_resolved_leaves_its_kind_uninspected() {
    let mut disk = sandbox_disk();
    disk.denied = vec![abs("/proc/7/root/srv")];
    disk.symlinks = vec![abs("/proc/7/root/data")];
    let with_argument = |argument: &str| {
        let mut process = sandboxed(&disk, "/usr/bin/python3");
        process.argv = Ok(os(&["python3", argument]));
        sandbox_host(process, disk.clone())
    };

    // Through a directory that can't be read, or a link, which kipple would follow from
    // its own root rather than the process's.
    for host in [
        with_argument("/srv/app/x.py"),
        with_argument("/data/app/x.py"),
    ] {
        assert!(is_unknown(&host.activity(PYTHON, VENV)));
        // A built layout needs only the executable, which resolved.
        assert!(matches!(host.activity(CARGO, TARGET), Activity::Idle));
    }
}

#[test]
fn a_sandboxed_path_ending_in_a_link_to_the_finding_is_never_idle() {
    // `VIRTUAL_ENV=/venv` inside the container is a link to the host's `.venv`; kipple
    // would follow it from its own root, so the environment can't be placed.
    let mut disk = sandbox_disk();
    disk.symlinks = vec![abs("/proc/7/root/venv")];
    let mut process = sandboxed(&disk, "/usr/bin/python3");
    process.env_paths = Ok(vec![EnvPath {
        var: "VIRTUAL_ENV".into(),
        path: abs("/venv"),
    }]);
    let host = sandbox_host(process, disk);

    assert!(is_unknown(&host.activity(PYTHON, VENV)));
}

#[test]
fn a_sandboxed_process_whose_root_or_executable_cant_be_placed_stays_uninspected() {
    let disk = sandbox_disk();
    let python = || sandboxed(&disk, "/usr/bin/python3");
    let mut other_file = python();
    other_file.outcome = ProcessOutcome::Identified {
        path: abs("/usr/bin/python3"),
        identity: FileIdentity { volume: 9, file: 9 },
        deleted: false,
    };
    let mut entry_unread = python();
    entry_unread.root_entry = Err(failed());
    let mut root_unread = python();
    root_unread.root_dir = Err(failed());

    for (name, process) in [
        ("executable is another file there", other_file),
        ("no way into its root", entry_unread),
        ("root unread", root_unread),
    ] {
        let host = sandbox_host(process, disk.clone());
        assert!(is_unknown(&host.activity(CARGO, TARGET)), "{name}");
    }
}

#[test]
fn a_chrooted_process_in_our_namespace_names_its_own_paths_below_its_root() {
    // The kernel shows its executable, working directory and mapped files as kipple
    // names them; what it names itself is below its root.
    let jailed = |root: Probed<PathBuf>, argv: &[&str]| {
        let mut process = inspected(7, ProcessOwner::CurrentUser, "/usr/bin/python3");
        process.root_dir = Err(failed());
        process.root_path = root;
        process.argv = Ok(os(argv));
        Host::new(vec![process])
    };

    let jail = jailed(
        Ok(abs("/home/u/code")),
        &["python3", "/app/.venv/bin/flask"],
    );
    assert_eq!(
        seen(&jail.activity(PYTHON, VENV)),
        Some(&Seen::Argument(abs("/home/u/code/app/.venv/bin/flask")))
    );
    // A browser's renderer is shut in a directory that is gone, and names nothing there.
    let gone = jailed(Ok(abs("/proc/4154/fdinfo")), &["/opt/browser/browser"]);
    assert!(matches!(gone.activity(PYTHON, VENV), Activity::Idle));
    let unread = jailed(Err(failed()), &["/opt/browser/browser"]);
    assert!(is_unknown(&unread.activity(PYTHON, VENV)));
}

#[test]
fn an_option_value_is_judged_as_the_path_it_holds() {
    let root = |argv: &[String]| {
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        Host::new(vec![uninspectable(9, ProcessOwner::Root, &argv)])
    };
    let option = |name: &str, path: &str| format!("{name}={}", abs(path).display());
    let daemon = abs("/usr/sbin/tailscaled").display().to_string();
    let state = "/home/u/code/app/target/state";

    let inside = root(&[daemon.clone(), option("--state", state)]);
    assert_eq!(
        seen(&inside.activity(CARGO, TARGET)),
        Some(&Seen::Argument(abs(state)))
    );
    let outside = root(&[daemon.clone(), option("--socket", "/run/tailscale/s.sock")]);
    assert!(matches!(
        outside.activity(CARGO, TARGET),
        Activity::IdleExceptSystem {
            unchecked: 1,
            by_command_line: 0
        }
    ));
    // A relative value can't be placed while root's working directory can't be read.
    let relative = root(&[daemon, "--config=conf/x.toml".to_owned()]);
    assert!(is_unknown(&relative.activity(CARGO, TARGET)));
}

#[test]
fn a_rewritten_process_title_is_judged_word_by_word() {
    // sshd rewrites its command line into one string with spaces.
    let title = |text: String| Host::new(vec![uninspectable(9, ProcessOwner::Root, &[&text])]);
    let sshd = abs("/usr/bin/sshd").display().to_string();
    let listener = title(format!("sshd: {sshd} -D [listener] 0 of 10-100 startups"));
    assert!(matches!(
        listener.activity(CARGO, TARGET),
        Activity::IdleExceptSystem {
            unchecked: 1,
            by_command_line: 0
        }
    ));
    let built = abs("/home/u/code/app/target/release/x");
    let naming = title(format!("worker {}", built.display()));
    assert_eq!(
        seen(&naming.activity(CARGO, TARGET)),
        Some(&Seen::Argument(built))
    );
    // A relative path with a space reads as relative whole and word by word, and one
    // relative word is enough to leave a title unplaced.
    let spaced = title("./my dir/x".to_owned());
    assert!(is_unknown(&spaced.activity(CARGO, TARGET)));
    let mixed = title(format!("worker {sshd} conf/x.toml"));
    assert!(is_unknown(&mixed.activity(CARGO, TARGET)));
}

#[test]
fn doctor_counts_the_own_processes_judged_by_their_command_line() {
    let snapshot = ProcessSnapshot {
        processes: vec![
            uninspectable(7, ProcessOwner::CurrentUser, &["/usr/bin/ssh-agent"]),
            uninspectable(8, ProcessOwner::CurrentUser, &["python3", "x.py"]),
            uninspectable(9, ProcessOwner::Root, &["/usr/bin/sshd"]),
            uninspectable(10, ProcessOwner::Elevated, &["/usr/bin/fusermount3"]),
            uninspectable(11, ProcessOwner::Elevated, &["./helper"]),
            ProcessRecord {
                capabilities: Err(failed()),
                ..uninspectable(12, other_user(), &["/usr/bin/colord"])
            },
            uninspectable(13, other_user(), &["/usr/bin/colord"]),
        ],
        inventory: Inventory::Complete,
        observer: Host::new(Vec::new()).observer,
    };

    let census = census(&snapshot, &Disk::default());

    assert_eq!(census.by_command_line, 1);
    assert_eq!(census.unchecked_system, 2);
    // Another user's process whose capabilities couldn't be read counts for every one;
    // one that can't read past the mode bits depends on the artifact's directories.
    assert_eq!(census.counted, 3);
}

#[test]
fn a_sandboxed_runtime_working_in_the_project_uses_its_interpreted_layout() {
    // A devcontainer's `node server.js`, working in the project it has bound at `/app`.
    let disk = sandbox_disk();
    let mut node = sandboxed(&disk, "/usr/bin/node");
    node.cwd = Ok(abs("/app"));
    node.argv = Ok(os(&["node", "server.js"]));

    let host = sandbox_host(node, disk);

    assert_eq!(
        seen(&host.activity(NODE, "/home/u/code/app/node_modules")),
        Some(&Seen::Runtime(abs("/app")))
    );
}

#[test]
fn an_own_process_whose_title_starts_with_a_runtime_counts() {
    let title = uninspectable(
        7,
        ProcessOwner::CurrentUser,
        &["node server.js --port 3000"],
    );

    assert!(is_unknown(&Host::new(vec![title]).activity(CARGO, TARGET)));
}

/// The two kinds of process with root's reach that aren't root's: a setuid-root helper
/// the user started, and another user's daemon that reads past the mode bits, with an
/// open home so that only its reach matters.
fn with_roots_reach(argv: &[&str]) -> [(&'static str, Host); 2] {
    let elevated = uninspectable(5, ProcessOwner::Elevated, argv);
    let mut capable = uninspectable(6, other_user(), argv);
    capable.capabilities = Ok(Capabilities(1 << 2));
    [elevated, capable].map(|process| {
        let name = if process.pid == 5 {
            "elevated"
        } else {
            "capable"
        };
        let mut host = Host::new(vec![process]);
        host.disk = Disk::default().with_home(false);
        (name, host)
    })
}

#[test]
fn a_process_with_roots_reach_is_left_out_like_roots_when_its_command_line_is_placed() {
    for (name, host) in with_roots_reach(&["/usr/bin/fusermount3"]) {
        let (activity, report) = host.scan(CARGO, TARGET);
        assert!(
            matches!(
                activity,
                Activity::IdleExceptSystem {
                    unchecked: 1,
                    by_command_line: 0
                }
            ),
            "{name}: {activity:?}"
        );
        assert_eq!(report.unchecked_system, 1, "{name}");
    }
    let built = "/home/u/code/app/target/release/x";
    for (name, host) in with_roots_reach(&[built]) {
        let activity = host.activity(CARGO, TARGET);
        assert_eq!(seen(&activity), Some(&Seen::Argument(abs(built))), "{name}");
    }
}

#[test]
fn a_process_with_roots_reach_counts_when_its_command_line_cant_clear_it() {
    // A relative path while its working directory can't be read, a command line that
    // can't be read, or macOS, which shows neither its command line nor its reach.
    for (name, host) in with_roots_reach(&["./target/release/x"]) {
        assert!(is_unknown(&host.activity(CARGO, TARGET)), "relative {name}");
    }
    for (name, mut host) in with_roots_reach(&["/usr/bin/fusermount3"]) {
        let mut macos = host.clone();
        macos.observer = Observer::PLAIN;
        assert!(is_unknown(&macos.activity(CARGO, TARGET)), "macOS {name}");
        host.processes[0].argv = Err(failed());
        assert!(is_unknown(&host.activity(CARGO, TARGET)), "unread {name}");
    }
}

#[test]
fn an_empty_or_blank_command_line_clears_nothing_where_the_os_shows_it() {
    // A process mid-exit, or one that blanked its argv, shows no program name: it might
    // be a runtime, and it names nothing that could be placed.
    let blank = |owner: ProcessOwner, argv: &[&str]| {
        let mut process = uninspectable(7, owner, &[]);
        process.argv = Ok(argv.iter().map(OsString::from).collect());
        Host::new(vec![process])
    };
    for argv in [&[][..], &[""], &["", ""], &[" \t"]] {
        for owner in [ProcessOwner::CurrentUser, ProcessOwner::Root] {
            for (evidence, path) in [(CARGO, TARGET), (PYTHON, VENV)] {
                let activity = blank(owner.clone(), argv).activity(evidence, path);
                assert!(
                    matches!(activity, Activity::Unknown(ActivityGap::Uninspected(1))),
                    "{owner:?} {argv:?} {path}: {activity:?}"
                );
            }
        }
    }
    // Without a program name it can't be ruled out as a runtime either.
    let nameless = blank(
        ProcessOwner::CurrentUser,
        &["", &abs("/usr/share/x").display().to_string()],
    );
    assert!(is_unknown(&nameless.activity(CARGO, TARGET)));
}

#[test]
fn a_title_is_placed_only_when_each_of_its_words_is() {
    // `/usr/bin/env ./target/release/x` rewritten into one string reads as an absolute
    // path whole, but its second word is relative and the working directory is unread.
    let argv = ["/usr/bin/env ./target/release/x"];
    for owner in [ProcessOwner::Root, ProcessOwner::CurrentUser] {
        let host = Host::new(vec![uninspectable(9, owner.clone(), &argv)]);
        assert!(
            matches!(
                host.activity(CARGO, TARGET),
                Activity::Unknown(ActivityGap::Uninspected(1))
            ),
            "{owner:?}"
        );
    }
}

#[test]
fn a_title_holding_secrets_reports_only_the_path_that_matched() {
    // A setproctitle'd worker rewrites its whole command line into argv[0].
    let title = |text: String| Host::new(vec![uninspectable(9, ProcessOwner::Root, &[&text])]);
    let python = abs("/home/u/code/app/.venv/bin/python");
    let secrets = "manage.py runserver --token=hunter2 --broker=redis://u:s3cret@h";
    let worker = title(format!("{} {secrets}", python.display()));
    assert_eq!(
        seen(&worker.activity(PYTHON, VENV)),
        Some(&Seen::Argument(python))
    );
    // A project directory with a space in its name matches only whole: what is shown
    // then is the finding itself, never the rest of the title.
    let spaced = "/home/u/code/my app/.venv";
    let whole = title(format!("{}/bin/python {secrets}", abs(spaced).display()));
    let activity = whole.activity(PYTHON, spaced);
    assert_eq!(seen(&activity), Some(&Seen::Argument(abs(spaced))));
    let shown = format!("{activity:?}");
    assert!(
        !shown.contains("hunter2") && !shown.contains("s3cret"),
        "{shown}"
    );
}

#[test]
fn a_runtime_naming_a_path_in_the_project_holds_its_interpreted_layout_from_anywhere() {
    // `cd ~ && node ~/code/app/server.js`, or a user service: Node loads the project's
    // `node_modules` without mapping it, and the process works outside the project.
    let modules = "/home/u/code/app/node_modules";
    let runtime = |executable, argv: &[&str]| {
        let mut process = inspected(7, ProcessOwner::CurrentUser, executable);
        process.cwd = Ok(abs("/tmp"));
        process.argv = Ok(os(argv));
        process
    };
    let server = "/home/u/code/app/server.js";
    let node = Host::new(vec![runtime("/usr/bin/node", &["node", server])]);
    assert_eq!(
        seen(&node.activity(NODE, modules)),
        Some(&Seen::Runtime(abs(server)))
    );
    let mut python = runtime("/usr/bin/python3", &["python3", "-m", "app"]);
    python.env_paths = Ok(vec![EnvPath {
        var: "PYTHONPATH".into(),
        path: abs("/home/u/code/app/src"),
    }]);
    assert_eq!(
        seen(&Host::new(vec![python]).activity(PYTHON, VENV)),
        Some(&Seen::Runtime(abs("/home/u/code/app/src")))
    );
    // Only a runtime of the layout holds it this way.
    let shell = Host::new(vec![runtime("/usr/bin/bash", &["bash", server])]);
    assert!(matches!(shell.activity(NODE, modules), Activity::Idle));
    assert!(matches!(node.activity(CARGO, TARGET), Activity::Idle));
}

#[test]
fn where_the_os_shows_no_process_paths_an_interpreted_layout_is_never_idle() {
    // Windows in M2 reads only each process's executable and owner.
    let mut windows = Host::new(Vec::new());
    windows.observer = Observer {
        process_paths: false,
        ..Observer::PLAIN
    };

    for (evidence, path) in [(PYTHON, VENV), (NODE, "/home/u/code/app/node_modules")] {
        assert!(
            matches!(
                windows.activity(evidence, path),
                Activity::Unknown(ActivityGap::Unsupported)
            ),
            "{path}"
        );
    }
    assert!(matches!(windows.activity(CARGO, TARGET), Activity::Idle));
}
