//! Finding the native tools a semantic plan runs. Read-only and never cached, so apply
//! sees the present. Until a tool's arm is here it is missing, which holds its items.

use std::fs;

use kipple_core::{NativeTool, ResolvedTool, ToolGap, ToolResolver};

use crate::capabilities;
use crate::environment::Environment;
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
    /// its version is. Whether that version may run a command is the integration's to say.
    fn git(&self) -> Result<ResolvedTool, ToolGap> {
        let tool = NativeTool::Git;
        let found =
            capabilities::find_on_path(tool.program(), &self.env).ok_or(ToolGap::Missing)?;
        let path = os::physical(&found).map_err(|_| ToolGap::Unreadable)?;
        let meta = fs::metadata(&path).map_err(|_| ToolGap::Unreadable)?;
        let identity = os::identity(&path, &meta).ok_or(ToolGap::Unreadable)?;
        let version =
            capabilities::version(tool, &path, &self.env).map_err(|_| ToolGap::UnknownVersion)?;
        Ok(ResolvedTool {
            tool,
            path,
            identity,
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
