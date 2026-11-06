//! The `gate`: what a real git says through the platform's `GitQuery`, supplied as facts.

#[cfg(unix)]
mod programs;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use kipple_adapters::builtin;
use kipple_core::{
    CancelToken, Engine, EventChannel, FactValue, Finding, Inventory, Observer, Ports, ProbeError,
    ProcessProbe, ProcessSnapshot, RegistrationState, Roots, ScanReport, ScanRequest, SymbolicRoot,
    event_channel,
};
use kipple_platform::{
    Environment, GitQuery, NoFollowFs, PathTools, SystemClock, Walker, resolve_roots,
};
use tempfile::TempDir;

/// A host running nothing, so a scan's activity never depends on this machine.
#[derive(Debug)]
struct NoProcesses;

impl ProcessProbe for NoProcesses {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        Ok(ProcessSnapshot {
            processes: Vec::new(),
            inventory: Inventory::Complete,
            observer: Observer::PLAIN,
        })
    }
}

/// Which `git init` a repository starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refs {
    Files,
    Reftable,
}

/// A repository at `projects/app` with one commit, and the test's own git setup.
#[derive(Debug)]
struct Repo {
    temp: TempDir,
    main: PathBuf,
    checkout: PathBuf,
}

impl Repo {
    /// A repository with no linked worktree yet.
    fn bare_bones(refs: Refs) -> Self {
        let temp = TempDir::new().unwrap();
        for dir in ["home", "hooks", "projects/app"] {
            fs::create_dir_all(temp.path().join(dir)).unwrap();
        }
        fs::write(temp.path().join("gitconfig"), b"").unwrap();
        let main = temp.path().join("projects/app");
        let checkout = temp.path().join("projects/wt");
        let repo = Self {
            temp,
            main,
            checkout,
        };
        match refs {
            Refs::Files => repo.git(&["init", "-q", "-b", "main"]),
            Refs::Reftable => repo.git(&["init", "-q", "-b", "main", "--ref-format=reftable"]),
        };
        repo.commit("file.txt", b"contents\n");
        repo
    }

    /// A repository with a clean linked worktree at `projects/wt`.
    fn new() -> Self {
        Self::linked(Refs::Files)
    }

    fn linked(refs: Refs) -> Self {
        let repo = Self::bare_bones(refs);
        repo.add_worktree();
        repo
    }

    fn add_worktree(&self) {
        self.git(&["worktree", "add", "-q", "-b", "wt", "../wt"]);
    }

    fn commit(&self, name: &str, bytes: &[u8]) {
        fs::write(self.main.join(name), bytes).unwrap();
        self.git(&["add", "--", name]);
        self.git(&["commit", "-q", "-m", name]);
    }

    fn projects(&self) -> PathBuf {
        self.temp.path().join("projects")
    }

    /// The linked worktree's admin directory.
    fn admin(&self) -> PathBuf {
        self.main.join(".git/worktrees/wt")
    }

    /// Runs git in the main worktree.
    fn git(&self, args: &[&str]) -> Vec<u8> {
        self.git_in(&self.main, args)
    }

    /// Runs git in the linked worktree.
    fn wt(&self, args: &[&str]) -> Vec<u8> {
        self.git_in(&self.checkout, args)
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> Vec<u8> {
        let output = self.run_git(dir, args);
        assert!(
            output.status.success(),
            "the fixture's git {args:?} succeeds: {output:?}"
        );
        output.stdout
    }

    /// Runs the host's git with the test's isolation.
    #[expect(
        clippy::disallowed_methods,
        reason = "a fixture builds its own repository with the host's git"
    )]
    fn run_git(&self, dir: &Path, args: &[&str]) -> Output {
        let hooks = format!(
            "core.hooksPath={}",
            self.temp.path().join("hooks").display()
        );
        let mut command = Command::new("git");
        command
            .current_dir(dir)
            .args(["-c", &hooks, "-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args);
        let inherited = std::env::vars_os().map(|(name, _)| name);
        for name in inherited.filter(|name| name.to_string_lossy().starts_with("GIT_")) {
            command.env_remove(name);
        }
        command
            .env("HOME", self.temp.path().join("home"))
            .env("XDG_CONFIG_HOME", self.temp.path().join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("gitconfig"))
            .output()
            .unwrap()
    }

    /// What a scan runs in: the host's `PATH` and nothing of the user's.
    fn env(&self) -> Environment {
        self.env_with(std::env::var_os("PATH").unwrap_or_default())
    }

    fn env_with(&self, path: OsString) -> Environment {
        let home = self.temp.path().join("home").into_os_string();
        let host = std::env::var_os("SystemRoot").map(|root| ("SystemRoot".into(), root));
        let vars = [
            (OsString::from("PATH"), path),
            (OsString::from("HOME"), home.clone()),
            (OsString::from("USERPROFILE"), home),
        ];
        Environment::from_vars(vars.into_iter().chain(host))
    }

    /// Only the project root the test made: no other integration's root is reached.
    fn roots(&self, env: &Environment) -> Roots {
        let resolved = resolve_roots(env, &[self.projects()]);
        let mut roots = Roots::default();
        for root in resolved.all(SymbolicRoot::ProjectRoots) {
            roots.add(SymbolicRoot::ProjectRoots, root.clone());
        }
        roots
    }

    fn scan(&self) -> Scan {
        self.scan_in(&self.env())
    }

    fn scan_in(&self, env: &Environment) -> Scan {
        let packs = builtin::packs(&semver::Version::new(0, 0, 0)).unwrap();
        let rules = packs.into_iter().flat_map(|pack| pack.rules).collect();
        let ports = Ports {
            fs: Box::new(NoFollowFs),
            sizer: Box::new(Walker::new().unwrap()),
            processes: Box::new(NoProcesses),
            clock: Box::new(SystemClock),
            tracked: Box::new(GitQuery::new(env)),
            tools: Box::new(PathTools::new(env)),
            worktrees: Box::new(GitQuery::new(env)),
        };
        let engine = Engine::new(ports, builtin::integrations(), rules).unwrap();
        let cancel = CancelToken::new();
        let EventChannel { tx: events, rx } = event_channel(4096, &cancel);
        let request = ScanRequest {
            roots: self.roots(env),
            lens: None,
            exclude: Vec::new(),
        };
        let report = engine.scan(&request, &events, &cancel);
        drop(rx);
        Scan(report)
    }
}

/// What a scan found.
#[derive(Debug)]
struct Scan(ScanReport);

impl Scan {
    /// The one git finding whose path ends in `projects/<name>`.
    fn finding(&self, name: &str) -> &Finding {
        let tail = Path::new("projects").join(name);
        let found: Vec<_> = self
            .0
            .findings
            .iter()
            .filter(|f| f.integration.0 == "git" && f.path.ends_with(&tail))
            .collect();
        assert_eq!(
            found.len(),
            1,
            "one finding at {name}: {:?}",
            self.0.findings
        );
        found.first().copied().unwrap()
    }

    /// The fact `fact` of the finding at `projects/<name>`.
    fn fact_at(&self, name: &str, fact: &str) -> FactValue {
        let facts = self.finding(name).facts.as_ref().unwrap();
        *facts.supplied.values.get(fact).unwrap()
    }

    /// The fact `fact` of the worktree at `projects/wt`.
    fn fact(&self, fact: &str) -> FactValue {
        self.fact_at("wt", fact)
    }
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Catches a false block: every row reads clear on a clean worktree, or nothing could
/// ever be eligible once m4c gives these rules predicates.
#[test]
fn a_clean_worktree_reads_clear_on_every_row() {
    let scan = Repo::new().scan();

    for (fact, value) in [
        ("worktree.linked", FactValue::Bool(true)),
        ("repository_identity", FactValue::Enum("verified")),
        ("worktree.locked", FactValue::Bool(false)),
        ("worktree.submodules", FactValue::Bool(false)),
        ("worktree.config_gate", FactValue::Enum("clear")),
        ("git.tracked_changes", FactValue::Count(0)),
        ("git.untracked_files", FactValue::Count(0)),
        ("git.hidden_changes", FactValue::Count(0)),
        ("worktree.unclassified_ignored", FactValue::Count(0)),
        ("worktree.operation_in_progress", FactValue::Bool(false)),
        ("worktree.per_worktree_refs", FactValue::Count(0)),
        ("worktree.index_locked", FactValue::Bool(false)),
        ("worktree.head_reachable", FactValue::Bool(true)),
    ] {
        assert_eq!(scan.fact(fact), value, "{fact}");
    }
    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredPresent.name())
    );
}

/// One state a worktree can be in, and the blocking value of the row it shows in.
struct Case {
    name: &'static str,
    set: fn(&Repo),
    at: &'static str,
    fact: &'static str,
    value: FactValue,
}

const fn case(name: &'static str, set: fn(&Repo), fact: &'static str, value: FactValue) -> Case {
    Case {
        name,
        set,
        at: "wt",
        fact,
        value,
    }
}

const IGNORED: &str = "worktree.unclassified_ignored";
const OPERATION: &str = "worktree.operation_in_progress";
const LOCKED_INDEX: &str = "worktree.index_locked";

fn ignored_file(repo: &Repo) {
    write(&repo.checkout.join(".gitignore"), b"local.env\n");
    write(&repo.checkout.join("local.env"), b"x\n");
}

fn excluded_file(repo: &Repo) {
    write(&repo.main.join(".git/info/exclude"), b"local.env\n");
    write(&repo.checkout.join("local.env"), b"x\n");
}

fn ignored_repository(repo: &Repo) {
    write(&repo.checkout.join(".gitignore"), b"scratch/\n");
    let nested = repo.checkout.join("scratch");
    fs::create_dir(&nested).unwrap();
    repo.git_in(&nested, &["init", "-q"]);
}

fn nested_worktree(repo: &Repo) {
    write(&repo.checkout.join(".gitignore"), b"scratch/\n");
    repo.git(&["worktree", "add", "-q", "-b", "n", "../wt/scratch/nested"]);
}

fn hidden_edit(repo: &Repo, flag: &str) {
    repo.wt(&["update-index", flag, "file.txt"]);
    write(&repo.checkout.join("file.txt"), b"changed\n");
}

fn rebase_stopped(repo: &Repo) {
    write(&repo.checkout.join("file.txt"), b"theirs\n");
    repo.wt(&["commit", "-q", "-a", "-m", "theirs"]);
    repo.commit("file.txt", b"ours\n");
    let output = repo.run_git(&repo.checkout, &["rebase", "main"]);
    assert!(!output.status.success(), "the rebase stops: {output:?}");
}

fn unreachable_head(repo: &Repo) {
    repo.wt(&["checkout", "-q", "--detach"]);
    write(&repo.checkout.join("file.txt"), b"changed\n");
    repo.wt(&["commit", "-q", "-a", "-m", "lost"]);
}

fn one_way_registration(repo: &Repo) {
    let elsewhere = repo.temp.path().join("projects/elsewhere/.git");
    let line = format!("{}\n", elsewhere.display());
    write(&repo.admin().join("gitdir"), line.as_bytes());
}

fn unborn(repo: &Repo) {
    repo.git(&["worktree", "add", "-q", "--detach", "../unborn"]);
    let unborn = repo.temp.path().join("projects/unborn");
    repo.git_in(&unborn, &["checkout", "-q", "--orphan", "new"]);
}

const CASES: &[Case] = &[
    case(
        "locked",
        |repo| {
            repo.git(&["worktree", "lock", "--reason", "agent", "../wt"]);
        },
        "worktree.locked",
        FactValue::Bool(true),
    ),
    case(
        "tracked change",
        |repo| write(&repo.checkout.join("file.txt"), b"changed\n"),
        "git.tracked_changes",
        FactValue::Count(1),
    ),
    case(
        "untracked",
        |repo| write(&repo.checkout.join("new.txt"), b"x\n"),
        "git.untracked_files",
        FactValue::Count(1),
    ),
    case(
        "untracked hidden by config",
        |repo| {
            repo.git(&["config", "status.showUntrackedFiles", "no"]);
            write(&repo.checkout.join("new.txt"), b"x\n");
        },
        "git.untracked_files",
        FactValue::Count(1),
    ),
    case(
        "ignored by .gitignore",
        ignored_file,
        IGNORED,
        FactValue::Count(1),
    ),
    case(
        "ignored by info/exclude",
        excluded_file,
        IGNORED,
        FactValue::Count(1),
    ),
    case(
        "ignored repository",
        ignored_repository,
        IGNORED,
        FactValue::Count(1),
    ),
    case(
        "nested worktree",
        nested_worktree,
        IGNORED,
        FactValue::Count(1),
    ),
    case(
        "skip-worktree edit",
        |repo| hidden_edit(repo, "--skip-worktree"),
        "git.hidden_changes",
        FactValue::Count(1),
    ),
    case(
        "assume-unchanged edit",
        |repo| hidden_edit(repo, "--assume-unchanged"),
        "git.hidden_changes",
        FactValue::Count(1),
    ),
    case(
        "real rebase",
        rebase_stopped,
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "rebase-merge",
        |repo| fs::create_dir(repo.admin().join("rebase-merge")).unwrap(),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "rebase-apply",
        |repo| fs::create_dir(repo.admin().join("rebase-apply")).unwrap(),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "bisect",
        |repo| write(&repo.admin().join("BISECT_LOG"), b"x\n"),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "cherry-pick",
        |repo| write(&repo.admin().join("CHERRY_PICK_HEAD"), b"x\n"),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "revert",
        |repo| write(&repo.admin().join("REVERT_HEAD"), b"x\n"),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "merge",
        |repo| write(&repo.admin().join("MERGE_HEAD"), b"x\n"),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "index.lock",
        |repo| write(&repo.admin().join("index.lock"), b""),
        LOCKED_INDEX,
        FactValue::Bool(true),
    ),
    case(
        "HEAD.lock",
        |repo| write(&repo.admin().join("HEAD.lock"), b""),
        LOCKED_INDEX,
        FactValue::Bool(true),
    ),
    case(
        "per-worktree ref",
        |repo| {
            repo.wt(&["update-ref", "refs/worktree/keep", "HEAD"]);
        },
        "worktree.per_worktree_refs",
        FactValue::Count(1),
    ),
    case(
        "unreachable detached HEAD",
        unreachable_head,
        "worktree.head_reachable",
        FactValue::Bool(false),
    ),
    Case {
        name: "unborn branch",
        set: unborn,
        at: "unborn",
        fact: "worktree.head_reachable",
        value: FactValue::Bool(false),
    },
    case(
        "one-way registration",
        one_way_registration,
        "registration_state",
        FactValue::Enum("present-unregistered"),
    ),
];

/// Catches a row that doesn't reach its fact, or reads clear on the very state it exists
/// to show: each of these loses work if a worktree in it is removed.
#[test]
fn each_gate_fact_is_supplied() {
    for case in CASES {
        let repo = Repo::new();
        (case.set)(&repo);

        let scan = repo.scan();

        assert_eq!(
            scan.fact_at(case.at, case.fact),
            case.value,
            "{}",
            case.name
        );
    }
}

/// A repository whose worktree has a submodule at `sm`, not populated.
fn with_submodule() -> Repo {
    let repo = Repo::bare_bones(Refs::Files);
    let sub = repo.temp.path().join("sub");
    fs::create_dir(&sub).unwrap();
    repo.git_in(&sub, &["init", "-q", "-b", "main"]);
    repo.git_in(&sub, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let url = sub.display().to_string();
    let add = ["-c", "protocol.file.allow=always", "submodule", "add", "-q"];
    repo.git(&[&add[..], &[url.as_str(), "sm"]].concat());
    repo.git(&["commit", "-q", "-m", "submodule"]);
    repo.add_worktree();
    repo
}

/// Catches a submodule missed for being empty here: git refuses to remove a worktree
/// with one, populated or not, and a populated one holds a repository of its own.
#[test]
fn a_submodule_is_supplied_populated_or_not() {
    let repo = with_submodule();
    assert_eq!(
        repo.scan().fact("worktree.submodules"),
        FactValue::Bool(true)
    );

    repo.wt(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "update",
        "--init",
    ]);
    assert_eq!(
        repo.scan().fact("worktree.submodules"),
        FactValue::Bool(true)
    );
}

/// Catches `core.worktree` answered around: status would describe another directory and
/// call this worktree clean.
#[test]
fn core_worktree_makes_status_facts_unknown() {
    let repo = Repo::new();
    repo.git(&["config", "extensions.worktreeConfig", "true"]);
    let other = repo.temp.path().join("other");
    fs::create_dir(&other).unwrap();
    let other = other.display().to_string();
    repo.wt(&["config", "--worktree", "core.worktree", &other]);

    let scan = repo.scan();

    assert_eq!(
        scan.fact("worktree.config_gate"),
        FactValue::Enum("blocked")
    );
    assert_eq!(scan.fact("git.tracked_changes"), FactValue::Unknown);
    assert_eq!(scan.fact("git.untracked_files"), FactValue::Unknown);
}

/// Catches a reftable repository read as if its refs were files: its per-worktree refs
/// aren't where they are counted, so zero would be a guess. A git before 2.45 can't make one.
#[test]
fn reftable_makes_facts_unknown() {
    let probe = Repo::bare_bones(Refs::Files);
    let args = ["init", "-q", "--ref-format=reftable", "../probe"];
    if !probe.run_git(&probe.main, &args).status.success() {
        return;
    }

    let scan = Repo::linked(Refs::Reftable).scan();

    assert_eq!(
        scan.fact("worktree.config_gate"),
        FactValue::Enum("blocked")
    );
    assert_eq!(scan.fact("worktree.per_worktree_refs"), FactValue::Unknown);
    assert_eq!(scan.fact(OPERATION), FactValue::Unknown);
    assert_eq!(scan.fact("git.tracked_changes"), FactValue::Unknown);
}

/// Catches a sparse directory read as an empty one: a gitlink or a hidden edit below it is
/// behind that record, so the rows that come from the index can't read clear.
#[test]
fn a_sparse_directory_leaves_the_index_rows_unknown() {
    let repo = Repo::bare_bones(Refs::Files);
    write(&repo.main.join("keep/a.txt"), b"a\n");
    write(&repo.main.join("away/b.txt"), b"b\n");
    repo.git(&["add", "--", "keep", "away"]);
    repo.git(&["commit", "-q", "-m", "two directories"]);
    repo.git(&["sparse-checkout", "set", "--cone", "--sparse-index", "keep"]);
    repo.add_worktree();

    let scan = repo.scan();

    assert_eq!(scan.fact("worktree.submodules"), FactValue::Unknown);
    assert_eq!(scan.fact("git.hidden_changes"), FactValue::Unknown);
}

/// Catches a config value carried into a report: the gate reads key names, and a URL or a
/// filter command can hold a credential.
#[test]
fn config_values_appear_nowhere() {
    const SECRET: &str = "s3cret-value";
    let repo = Repo::new();
    let url = format!("https://user:{SECRET}@example.invalid/x.git");
    repo.git(&["config", "remote.origin.url", &url]);
    repo.git(&["config", "filter.mark.clean", &format!("cat {SECRET}")]);

    let scan = repo.scan();

    assert_eq!(
        scan.fact("worktree.config_gate"),
        FactValue::Enum("blocked")
    );
    let report = format!("{:?}", scan.0);
    assert_eq!(report.matches(SECRET).count(), 0, "in {report}");
}

/// Catches a space or a byte past ASCII mangled between git's output and the report.
#[test]
fn a_path_with_a_space_and_a_byte_past_ascii_is_read() {
    let repo = Repo::bare_bones(Refs::Files);
    repo.git(&["worktree", "add", "-q", "-b", "odd", "../\u{e9}t\u{e9} one"]);

    let scan = repo.scan();

    assert_eq!(
        scan.fact_at("\u{e9}t\u{e9} one", "worktree.config_gate"),
        FactValue::Enum("clear")
    );
}

/// Catches a bare main refused for having no checkout: its worktrees are found through
/// their own `.git` files, and belong to it all the same.
#[test]
fn a_bare_main_lists_its_worktrees() {
    let repo = Repo::bare_bones(Refs::Files);
    let bare = repo.temp.path().join("bare.git");
    let bare_text = bare.display().to_string();
    repo.git(&["clone", "-q", "--bare", ".", &bare_text]);
    repo.git_in(
        &bare,
        &["worktree", "add", "-q", "-b", "wt", "../projects/wt"],
    );

    let scan = repo.scan();

    assert_eq!(scan.fact("worktree.linked"), FactValue::Bool(true));
    assert_eq!(
        scan.fact("repository_identity"),
        FactValue::Enum("verified")
    );
}

/// Catches a `.git` pointing at another worktree's admin entry read as this one: every
/// fact would describe the other checkout.
#[test]
fn a_gitdir_at_another_worktree_is_invalid() {
    let repo = Repo::new();
    repo.git(&["worktree", "add", "-q", "-b", "other", "../other"]);
    let other = repo.main.join(".git/worktrees/other");
    let line = format!("gitdir: {}\n", other.display());
    write(&repo.checkout.join(".git"), line.as_bytes());

    let scan = repo.scan();

    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredInvalid.name())
    );
}

/// Catches a `.git` that is a link followed to a file elsewhere: a checkout's own `.git`
/// is a plain file, so one that isn't proves nothing.
#[cfg(unix)]
#[test]
fn a_git_link_is_no_present_registration() {
    let repo = Repo::new();
    let elsewhere = repo.temp.path().join("gitfile");
    let dot_git = repo.checkout.join(".git");
    fs::copy(&dot_git, &elsewhere).unwrap();
    remove_file(&dot_git);
    std::os::unix::fs::symlink(&elsewhere, &dot_git).unwrap();

    let scan = repo.scan();

    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredInvalid.name())
    );
}

#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "a fixture replaces a file below its temp root"
)]
fn remove_file(path: &Path) {
    fs::remove_file(path).unwrap();
}

#[expect(
    clippy::disallowed_methods,
    reason = "a fixture removes a directory below its temp root, as a person would"
)]
fn remove_dir(path: &Path) {
    fs::remove_dir_all(path).unwrap();
}

#[expect(
    clippy::disallowed_methods,
    reason = "a fixture moves a directory below its temp root, as a person would"
)]
fn rename(from: &Path, to: &Path) {
    fs::rename(from, to).unwrap();
}

/// Catches a lock lost once the directory is gone: a lock on a missing checkout usually
/// means a disk that isn't mounted, and prune must leave it.
#[test]
fn a_locked_checkout_that_is_gone_stays_locked() {
    let repo = Repo::new();
    repo.git(&["worktree", "lock", "../wt"]);
    remove_dir(&repo.checkout);

    let scan = repo.scan();

    assert_eq!(scan.fact("worktree.locked"), FactValue::Bool(true));
    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredMissing.name())
    );
}

/// Catches a repository that moved leaving its checkouts unreported: each `.git` file
/// points at nothing, so no repository lists them, and they are still someone's work.
#[test]
fn a_repository_moved_out_of_reach_leaves_its_worktree_unregistered() {
    let repo = Repo::new();
    rename(&repo.main, &repo.temp.path().join("moved"));

    let scan = repo.scan();

    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::PresentUnregistered.name())
    );
}

/// Catches a checkout found twice when its repository moved inside a root: the moved
/// repository still lists it, so it has its registration's one finding, which disagrees
/// with what is on disk.
#[test]
fn a_repository_moved_in_reach_keeps_one_finding_per_worktree() {
    let repo = Repo::new();
    rename(&repo.main, &repo.temp.path().join("projects/moved"));

    let scan = repo.scan();

    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredInvalid.name())
    );
}
