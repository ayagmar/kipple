//! Running git: found once on `PATH`, checked to be the same file before each run, and
//! isolated from every config and hook but the repository's own. Its command line overrides
//! `core.fsmonitor` and `core.hooksPath`, its environment stops a lazy fetch where git
//! honours that, and the worktree queries read the repository's config names before they
//! ask anything that could run a filter (`worktree.rs`), because a repository's config can
//! name a program no override reaches.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kipple_core::{
    CancelToken, Component, FileIdentity, GIT_ISOLATION, GitGap, NativeTool, ToolVersion,
};

use crate::capabilities;
use crate::environment::Environment;
use crate::native::{git_isolation, rendered};
use crate::os;
use crate::runner::{self, Invocation, Output, Var};

/// How long one git command may take.
const DEADLINE: Duration = Duration::from_secs(10);

/// The most output kept from one git command. More is an answer cut short.
pub(super) const OUTPUT_LIMIT: u64 = 64 * 1024 * 1024;

/// The first git with `GIT_CONFIG_GLOBAL`, without which the user's global config can't
/// be left out.
const OLDEST: [u64; 2] = [2, 32];

/// The git kipple runs: an absolute path from `PATH`, and the file it was when its version
/// was checked.
#[derive(Debug, Clone)]
pub(super) struct Git {
    path: PathBuf,
    identity: FileIdentity,
    /// What it said it is when it was found.
    version: ToolVersion,
    /// The environment it runs in, for what the runner passes on.
    env: Environment,
}

/// How git reads a pathspec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Matching {
    /// Byte for byte: every pathspec is literal.
    Exact,
    /// Ignoring ASCII case, for a path whose directories fold case. git refuses its
    /// global literal setting beside any other, so the pathspec says `literal` itself.
    FoldCase,
}

/// Finds git on the absolute entries of `env`'s `PATH`, as `doctor` does, and checks its
/// version.
pub(super) fn resolve(env: &Environment) -> Result<Git, GitGap> {
    let path = capabilities::find_on_path(NativeTool::Git.program(), env).ok_or(GitGap::Missing)?;
    let identity = identity(&path).ok_or(GitGap::Failed)?;
    let version = capabilities::version(NativeTool::Git, &path, env).map_err(|_| GitGap::Failed)?;
    if version < ToolVersion(OLDEST.to_vec()) {
        return Err(GitGap::TooOld);
    }
    Ok(Git {
        path,
        identity,
        version,
        env: env.clone(),
    })
}

/// The identity of the file at `path`, through any link.
pub(super) fn identity(path: &Path) -> Option<FileIdentity> {
    let meta = fs::metadata(path).ok()?;
    os::identity(path, &meta)
}

impl Git {
    /// Whether this git is older than `floor`.
    pub(super) fn older_than(&self, floor: &[u64]) -> bool {
        self.version < ToolVersion(floor.to_vec())
    }

    /// What git prints for `tail` from `dir`, whole and with exit 0.
    pub(super) fn ask(
        &self,
        dir: &Path,
        tail: &[&str],
        cancel: &CancelToken,
    ) -> Result<Vec<u8>, GitGap> {
        let tail: Vec<&OsStr> = tail.iter().map(OsStr::new).collect();
        let output = self.run(dir, Matching::Exact, &tail, cancel)?;
        Ok(output.stdout)
    }

    /// The work tree git finds from `dir`, as it prints it.
    pub(super) fn toplevel(&self, dir: &Path, cancel: &CancelToken) -> Result<PathBuf, GitGap> {
        let tail = [OsStr::new("rev-parse"), OsStr::new("--show-toplevel")];
        let output = self.run(dir, Matching::Exact, &tail, cancel)?;
        let line = output.stdout.strip_suffix(b"\n").ok_or(GitGap::Truncated)?;
        let path = PathBuf::from(os_string(line).ok_or(GitGap::Truncated)?);
        if !path.is_absolute() {
            return Err(GitGap::Truncated);
        }
        Ok(path)
    }

    /// Every path the index of the repository at `dir` holds at or below `pathspec`,
    /// relative to its top. `--sparse` keeps a sparse index as it is where it can, and a
    /// sparse directory the pathspec names comes back as a record [`Listing`] refuses. git
    /// still expands the index for a sparse directory that has something on disk, as an
    /// item below one does, unless config says not to, which would hide what is tracked
    /// there: so that config is overridden, and [`read_index`] sees an expansion that failed.
    pub(super) fn tracked(
        &self,
        dir: &Path,
        pathspec: &Pathspec,
        cancel: &CancelToken,
    ) -> Result<Listing, GitGap> {
        let mut tail = vec![
            OsStr::new("-c"),
            OsStr::new("sparse.expectFilesOutsideOfPatterns=false"),
            OsStr::new("ls-files"),
            OsStr::new("-z"),
            OsStr::new("--cached"),
            OsStr::new("--sparse"),
            OsStr::new("--full-name"),
            OsStr::new("--"),
        ];
        tail.extend(pathspec.arg.as_deref());
        let output = self.run(dir, pathspec.matching, &tail, cancel)?;
        Listing::of(read_index(output)?)
    }

    fn run(
        &self,
        dir: &Path,
        matching: Matching,
        tail: &[&OsStr],
        cancel: &CancelToken,
    ) -> Result<Output, GitGap> {
        if identity(&self.path) != Some(self.identity) {
            return Err(GitGap::Failed);
        }
        let isolation_args = rendered(GIT_ISOLATION, None).ok_or(GitGap::Failed)?;
        let mut args = vec![OsStr::new("-C"), dir.as_os_str()];
        let mut vars = git_isolation();
        if matching == Matching::Exact {
            args.push(OsStr::new("--literal-pathspecs"));
            vars.push(Var {
                name: "GIT_LITERAL_PATHSPECS",
                value: OsStr::new("1"),
            });
        }
        args.extend(isolation_args.iter().map(OsString::as_os_str));
        args.extend(tail);
        let invocation = Invocation {
            program: &self.path,
            args: &args,
            vars: &vars,
            deadline: DEADLINE,
            limit: OUTPUT_LIMIT,
            cancel: Some(cancel),
            dir: None,
        };
        let output = runner::invoke(&invocation, &self.env).map_err(|_| GitGap::Failed)?;
        if !output.status.success() {
            return Err(GitGap::Failed);
        }
        if output.truncated {
            return Err(GitGap::Truncated);
        }
        Ok(output)
    }
}

/// What `ls-files` printed, unless it lost part of the index: a tree it couldn't read when
/// expanding a sparse index prints `error:`, leaves those entries out and still exits 0. Only
/// that line counts, as an expansion that worked prints hints here.
fn read_index(output: Output) -> Result<Vec<u8>, GitGap> {
    let mut lines = output.stderr.split(|byte| *byte == b'\n');
    if lines.any(|line| line.starts_with(b"error:")) {
        return Err(GitGap::Failed);
    }
    Ok(output.stdout)
}

/// One pathspec, and how git is to match it.
#[derive(Debug)]
pub(super) struct Pathspec {
    /// None for the whole work tree.
    arg: Option<OsString>,
    matching: Matching,
}

impl Pathspec {
    /// The pathspec of `path` below the work tree, joined with `/` as git writes paths;
    /// none when `path` is empty.
    ///
    /// # Errors
    /// [`GitGap::Unpassable`] when a component can't reach git without loss: ill-formed
    /// UTF-16 on Windows.
    pub(super) fn of(path: &[Component], matching: Matching) -> Result<Self, GitGap> {
        if path.is_empty() {
            return Ok(Self {
                arg: None,
                matching: Matching::Exact,
            });
        }
        let mut arg = match matching {
            Matching::Exact => Vec::new(),
            Matching::FoldCase => b":(literal,icase)".to_vec(),
        };
        for (index, component) in path.iter().enumerate() {
            if index > 0 {
                arg.push(b'/');
            }
            arg.extend(passable(component.as_os_str()).ok_or(GitGap::Unpassable)?);
        }
        Ok(Self {
            arg: Some(os_string(&arg).ok_or(GitGap::Unpassable)?),
            matching,
        })
    }
}

/// One path git returned, by component.
pub(super) type Record = Vec<Component>;

/// What `ls-files -z` printed: records that each end in a NUL.
#[derive(Debug)]
pub(super) struct Listing(Vec<u8>);

impl Listing {
    /// `stdout`, if it is whole: empty, or ending in a NUL.
    pub(super) fn of(stdout: Vec<u8>) -> Result<Self, GitGap> {
        if stdout.is_empty() || stdout.ends_with(b"\0") {
            Ok(Self(stdout))
        } else {
            Err(GitGap::Truncated)
        }
    }

    /// Its records in order, each split into its components. An empty one, or one that
    /// isn't a path below the work tree, is malformed; one ending in `/` is a sparse
    /// directory, which places nothing kipple can check.
    pub(super) fn records(&self) -> impl Iterator<Item = Result<Record, GitGap>> + '_ {
        self.0
            .strip_suffix(b"\0")
            .into_iter()
            .flat_map(|body| body.split(|byte| *byte == 0))
            .map(record)
    }
}

fn record(bytes: &[u8]) -> Result<Record, GitGap> {
    if bytes.is_empty() {
        return Err(GitGap::Truncated);
    }
    if bytes.ends_with(b"/") {
        return Err(GitGap::Unplaced);
    }
    bytes
        .split(|byte| *byte == b'/')
        .map(|part| {
            let name = os_string(part).ok_or(GitGap::Truncated)?;
            Component::new(name).map_err(|_| GitGap::Truncated)
        })
        .collect()
}

/// `bytes` as git writes a path: as they are on Linux and macOS, UTF-8 on Windows.
#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "every byte string is a path on Linux and macOS, not on Windows"
)]
pub(super) fn os_string(bytes: &[u8]) -> Option<OsString> {
    use std::os::unix::ffi::OsStrExt as _;
    Some(OsStr::from_bytes(bytes).to_owned())
}

#[cfg(windows)]
pub(super) fn os_string(bytes: &[u8]) -> Option<OsString> {
    std::str::from_utf8(bytes).ok().map(OsString::from)
}

/// `name` as git takes it: its bytes on Linux and macOS; on Windows its UTF-8, which
/// ill-formed UTF-16 has none of.
#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "every name reaches git on Linux and macOS, not on Windows"
)]
fn passable(name: &OsStr) -> Option<&[u8]> {
    use std::os::unix::ffi::OsStrExt as _;
    Some(name.as_bytes())
}

#[cfg(windows)]
fn passable(name: &OsStr) -> Option<&[u8]> {
    name.to_str().map(str::as_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(stdout: &[u8]) -> Result<Vec<Record>, GitGap> {
        Listing::of(stdout.to_vec())?.records().collect()
    }

    fn names(record: &Record) -> Vec<&str> {
        record
            .iter()
            .map(|c| c.as_os_str().to_str().unwrap())
            .collect()
    }

    /// Catches an answer that was cut or malformed being read as complete: a last record
    /// without its NUL, or an empty record, would drop a tracked path.
    #[test]
    fn only_complete_nul_terminated_records_are_an_answer() {
        assert_eq!(records(b"").unwrap().len(), 0, "nothing tracked");
        let two = records(b"target/a\0target/b/c\0").unwrap();
        assert_eq!(names(&two[1]), ["target", "b", "c"]);

        for cut in [&b"target/a\0target/b"[..], b"target/a", b"\0", b"a\0\0b\0"] {
            assert_eq!(records(cut).unwrap_err(), GitGap::Truncated, "{cut:?}");
        }
        for malformed in [&b"a//b\0"[..], b"a/./b\0", b"a/../b\0", b"/a\0"] {
            assert_eq!(
                records(malformed).unwrap_err(),
                GitGap::Truncated,
                "{malformed:?}"
            );
        }
        assert_eq!(
            records(b"target/sparse/\0").unwrap_err(),
            GitGap::Unplaced,
            "a sparse directory entry places nothing kipple can check"
        );
    }
}
