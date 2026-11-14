//! Finding the native tools a semantic plan runs. Read-only and never cached, so apply
//! sees the present. Until a tool's arm is here it is missing, which holds its items.

pub(crate) mod npmrc;

use std::fs::{self, File};
use std::io::{self, ErrorKind, Read as _};
use std::path::{Path, PathBuf};

use kipple_core::{EntryKind, EntrySnapshot, NativeTool, ResolvedTool, ToolGap, ToolResolver};

use crate::capabilities;
use crate::environment::Environment;
use crate::executor::sys;
use crate::os;

/// The npm versions kipple was verified against, each with a contract test and a layout
/// fixture. The npm integration lists the same ones; a version is one row in each.
const NPM_VERIFIED: [&str; 2] = ["11.20.0", "12.2.1"];

/// The most of npm's `package.json` read. A larger one is unreadable.
const MANIFEST_LIMIT: u64 = 1024 * 1024;

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

impl ToolResolver for PathTools {
    fn resolve(&self, tool: NativeTool) -> Result<ResolvedTool, ToolGap> {
        match tool {
            NativeTool::Git => self.git(),
            NativeTool::Npm | NativeTool::Paccache | NativeTool::Journalctl => {
                Err(ToolGap::Missing)
            }
        }
    }
}

/// An npm found on `PATH`, read from its files alone.
#[derive(Debug, Clone)]
pub(crate) struct NpmInstall {
    /// Where its package and node are.
    pub(crate) located: npmrc::Install,
    /// The version its `package.json` gives.
    version: String,
}

impl NpmInstall {
    fn is_verified(&self) -> bool {
        NPM_VERIFIED.contains(&self.version.as_str())
    }
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
        version: manifest.version,
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
    Ok(Manifest {
        version: version.to_owned(),
    })
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
