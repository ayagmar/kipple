//! Finding the native tools a semantic plan runs. Read-only and never cached, so apply
//! sees the present. Until a tool's arm is here it is missing, which holds its items.

pub(crate) mod npmrc;

use std::fs::{self, File};
use std::io::{self, ErrorKind, Read as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kipple_core::{
    EntryKind, EntrySnapshot, NativeTool, ResolvedTool, ToolFile, ToolGap, ToolResolver,
};

use crate::capabilities;
use crate::environment::Environment;
use crate::executor::sys;
use crate::os;
use crate::runner;

/// The npm versions kipple was verified against, each with a contract test and a layout
/// fixture. The npm integration lists the same ones; a version is one row in each.
const NPM_VERIFIED: [&str; 2] = ["11.20.0", "12.2.1"];

/// The most of npm's `package.json` read. A larger one is unreadable.
const MANIFEST_LIMIT: u64 = 1024 * 1024;

/// How long `<node> --version` may take.
const NODE_VERSION_DEADLINE: Duration = Duration::from_secs(2);

/// The platform's [`ToolResolver`]: tools on the `PATH` of the environment it is given.
#[derive(Debug)]
pub struct PathTools {
    env: Environment,
}

impl PathTools {
    /// The resolver for the machine `env` describes.
    #[must_use]
    pub fn new(env: &Environment) -> Self {
        Self { env: env.clone() }
    }

    /// git: the first on the absolute entries of `PATH`, as `doctor` finds it, named by
    /// the file it leads to so the runner checks and starts that file, with what it says
    /// its version is. A script naming its interpreter is refused before anything runs
    /// it: a wrapper or a version manager's shim would start a shell (pinned 27). Whether
    /// that version may run a command is the integration's to say.
    fn git(&self) -> Result<ResolvedTool, ToolGap> {
        let tool = NativeTool::Git;
        let found =
            capabilities::find_on_path(tool.program(), &self.env).ok_or(ToolGap::Missing)?;
        let path = os::physical(&found).map_err(|_| ToolGap::Unreadable)?;
        let file = tool_file(&path).map_err(|_| ToolGap::Unreadable)?;
        if file.kind != EntryKind::File {
            return Err(ToolGap::Unreadable);
        }
        if names_interpreter(&path).map_err(|_| ToolGap::Unreadable)? {
            return Err(ToolGap::Script);
        }
        let version =
            capabilities::version(tool, &path, &self.env).map_err(|_| ToolGap::UnknownVersion)?;
        Ok(ResolvedTool {
            tool,
            path,
            identity: file.identity,
            fingerprint: file.fingerprint,
            version: version.to_string(),
            runtime: None,
        })
    }
}

impl PathTools {
    /// npm: the first npm on the absolute entries of `PATH` that is a version kipple
    /// verified, as the pair that runs it: its `npm-cli.js` and the node of its own prefix,
    /// each named by the file it is. Nothing of npm runs: its version and the node versions
    /// it supports are read from its `package.json`, and the one process started is
    /// `<node> --version`, after the node is known not to be a script.
    fn npm(&self) -> Result<ResolvedTool, ToolGap> {
        let installs = npm_installs(&self.env);
        let verified = installs
            .iter()
            .flatten()
            .find(|install| install.is_verified());
        let Some(install) = verified else {
            return Err(npm_gap(&installs));
        };
        let script = tool_file(&install.script).map_err(|_| ToolGap::Unreadable)?;
        let node = tool_file(&install.located.node).map_err(|_| ToolGap::Unreadable)?;
        if script.kind != EntryKind::File || node.kind != EntryKind::File {
            return Err(ToolGap::Unreadable);
        }
        if names_interpreter(&install.located.node).map_err(|_| ToolGap::Unreadable)? {
            return Err(ToolGap::Script);
        }
        let engines = install.engines.as_deref().ok_or(ToolGap::UnknownVersion)?;
        let version = node_version(&install.located.node, &self.env)?;
        if !engines::satisfied(engines, version).ok_or(ToolGap::UnknownVersion)? {
            return Err(ToolGap::UnverifiedVersion);
        }
        Ok(ResolvedTool {
            tool: NativeTool::Npm,
            path: install.script.clone(),
            identity: script.identity,
            fingerprint: script.fingerprint,
            version: install.version.clone(),
            runtime: Some(ToolFile {
                path: install.located.node.clone(),
                identity: node.identity,
                fingerprint: node.fingerprint,
            }),
        })
    }
}

impl ToolResolver for PathTools {
    fn resolve(&self, tool: NativeTool) -> Result<ResolvedTool, ToolGap> {
        match tool {
            NativeTool::Git => self.git(),
            NativeTool::Npm => self.npm(),
            NativeTool::Paccache | NativeTool::Journalctl => Err(ToolGap::Missing),
        }
    }
}

/// An npm found on `PATH`, read from its files alone.
#[derive(Debug, Clone)]
pub(crate) struct NpmInstall {
    /// Where its package and node are.
    pub(crate) located: npmrc::Install,
    /// Its `npm-cli.js`, as the file it is.
    script: PathBuf,
    /// The version its `package.json` gives.
    version: String,
    /// The node versions it supports, as its `package.json` gives them.
    engines: Option<String>,
}

impl NpmInstall {
    fn is_verified(&self) -> bool {
        NPM_VERIFIED.contains(&self.version.as_str())
    }
}

/// Why no npm on `PATH` is one kipple verified: the version of one that is readable, else
/// what was wrong with the first, else none at all.
fn npm_gap(installs: &[Result<NpmInstall, ToolGap>]) -> ToolGap {
    if installs.iter().any(Result::is_ok) {
        return ToolGap::UnverifiedVersion;
    }
    installs
        .iter()
        .find_map(|install| install.as_ref().err().copied())
        .unwrap_or(ToolGap::Missing)
}

/// The npm whose configuration places the cache: the first on `PATH` that kipple verified,
/// else the first it can read, so a cache stays placed, and closed, for an npm it doesn't
/// run. Reads files only.
pub(crate) fn configured_npm(env: &Environment) -> Option<npmrc::Install> {
    let installs: Vec<NpmInstall> = npm_installs(env).into_iter().flatten().collect();
    let verified = installs.iter().find(|install| install.is_verified());
    verified
        .or_else(|| installs.first())
        .map(|install| install.located.clone())
}

/// What each `npm` on the absolute entries of `PATH` is, in `PATH`'s order.
fn npm_installs(env: &Environment) -> Vec<Result<NpmInstall, ToolGap>> {
    let name = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let Some(path) = env.get("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .filter(|candidate| {
            fs::metadata(candidate).is_ok_and(|meta| meta.is_file() && os::is_executable(&meta))
        })
        .map(|candidate| npm_install(&candidate))
        .collect()
}

/// The npm `found` on `PATH` leads to: `npm-cli.js` in a package under `lib/node_modules`,
/// or a `bin/npm` beside such a package, as an install keeps a wrapper (on Windows,
/// `npm.cmd` beside `node_modules`). A version manager's shim or any other program is
/// refused as a script that starts another program: kipple can't tell which npm it picks.
fn npm_install(found: &Path) -> Result<NpmInstall, ToolGap> {
    let physical = os::physical(found).map_err(|_| ToolGap::Unreadable)?;
    let (prefix, script) = npm_layout(&physical).ok_or(ToolGap::Script)?;
    let script = os::physical(&script).map_err(|error| absent_or(&error, ToolGap::Script))?;
    let package = script
        .ancestors()
        .nth(2)
        .filter(|_| script.ends_with(Path::new("node_modules/npm/bin/npm-cli.js")))
        .ok_or(ToolGap::Script)?
        .to_path_buf();
    let node = if cfg!(windows) {
        prefix.join("node.exe")
    } else {
        prefix.join("bin").join("node")
    };
    let node = os::physical(&node).map_err(|error| absent_or(&error, ToolGap::Missing))?;
    let manifest = manifest(&package)?;
    Ok(NpmInstall {
        located: npmrc::Install { package, node },
        script,
        version: manifest.version,
        engines: manifest.engines,
    })
}

/// `absent` when `error` says there is no such file, else unreadable.
fn absent_or(error: &io::Error, absent: ToolGap) -> ToolGap {
    if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) {
        absent
    } else {
        ToolGap::Unreadable
    }
}

/// The prefix of the install `physical` belongs to, and where its `npm-cli.js` should be.
fn npm_layout(physical: &Path) -> Option<(PathBuf, PathBuf)> {
    if cfg!(windows) {
        let prefix = physical.parent()?;
        let script = prefix.join("node_modules/npm/bin/npm-cli.js");
        return Some((prefix.to_path_buf(), script));
    }
    if physical.ends_with(Path::new("lib/node_modules/npm/bin/npm-cli.js")) {
        let prefix = physical.ancestors().nth(5)?;
        return Some((prefix.to_path_buf(), physical.to_path_buf()));
    }
    let bin = physical.parent()?;
    let named = |path: &Path, name: &str| path.file_name().is_some_and(|own| own == name);
    if !named(physical, "npm") || !named(bin, "bin") {
        return None;
    }
    let prefix = bin.parent()?;
    let script = prefix.join("lib/node_modules/npm/bin/npm-cli.js");
    Some((prefix.to_path_buf(), script))
}

/// What npm's `package.json` says.
struct Manifest {
    version: String,
    engines: Option<String>,
}

/// npm's `package.json` in `package`: its `name` must be `npm`, and its `version` text.
fn manifest(package: &Path) -> Result<Manifest, ToolGap> {
    let mut bytes = Vec::new();
    File::open(package.join("package.json"))
        .and_then(|file| file.take(MANIFEST_LIMIT + 1).read_to_end(&mut bytes))
        .map_err(|_| ToolGap::Unreadable)?;
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return Err(ToolGap::Unreadable);
    }
    let json: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| ToolGap::UnknownVersion)?;
    if json.get("name").and_then(serde_json::Value::as_str) != Some("npm") {
        return Err(ToolGap::Unreadable);
    }
    let version = json
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or(ToolGap::UnknownVersion)?;
    let engines = json
        .pointer("/engines/node")
        .and_then(serde_json::Value::as_str);
    Ok(Manifest {
        version: version.to_owned(),
        engines: engines.map(str::to_owned),
    })
}

/// What `<node> --version` prints: `v` and three numbers.
fn node_version(node: &Path, env: &Environment) -> Result<engines::Version, ToolGap> {
    let output = runner::run(node, &["--version"], env, NODE_VERSION_DEADLINE)
        .map_err(|_| ToolGap::UnknownVersion)?;
    if !output.status.success() {
        return Err(ToolGap::UnknownVersion);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next().unwrap_or_default().trim();
    line.strip_prefix('v')
        .and_then(engines::version)
        .ok_or(ToolGap::UnknownVersion)
}

/// The subset of npm's version ranges `engines` fields use: alternatives joined by `||`,
/// each a list of comparators (`^`, `~`, `>=`, `>`, `<=`, `<`, `=` or none) on a version
/// of three numbers. Anything else is unknown.
mod engines {
    /// A version of three numbers.
    pub(super) type Version = [u64; 3];

    /// `text` as three numbers, nothing else.
    pub(super) fn version(text: &str) -> Option<Version> {
        let mut parts = text.split('.').map(|part| {
            let digits = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
            digits.then(|| part.parse().ok()).flatten()
        });
        let version = [parts.next()??, parts.next()??, parts.next()??];
        parts.next().is_none().then_some(version)
    }

    /// Whether `version` is in `range`, `None` when `range` isn't one this reads.
    pub(super) fn satisfied(range: &str, version: Version) -> Option<bool> {
        let mut any = false;
        for alternative in range.split("||") {
            let mut all = true;
            let mut comparators = alternative.split_whitespace().peekable();
            comparators.peek()?;
            for comparator in comparators {
                all &= holds(comparator, version)?;
            }
            any |= all;
        }
        Some(any)
    }

    fn holds(comparator: &str, version: Version) -> Option<bool> {
        let operators = [">=", "<=", ">", "<", "^", "~", "="];
        let (operator, rest) = operators
            .into_iter()
            .find_map(|op| comparator.strip_prefix(op).map(|rest| (op, rest)))
            .unwrap_or(("=", comparator));
        let bound = self::version(rest)?;
        Some(match operator {
            ">=" => version >= bound,
            "<=" => version <= bound,
            ">" => version > bound,
            "<" => version < bound,
            "^" => version >= bound && version < caret_ceiling(bound),
            "~" => version >= bound && version < [bound[0], bound[1] + 1, 0],
            _ => version == bound,
        })
    }

    /// The first version `^bound` excludes: the next of its first number that isn't 0.
    const fn caret_ceiling(bound: Version) -> Version {
        match bound {
            [0, 0, patch] => [0, 0, patch + 1],
            [0, minor, _] => [0, minor + 1, 0],
            [major, _, _] => [major + 1, 0, 0],
        }
    }
}

/// What the file at `path` is now, its last name read without following a link: its
/// kind, identity and fingerprint, which a run compares right before it starts the tool.
pub(crate) fn tool_file(path: &Path) -> io::Result<EntrySnapshot> {
    let unnamed = || io::Error::from(ErrorKind::InvalidInput);
    let parent = path.parent().ok_or_else(unnamed)?;
    let name = sys::name(path.file_name().ok_or_else(unnamed)?)?;
    Ok(sys::Dir::open_followed(parent)?.stat(&name)?.snapshot)
}

/// Whether the file at `path` starts with `#!`, so the kernel would start the program it
/// names instead.
fn names_interpreter(path: &Path) -> io::Result<bool> {
    let mut head = Vec::with_capacity(2);
    File::open(path)?.take(2).read_to_end(&mut head)?;
    Ok(head == b"#!")
}

#[cfg(all(test, unix))]
mod tests;
