//! `cargo xtask check-arch`: the crate boundaries from `docs/05-architecture.md` §1.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::cmd;

/// Every dependency allowed between kipple crates. Any other edge is a violation.
/// `xtask` is a dev-only tool and may depend on any crate.
const ALLOWED_EDGES: &[(&str, &str)] = &[
    ("kipple-adapters", "kipple-core"),
    ("kipple-platform", "kipple-core"),
    ("kipple", "kipple-core"),
    ("kipple", "kipple-adapters"),
    ("kipple", "kipple-platform"),
];

const DEV_TOOL: &str = "xtask";

/// The only crate that may contain OS-specific code.
const PLATFORM_CRATE_DIR: &str = "crates/kipple-platform";

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Debug, Deserialize)]
struct Package {
    name: String,
    dependencies: Vec<Dependency>,
}

#[derive(Debug, Deserialize)]
struct Dependency {
    name: String,
    /// `None` for a normal dependency, otherwise `"dev"` or `"build"`.
    kind: Option<String>,
}

/// A Rust source file, with its path relative to the workspace root.
#[derive(Debug)]
struct SourceFile {
    path: PathBuf,
    contents: String,
}

/// Fails if the workspace breaks a crate boundary.
pub(crate) fn run(root: &Path) -> Result<()> {
    let mut violations = edge_violations(&load_metadata(root)?.packages);
    violations.extend(target_os_violations(&rust_sources(root)?));
    if violations.is_empty() {
        return Ok(());
    }
    let mut stderr = io::stderr().lock();
    for violation in &violations {
        writeln!(stderr, "check-arch: {violation}")?;
    }
    bail!(
        "{} architecture violation(s), see docs/05-architecture.md §1",
        violations.len()
    )
}

fn load_metadata(root: &Path) -> Result<Metadata> {
    let mut command = cmd::command("cargo", root);
    command.args(["metadata", "--format-version", "1", "--no-deps", "--locked"]);
    let json = cmd::stdout(&mut command)?;
    serde_json::from_slice(&json).context("could not parse `cargo metadata` output")
}

/// Dependencies between workspace crates that are not in [`ALLOWED_EDGES`], of any kind.
fn edge_violations(packages: &[Package]) -> Vec<String> {
    let members: BTreeSet<&str> = packages.iter().map(|p| p.name.as_str()).collect();
    let mut violations = Vec::new();
    for package in packages.iter().filter(|p| p.name != DEV_TOOL) {
        for dependency in &package.dependencies {
            let edge = (package.name.as_str(), dependency.name.as_str());
            if members.contains(edge.1) && !ALLOWED_EDGES.contains(&edge) {
                let kind = dependency
                    .kind
                    .as_deref()
                    .map_or(String::new(), |k| format!("{k}-"));
                violations.push(format!(
                    "`{}` has a {kind}dependency on `{}`, which is not an allowed edge",
                    edge.0, edge.1
                ));
            }
        }
    }
    violations.sort();
    violations
}

/// Mentions of `target_os` in any crate other than `kipple-platform`.
fn target_os_violations(files: &[SourceFile]) -> Vec<String> {
    let mut violations = Vec::new();
    for file in files
        .iter()
        .filter(|f| !f.path.starts_with(PLATFORM_CRATE_DIR))
    {
        for (index, line) in file.contents.lines().enumerate() {
            if line.contains("target_os") {
                violations.push(format!(
                    "{}:{}: `target_os` is only allowed in {PLATFORM_CRATE_DIR}",
                    file.path.display(),
                    index + 1
                ));
            }
        }
    }
    violations.sort();
    violations
}

/// Every `.rs` file under `crates/`.
fn rust_sources(root: &Path) -> Result<Vec<SourceFile>> {
    let mut files = Vec::new();
    let mut pending = vec![root.join("crates")];
    while let Some(dir) = pending.pop() {
        let entries =
            fs::read_dir(&dir).with_context(|| format!("could not read {}", dir.display()))?;
        for entry in entries {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let contents = fs::read_to_string(&path)
                    .with_context(|| format!("could not read {}", path.display()))?;
                let path = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                files.push(SourceFile { path, contents });
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, dependencies: &[(&str, Option<&str>)]) -> Package {
        Package {
            name: name.to_owned(),
            dependencies: dependencies
                .iter()
                .map(|(dependency, kind)| Dependency {
                    name: (*dependency).to_owned(),
                    kind: kind.map(str::to_owned),
                })
                .collect(),
        }
    }

    fn documented_graph() -> Vec<Package> {
        vec![
            package("kipple-core", &[("serde", None)]),
            package("kipple-platform", &[("kipple-core", None)]),
            package("kipple-adapters", &[("kipple-core", None)]),
            package(
                "kipple",
                &[
                    ("kipple-core", None),
                    ("kipple-platform", None),
                    ("kipple-adapters", None),
                ],
            ),
            package("xtask", &[("kipple", None), ("kipple-core", None)]),
        ]
    }

    fn with_dependency(name: &str, dependency: &str, kind: Option<&str>) -> Vec<Package> {
        let mut packages = documented_graph();
        for package in packages.iter_mut().filter(|p| p.name == name) {
            package.dependencies.push(Dependency {
                name: dependency.to_owned(),
                kind: kind.map(str::to_owned),
            });
        }
        packages
    }

    fn source(path: &str, contents: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            contents: contents.to_owned(),
        }
    }

    #[test]
    fn the_documented_crate_graph_passes() {
        assert_eq!(edge_violations(&documented_graph()), Vec::<String>::new());
    }

    #[test]
    fn core_depending_on_platform_fails() {
        let violations = edge_violations(&with_dependency("kipple-core", "kipple-platform", None));
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("`kipple-core` has a dependency on `kipple-platform`"));
    }

    #[test]
    fn core_dev_dependency_on_a_kipple_crate_fails() {
        let packages = with_dependency("kipple-core", "kipple-adapters", Some("dev"));
        assert_eq!(edge_violations(&packages).len(), 1);
    }

    #[test]
    fn adapters_and_platform_depending_on_each_other_fails() {
        let mut packages = with_dependency("kipple-adapters", "kipple-platform", None);
        packages.extend(with_dependency(
            "kipple-platform",
            "kipple-adapters",
            Some("build"),
        ));
        let violations = edge_violations(&packages);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("`kipple-adapters` has a dependency"))
        );
        assert!(
            violations
                .iter()
                .any(|v| v.contains("`kipple-platform` has a build-dependency"))
        );
    }

    #[test]
    fn a_library_crate_depending_on_the_binary_fails() {
        let packages = with_dependency("kipple-adapters", "kipple", None);
        assert_eq!(edge_violations(&packages).len(), 1);
    }

    #[test]
    fn target_os_outside_platform_fails() {
        let files = [
            source(
                "crates/kipple-core/src/lib.rs",
                "//! Core.\n#[cfg(target_os = \"linux\")]\n",
            ),
            source(
                "crates/kipple/src/main.rs",
                "if cfg!(all(unix, target_os = \"macos\")) {}\n",
            ),
        ];
        let violations = target_os_violations(&files);
        assert_eq!(violations.len(), 2);
        assert!(
            violations
                .iter()
                .any(|v| v.starts_with("crates/kipple-core/src/lib.rs:2:"))
        );
    }

    #[test]
    fn source_discovery_finds_every_crate_relative_to_the_workspace_root() {
        let files = rust_sources(Path::new(crate::WORKSPACE_ROOT)).unwrap();
        let has = |path: &str| files.iter().any(|f| f.path == Path::new(path));
        assert!(has("crates/kipple-core/src/lib.rs"));
        assert!(has("crates/kipple/src/main.rs"));
        assert!(files.iter().any(|f| f.path.starts_with(PLATFORM_CRATE_DIR)));
    }

    #[test]
    fn target_os_inside_platform_passes() {
        let files = [source(
            "crates/kipple-platform/src/linux.rs",
            "#[cfg(target_os = \"linux\")]\n",
        )];
        assert_eq!(target_os_violations(&files), Vec::<String>::new());
    }
}
