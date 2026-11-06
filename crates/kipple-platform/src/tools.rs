//! Finding the native tools a semantic plan runs. Read-only and never cached, so apply
//! sees the present. Until a tool's arm is here it is missing, which holds its items.

use kipple_core::{NativeTool, ResolvedTool, ToolGap, ToolResolver};

use crate::environment::Environment;

/// The platform's [`ToolResolver`]: tools on the `PATH` of the environment it is given.
#[derive(Debug)]
pub struct PathTools {
    #[expect(dead_code, reason = "the PATH each tool's arm walks")]
    env: Environment,
}

impl PathTools {
    /// The resolver for the machine `env` describes.
    #[must_use]
    pub fn new(env: &Environment) -> Self {
        Self { env: env.clone() }
    }
}

impl ToolResolver for PathTools {
    fn resolve(&self, _: NativeTool) -> Result<ResolvedTool, ToolGap> {
        Err(ToolGap::Missing)
    }
}
