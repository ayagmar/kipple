//! Native tool capabilities (`docs/05-architecture.md` §1): a tool counts only once its
//! read-only version probe succeeded.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use kipple_core::{NativeTool, ToolError, ToolStatus, ToolVersion};

use crate::environment::Environment;
use crate::os;
use crate::runner::{self, Output};

/// How long a `--version` probe may take.
const VERSION_DEADLINE: Duration = Duration::from_secs(2);

/// Finds each native tool on `PATH` and probes its version.
#[must_use]
pub fn probe_tools(env: &Environment) -> Vec<(NativeTool, ToolStatus)> {
    NativeTool::ALL
        .into_iter()
        .map(|tool| (tool, probe(tool, env)))
        .collect()
}

fn probe(tool: NativeTool, env: &Environment) -> ToolStatus {
    let Some(path) = find_on_path(tool.program(), env) else {
        return ToolStatus::NotFound;
    };
    let version = runner::run(&path, &["--version"], env, VERSION_DEADLINE)
        .and_then(|output| version_of(tool, &output));
    ToolStatus::Found { path, version }
}

/// The first executable regular file named `program` in an absolute `PATH` entry.
/// Relative entries are skipped: they would depend on the current directory.
fn find_on_path(program: &str, env: &Environment) -> Option<PathBuf> {
    let file_name = format!("{program}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(env.get("PATH")?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(&file_name))
        .find(|candidate| {
            fs::metadata(candidate).is_ok_and(|meta| meta.is_file() && os::is_executable(&meta))
        })
}

fn version_of(tool: NativeTool, output: &Output) -> Result<ToolVersion, ToolError> {
    if !output.status.success() {
        return Err(ToolError::Failed {
            code: output.status.code(),
            stderr: first_line(&output.stderr),
        });
    }
    let line = first_line(&output.stdout);
    parse_version(tool, &line).ok_or(ToolError::UnrecognisedOutput { output: line })
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The version in the first line of `tool --version`. Only the leading numeric
/// components count: `2.51.0.windows.1` is `2.51.0`.
fn parse_version(tool: NativeTool, line: &str) -> Option<ToolVersion> {
    let prefix = match tool {
        NativeTool::Git => "git version ",
        NativeTool::Paccache => "paccache ",
        NativeTool::Journalctl => "systemd ",
    };
    let token = line.strip_prefix(prefix)?.split_whitespace().next()?;
    let token = token.strip_prefix('v').unwrap_or(token);
    let parts: Vec<u64> = token
        .split('.')
        .map_while(|part| part.parse().ok())
        .collect();
    (!parts.is_empty()).then_some(ToolVersion(parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(tool: NativeTool, line: &str) -> Option<String> {
        parse_version(tool, line).map(|v| v.to_string())
    }

    #[test]
    fn versions_parse_from_what_each_tool_prints_on_every_os() {
        let git = |line| version(NativeTool::Git, line);
        assert_eq!(git("git version 2.56.0").as_deref(), Some("2.56.0"));
        assert_eq!(
            git("git version 2.51.0.windows.1").as_deref(),
            Some("2.51.0")
        );
        assert_eq!(
            git("git version 2.39.5 (Apple Git-154)").as_deref(),
            Some("2.39.5")
        );
        let journal = version(NativeTool::Journalctl, "systemd 262 (262-1-arch)");
        assert_eq!(journal.as_deref(), Some("262"));
        let paccache = version(NativeTool::Paccache, "paccache v1.10.0");
        assert_eq!(paccache.as_deref(), Some("1.10.0"));
    }

    #[test]
    fn output_from_a_different_program_is_not_a_version() {
        assert_eq!(version(NativeTool::Git, "hub version 2.14.2"), None);
        assert_eq!(version(NativeTool::Journalctl, "systemd"), None);
        assert_eq!(
            version(NativeTool::Paccache, "paccache: unknown option"),
            None
        );
    }
}
