//! A repository real git made, under docs/08 §5's isolation, and a scan of it through the
//! platform's `GitQuery`: what the `gate` and the removal's own tests start from.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use kipple_adapters::builtin;
use kipple_core::{
    CancelToken, Engine, EventChannel, FactValue, Finding, Inventory, Observer, Ports, ProbeError,
    ProcessProbe, ProcessSnapshot, Roots, ScanReport, ScanRequest, SymbolicRoot, event_channel,
};
use kipple_platform::{
    Environment, GitQuery, NoFollowFs, PathTools, SystemClock, Walker, resolve_roots,
};
use tempfile::TempDir;

/// A host running nothing, so a scan's activity never depends on this machine.
#[derive(Debug)]
pub(crate) struct NoProcesses;

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
pub(crate) enum Refs {
    Files,
    Reftable,
}

/// A repository at `projects/app` with one commit, and the test's own git setup.
#[derive(Debug, Clone)]
pub(crate) struct Repo {
    /// Shared, so a change the apply makes at its own moment can hold the repository.
    pub(crate) temp: Arc<TempDir>,
    pub(crate) main: PathBuf,
    pub(crate) checkout: PathBuf,
}

impl Repo {
    /// A repository with no linked worktree yet.
    pub(crate) fn bare_bones(refs: Refs) -> Self {
        let temp = TempDir::new().unwrap();
        for dir in ["home", "hooks", "projects/app"] {
            fs::create_dir_all(temp.path().join(dir)).unwrap();
        }
        fs::write(temp.path().join("gitconfig"), b"").unwrap();
        let main = temp.path().join("projects/app");
        let checkout = temp.path().join("projects/wt");
        let repo = Self {
            temp: Arc::new(temp),
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
    pub(crate) fn new() -> Self {
        Self::linked(Refs::Files)
    }

    pub(crate) fn linked(refs: Refs) -> Self {
        let repo = Self::bare_bones(refs);
        repo.add_worktree();
        repo
    }

    pub(crate) fn add_worktree(&self) {
        self.git(&["worktree", "add", "-q", "-b", "wt", "../wt"]);
    }

    pub(crate) fn commit(&self, name: &str, bytes: &[u8]) {
        fs::write(self.main.join(name), bytes).unwrap();
        self.git(&["add", "--", name]);
        self.git(&["commit", "-q", "-m", name]);
    }

    pub(crate) fn projects(&self) -> PathBuf {
        self.temp.path().join("projects")
    }

    /// The linked worktree's admin directory.
    pub(crate) fn admin(&self) -> PathBuf {
        self.main.join(".git/worktrees/wt")
    }

    /// Runs git in the main worktree.
    pub(crate) fn git(&self, args: &[&str]) -> Vec<u8> {
        self.git_in(&self.main, args)
    }

    /// Runs git in the linked worktree.
    pub(crate) fn wt(&self, args: &[&str]) -> Vec<u8> {
        self.git_in(&self.checkout, args)
    }

    pub(crate) fn git_in(&self, dir: &Path, args: &[&str]) -> Vec<u8> {
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
    pub(crate) fn run_git(&self, dir: &Path, args: &[&str]) -> Output {
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
    pub(crate) fn env(&self) -> Environment {
        self.env_with(std::env::var_os("PATH").unwrap_or_default())
    }

    pub(crate) fn env_with(&self, path: OsString) -> Environment {
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
    pub(crate) fn roots(&self, env: &Environment) -> Roots {
        let resolved = resolve_roots(env, &[self.projects()]);
        let mut roots = Roots::default();
        for root in resolved.all(SymbolicRoot::ProjectRoots) {
            roots.add(SymbolicRoot::ProjectRoots, root.clone());
        }
        roots
    }

    pub(crate) fn scan(&self) -> Scan {
        self.scan_in(&self.env())
    }

    pub(crate) fn scan_in(&self, env: &Environment) -> Scan {
        scan_of(env, self.roots(env))
    }
}

/// Scans `roots` as given, with nothing of their spelling resolved.
pub(crate) fn scan_of(env: &Environment, roots: Roots) -> Scan {
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
        roots,
        lens: None,
        exclude: Vec::new(),
        // The host protects every finding, as the default does: these tests read
        // the facts, which its answer leaves as they are.
        protections: kipple_core::Protections::default(),
    };
    let report = engine.scan(&request, &events, &cancel);
    drop(rx);
    Scan(report)
}

/// What a scan found.
#[derive(Debug)]
pub(crate) struct Scan(pub(crate) ScanReport);

impl Scan {
    /// The one git finding whose path ends in `projects/<name>`.
    pub(crate) fn finding(&self, name: &str) -> &Finding {
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
    pub(crate) fn fact_at(&self, name: &str, fact: &str) -> FactValue {
        let facts = self.finding(name).facts.as_ref().unwrap();
        *facts.supplied.values.get(fact).unwrap()
    }

    /// The fact `fact` of the worktree at `projects/wt`.
    pub(crate) fn fact(&self, fact: &str) -> FactValue {
        self.fact_at("wt", fact)
    }
}

pub(crate) fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
