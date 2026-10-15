//! `kipple doctor` output and the CLI's broken-pipe contract
//! (`docs/03-functional-spec.md` §2 and §4), on a report built from fixed facts.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use kipple::doctor::{ConfigStatus, IntegrationRoots, Report, RootCheck};
use kipple::{CANCELLED, exit_code};
use kipple_core::{
    FileIdentity, IntegrationId, Inventory, Lens, ProbeError, ProcessSnapshot, ResolvedRoot,
    RootLink, RootSource, SymbolicRoot, UnresolvedRoot,
};
use kipple_platform::{KippleDir, KippleDirs};

const HINT: &str = "grant Full Disk Access";

/// A stdout whose reader has gone, as with `kipple doctor | head -1`.
struct ClosedPipe;

impl Write for ClosedPipe {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
}

/// A report whose cache root failed to open with `cache_error`.
fn report(cache_error: io::ErrorKind) -> Report {
    let unresolved = |base| KippleDir {
        base,
        path: Err(UnresolvedRoot::NotSet { var: "HOME" }),
    };
    let cache = ResolvedRoot::new(
        SymbolicRoot::XdgCache,
        PathBuf::from("/home/u/.cache"),
        RootSource::Env("XDG_CACHE_HOME"),
    );
    let failure = ProbeError::io(&cache.path, cache_error.into());
    Report {
        dirs: KippleDirs {
            config: unresolved(SymbolicRoot::XdgConfig),
            data: unresolved(SymbolicRoot::XdgData),
            cache: unresolved(SymbolicRoot::XdgCache),
        },
        config_file: None,
        config: ConfigStatus::default(),
        roots: vec![RootCheck {
            root: SymbolicRoot::XdgCache,
            status: Ok((cache, Err(failure))),
        }],
        integrations: Vec::new(),
        tools: Vec::new(),
        processes: Ok(ProcessSnapshot {
            processes: Vec::new(),
            inventory: Inventory::Complete,
        }),
        permission_hint: Some(HINT),
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn printed(report: &Report) -> String {
    let mut out = Vec::new();
    report.write_to(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn a_closed_stdout_exits_130_and_says_nothing_more() {
    let mut stderr = Vec::new();

    let written = report(io::ErrorKind::NotFound).write_to(&mut ClosedPipe);
    let code = exit_code(written, &mut stderr);

    assert_eq!(code, ExitCode::from(CANCELLED));
    assert_eq!(String::from_utf8(stderr).unwrap(), "");
}

#[test]
fn a_root_kipple_may_not_read_shows_the_os_error_and_the_permission_hint() {
    let denied = io::Error::from(io::ErrorKind::PermissionDenied).to_string();

    let output = printed(&report(io::ErrorKind::PermissionDenied));

    let shown = format!(
        "can't be read: {}: {denied}",
        PathBuf::from("/home/u/.cache").display()
    );
    assert!(output.contains(&shown), "{output}");
    assert!(output.contains(HINT), "{output}");
}

#[test]
fn the_permission_hint_appears_only_after_a_permission_failure() {
    let output = printed(&report(io::ErrorKind::NotFound));

    assert!(
        output.contains("(from XDG_CACHE_HOME): not present"),
        "{output}"
    );
    assert!(!output.contains(HINT), "{output}");
}

#[test]
fn a_root_resolved_through_a_link_shows_the_link_and_where_it_leads() {
    let mut report = report(io::ErrorKind::NotFound);
    let check = &mut report.roots[0];
    if let Ok((cache, _)) = &mut check.status {
        cache.link = Some(RootLink {
            configured: cache.path.clone(),
            identity: FileIdentity { volume: 1, file: 2 },
        });
        cache.path = PathBuf::from("/mnt/ssd/cache");
    }

    let output = printed(&report);

    let shown = format!(
        "{} -> {}  (from XDG_CACHE_HOME)",
        PathBuf::from("/home/u/.cache").display(),
        PathBuf::from("/mnt/ssd/cache").display()
    );
    assert!(output.contains(&shown), "{output}");
}

#[test]
fn each_integration_is_listed_with_the_roots_it_reads_in_its_own_column() {
    let mut report = report(io::ErrorKind::NotFound);
    report.integrations = vec![IntegrationRoots {
        id: IntegrationId("claude-code"),
        lens: Lens::Agents,
        supported: true,
        roots: vec![SymbolicRoot::ClaudeConfig],
    }];

    let output = printed(&report);

    assert!(
        output.contains("\n  claude-code reads claude.config\n"),
        "{output}"
    );
}
