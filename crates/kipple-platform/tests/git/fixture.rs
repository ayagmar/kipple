//! A disposable git repository for the tracked query, built by the host's git with the
//! test's own config: an empty global config file, no system config, an empty hooks
//! directory and a temporary `HOME`, so nothing of the user's is read or run.
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use kipple_core::{
    Anchor, CancelToken, FileIdentity, Freezer as _, FrozenTree, FsProbe as _, GitGap, NodeId,
    RelPath, SymbolicRoot, TrackedQuery as _, VerifiedRoot,
};
use kipple_platform::{ConfinedFs, Environment, GitQuery, NoFollowFs};
use tempfile::TempDir;

/// A temp directory holding `repo/`, a git work tree, and the test's own git setup.
#[derive(Debug)]
pub(crate) struct Repo {
    pub(crate) temp: TempDir,
    pub(crate) root: PathBuf,
}

impl Repo {
    /// An empty repository.
    pub(crate) fn new() -> Self {
        let temp = TempDir::new().unwrap();
        for dir in ["home", "hooks", "repo"] {
            fs::create_dir(temp.path().join(dir)).unwrap();
        }
        fs::write(temp.path().join("gitconfig"), b"").unwrap();
        let root = temp.path().join("repo");
        let repo = Self { temp, root };
        repo.git(&["init", "-q"]);
        repo
    }

    /// `relative` below the work tree.
    pub(crate) fn at(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    /// Writes `relative` below the work tree, with its directories.
    pub(crate) fn write(&self, relative: impl AsRef<Path>) -> PathBuf {
        let path = self.at(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"contents").unwrap();
        path
    }

    /// Writes `relative` and adds it to the index.
    pub(crate) fn track(&self, relative: impl AsRef<Path>) {
        let relative = relative.as_ref();
        self.write(relative);
        self.git_os(&[
            OsStr::new("add"),
            OsStr::new("--"),
            &literal(relative.as_os_str()),
        ]);
    }

    /// Runs the host's git in the work tree with the test's isolation.
    pub(crate) fn git(&self, args: &[&str]) -> Vec<u8> {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        self.git_os(&args)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "fixture repositories are built with the host's git"
    )]
    pub(crate) fn git_os(&self, args: &[&OsStr]) -> Vec<u8> {
        let hooks = format!(
            "core.hooksPath={}",
            self.temp.path().join("hooks").display()
        );
        let mut command = Command::new("git");
        command
            .current_dir(&self.root)
            .args(["-c", &hooks, "-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        let output = command
            .env("HOME", self.temp.path().join("home"))
            .env("XDG_CONFIG_HOME", self.temp.path().join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.temp.path().join("gitconfig"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "the fixture's git {args:?} succeeds: {output:?}"
        );
        output.stdout
    }

    /// The anchor of `relative` below the work tree, which is the root.
    pub(crate) fn anchor(&self, relative: impl AsRef<Path>) -> Anchor {
        let relative = RelPath::of(relative.as_ref()).unwrap();
        let parent = relative
            .components()
            .split_last()
            .map(|(_, way)| identity(&RelPath::new(way.to_vec()).below(&self.root)));
        Anchor {
            root: VerifiedRoot {
                root: SymbolicRoot::ProjectRoots,
                path: self.root.clone(),
                identity: identity(&self.root),
            },
            relative,
            parent,
        }
    }

    /// How many paths at or inside `relative` git tracks.
    pub(crate) fn count(&self, relative: impl AsRef<Path>) -> Result<u64, GitGap> {
        query().count(&self.root, &self.at(relative), &CancelToken::new())
    }

    /// The item at `relative`, frozen, and the paths of what git tracks in it.
    pub(crate) fn tracked(&self, relative: impl AsRef<Path>) -> Result<Vec<String>, GitGap> {
        let anchor = self.anchor(relative);
        let tree = freeze(&anchor);
        let ids = query().entries(&self.root, &anchor, &tree, &CancelToken::new())?;
        Ok(paths(&tree, &ids))
    }
}

/// The query, with git found on the test's `PATH` and nothing else of its environment
/// but what the OS needs.
pub(crate) fn query() -> GitQuery {
    GitQuery::new(&host_env(&[]))
}

/// The environment kipple would see: the test's `PATH`, what Windows needs to start a
/// process, and `extra` variables, each a name and its value.
pub(crate) fn host_env(extra: &[[&str; 2]]) -> Environment {
    let host = ["PATH", "SystemRoot"]
        .into_iter()
        .filter_map(|name| Some((OsString::from(name), std::env::var_os(name)?)));
    let extra = extra
        .iter()
        .map(|[name, value]| (OsString::from(name), OsString::from(value)));
    Environment::from_vars(host.chain(extra))
}

pub(crate) fn freeze(anchor: &Anchor) -> FrozenTree {
    ConfinedFs.freeze(anchor, &CancelToken::new()).unwrap().tree
}

/// The paths below the top of `ids` in `tree`, `/`-joined and sorted.
pub(crate) fn paths(tree: &FrozenTree, ids: &[NodeId]) -> Vec<String> {
    let mut paths: Vec<String> = ids
        .iter()
        .map(|id| {
            let path = tree.entry(*id).unwrap().path();
            let names: Vec<String> = path
                .components()
                .iter()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            names.join("/")
        })
        .collect();
    paths.sort();
    paths
}

/// The identity a scan records for `path`.
pub(crate) fn identity(path: &Path) -> FileIdentity {
    NoFollowFs.metadata(path).unwrap().identity.unwrap()
}

/// `path` as a pathspec that git reads literally.
fn literal(path: &OsStr) -> OsString {
    let mut spec = OsString::from(":(literal)");
    #[cfg(unix)]
    spec.push(path);
    #[cfg(windows)]
    spec.push(path.to_string_lossy().replace('\\', "/"));
    spec
}

/// Renames `from` to `to`, as a person would.
#[expect(
    clippy::disallowed_methods,
    reason = "a fixture renames a directory below its temp root"
)]
pub(crate) fn rename(from: &Path, to: &Path) {
    fs::rename(from, to).unwrap();
}

/// Removes the file at `path`, and its directory once empty, as a person would.
#[expect(
    clippy::disallowed_methods,
    reason = "a fixture removes a file below its temp root"
)]
pub(crate) fn remove(path: &Path) {
    fs::remove_file(path).unwrap();
    if let Some(dir) = path.parent()
        && fs::read_dir(dir).unwrap().next().is_none()
    {
        fs::remove_dir(dir).unwrap();
    }
}

/// A `git` on a `PATH` of its own: it reports `version`, names the repository's work
/// tree as its top, and prints `listing` for `ls-files`.
#[cfg(unix)]
pub(crate) fn fake_git(repo: &Repo, version: &str, listing: &Path) -> Environment {
    use std::os::unix::fs::PermissionsExt as _;

    let bin = repo.temp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in\n\
         *--version*) echo 'git version {version}' ;;\n\
         *rev-parse*) printf '%s\\n' '{top}' ;;\n\
         *ls-files*) cat '{listing}' ;;\n\
         *) exit 1 ;;\nesac\n",
        top = repo.root.display(),
        listing = listing.display(),
    );
    let git = bin.join("git");
    fs::write(&git, script).unwrap();
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
    Environment::from_vars([(OsString::from("PATH"), bin.into_os_string())])
}
