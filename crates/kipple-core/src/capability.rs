//! Native tools the platform found and could run (`docs/05-architecture.md` §1).
//!
//! Presence alone is not a capability: the tool's version probe must succeed. Which
//! versions an integration supports is that integration's decision.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// A native tool kipple knows how to probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativeTool {
    /// Git, for worktree discovery.
    Git,
    /// `paccache` from pacman-contrib, for package cache advice.
    Paccache,
    /// `journalctl` from systemd, for journal advice.
    Journalctl,
}

impl NativeTool {
    /// Every tool, in report order.
    pub const ALL: [Self; 3] = [Self::Git, Self::Paccache, Self::Journalctl];

    /// The executable's name without an extension.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Paccache => "paccache",
            Self::Journalctl => "journalctl",
        }
    }
}

/// A version as dot-separated numbers, such as `2.51.0`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ToolVersion(pub Vec<u64>);

impl fmt::Display for ToolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = self.0.iter();
        if let Some(first) = parts.next() {
            write!(f, "{first}")?;
        }
        parts.try_for_each(|part| write!(f, ".{part}"))
    }
}

/// Why a tool's version probe failed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ToolError {
    /// The tool could not be started.
    #[error("could not start it: {0}")]
    Start(Arc<io::Error>),
    /// Waiting for the tool or reading its output failed.
    #[error("could not wait for it: {0}")]
    Wait(Arc<io::Error>),
    /// The tool did not finish within its deadline and was stopped.
    #[error("it did not finish within {} ms", deadline.as_millis())]
    TimedOut {
        /// The deadline it missed.
        deadline: Duration,
    },
    /// The tool exited unsuccessfully.
    #[error("it failed ({}): {stderr}", code.map_or_else(|| "no exit code".to_owned(), |c| format!("exit code {c}")))]
    Failed {
        /// The exit code, if it exited normally.
        code: Option<i32>,
        /// The start of its stderr, lossily decoded.
        stderr: String,
    },
    /// The output did not contain a version kipple recognises.
    #[error("unrecognised version output {output:?}")]
    UnrecognisedOutput {
        /// The first line of its output.
        output: String,
    },
}

/// What probing one native tool found.
#[derive(Debug, Clone)]
pub enum ToolStatus {
    /// No executable of that name on the search path.
    NotFound,
    /// An executable was found and its version probed.
    Found {
        /// The executable that was run.
        path: PathBuf,
        /// Its version, or why the probe failed.
        version: Result<ToolVersion, ToolError>,
    },
}
