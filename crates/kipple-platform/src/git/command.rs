//! Running git: found once on `PATH`, checked to be the same file before each run, and
//! isolated from every config and hook but the repository's own, which its command line
//! overrides where it could run anything.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kipple_core::{CancelToken, Component, FileIdentity, GitGap, NativeTool, ToolVersion};

use crate::capabilities;
use crate::environment::Environment;
use crate::os;
use crate::runner::{self, Invocation, Output, Var};

/// How long one git command may take.
const DEADLINE: Duration = Duration::from_secs(10);

/// The most output kept from one git command. More is an answer cut short.
pub(super) const OUTPUT_LIMIT: u64 = 64 * 1024 * 1024;

/// The first git with `GIT_CONFIG_GLOBAL`, without which the user's global config can't
/// be left out.
const OLDEST: [u64; 2] = [2, 32];

/// A file that is always empty and that no one can replace: the global config, the
/// hooks directory and, on Linux and macOS, the XDG config directory all point here, so
/// git finds nothing in them.
#[cfg(unix)]
const NULL_DEVICE: &str = "/dev/null";
#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";

/// The git kipple runs: an absolute path from `PATH`, and the file it was when its version
/// was checked.
#[derive(Debug, Clone)]
pub(super) struct Git {
    path: PathBuf,
    identity: FileIdentity,
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
        env: env.clone(),
    })
}

/// The identity of the file at `path`.
fn identity(path: &Path) -> Option<FileIdentity> {
    let meta = fs::metadata(path).ok()?;
    os::identity(path, &meta)
}

impl Git {
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
    /// relative to its top.
    pub(super) fn tracked(
        &self,
        dir: &Path,
        pathspec: &Pathspec,
        cancel: &CancelToken,
    ) -> Result<Listing, GitGap> {
        let tail = [
            OsStr::new("ls-files"),
            OsStr::new("-z"),
            OsStr::new("--cached"),
            OsStr::new("--full-name"),
            OsStr::new("--"),
            &pathspec.arg,
        ];
        let output = self.run(dir, pathspec.matching, &tail, cancel)?;
        Listing::of(output.stdout)
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
        let hooks = format!("core.hooksPath={NULL_DEVICE}");
        let mut args = vec![OsStr::new("-C"), dir.as_os_str(), OsStr::new("--no-pager")];
        if matching == Matching::Exact {
            args.push(OsStr::new("--literal-pathspecs"));
        }
        args.extend([
            OsStr::new("-c"),
            OsStr::new("core.fsmonitor=false"),
            OsStr::new("-c"),
            OsStr::new(&hooks),
            OsStr::new("--no-optional-locks"),
        ]);
        args.extend(tail);
        let invocation = Invocation {
            program: &self.path,
            args: &args,
            vars: &isolation(matching),
            deadline: DEADLINE,
            limit: OUTPUT_LIMIT,
            cancel: Some(cancel),
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

/// The variables git gets beside the runner's: no system or global config, no XDG config
/// directory, no optional locks, no pager, and literal pathspecs where they are exact.
fn isolation(matching: Matching) -> Vec<Var<'static>> {
    let var = |name, value| Var {
        name,
        value: OsStr::new(value),
    };
    let mut vars = vec![
        var("GIT_CONFIG_NOSYSTEM", "1"),
        var("GIT_CONFIG_GLOBAL", NULL_DEVICE),
        var("GIT_OPTIONAL_LOCKS", "0"),
        var("GIT_PAGER", "cat"),
    ];
    // Windows has no directory that can't hold a file: its XDG directory stays unset, as
    // `HOME` is, so git looks for none.
    #[cfg(unix)]
    vars.push(var("XDG_CONFIG_HOME", NULL_DEVICE));
    if matching == Matching::Exact {
        vars.push(var("GIT_LITERAL_PATHSPECS", "1"));
    }
    vars
}

/// One pathspec, and how git is to match it.
#[derive(Debug)]
pub(super) struct Pathspec {
    arg: OsString,
    matching: Matching,
}

impl Pathspec {
    /// The pathspec of `path` below the work tree, joined with `/` as git writes paths.
    ///
    /// # Errors
    /// [`GitGap::Unpassable`] when a component can't reach git without loss: ill-formed
    /// UTF-16 on Windows.
    pub(super) fn of(path: &[Component], matching: Matching) -> Result<Self, GitGap> {
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
            arg: os_string(&arg).ok_or(GitGap::Unpassable)?,
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
fn os_string(bytes: &[u8]) -> Option<OsString> {
    use std::os::unix::ffi::OsStrExt as _;
    Some(OsStr::from_bytes(bytes).to_owned())
}

#[cfg(windows)]
fn os_string(bytes: &[u8]) -> Option<OsString> {
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
