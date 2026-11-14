//! Finding the native tools a semantic plan runs. Read-only and never cached, so apply
//! sees the present. Until a tool's arm is here it is missing, which holds its items.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::Path;

use kipple_core::{EntryKind, EntrySnapshot, NativeTool, ResolvedTool, ToolGap, ToolResolver};

use crate::capabilities;
use crate::environment::Environment;
use crate::executor::sys;
use crate::os;

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

/// What the file at `path` is now, its last name read without following a link: its
/// kind, identity and fingerprint, which a run compares right before it starts the tool.
pub(crate) fn tool_file(path: &Path) -> io::Result<EntrySnapshot> {
    let unnamed = || io::Error::from(io::ErrorKind::InvalidInput);
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
