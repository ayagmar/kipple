//! `kipple doctor` output and the CLI's broken-pipe contract
//! (`docs/03-functional-spec.md` §2 and §4), on a report built from fixed facts.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kipple::doctor::{ConfigStatus, IntegrationRoots, Probed, Report, RootCheck};
use kipple::theme::Ink;
use kipple::{CANCELLED, exit_code};
use kipple_adapters::builtin::descriptors;
use kipple_core::{
    FileIdentity, HeldPath, IntegrationId, Inventory, Lens, Lineage, Observer, Owners, ProbeError,
    ProcessSnapshot, Protection, Protections, ResolvedRoot, RootLink, RootSource, Roots,
    SymbolicRoot, UnresolvedRoot,
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
            status: Ok(Probed {
                at: cache,
                meta: Err(failure),
            }),
        }],
        integrations: Vec::new(),
        tools: Vec::new(),
        processes: Ok(ProcessSnapshot {
            processes: Vec::new(),
            inventory: Inventory::Complete,
            observer: Observer::PLAIN,
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
    report.write_to(&mut out, &Ink::plain()).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn a_closed_stdout_exits_130_and_says_nothing_more() {
    let mut stderr = Vec::new();

    let written = report(io::ErrorKind::NotFound).write_to(&mut ClosedPipe, &Ink::plain());
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
    if let Ok(Probed { at: cache, .. }) = &mut check.status {
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

// Catches the longest root name filling its column and running into what is said of it.
#[test]
fn a_root_name_as_wide_as_its_column_is_set_apart_from_its_state() {
    let mut report = report(io::ErrorKind::NotFound);
    report.roots = vec![RootCheck {
        root: SymbolicRoot::RoamingAppData,
        status: Err(UnresolvedRoot::NotSet { var: "APPDATA" }),
    }];

    let output = printed(&report);

    assert!(
        output.contains("\n  known.roaming-app-data unknown: "),
        "{output}"
    );
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

/// Catches an unusable `CLAUDE_CONFIG_DIR` leaving kipple free to touch a store it can't
/// locate, or leaving the person unable to tell why nothing can be cleaned.
#[test]
fn malformed_override_holds_every_finding_and_doctor_says_why() {
    let malformed = UnresolvedRoot::NotAbsolute {
        var: "CLAUDE_CONFIG_DIR",
        value: "claude".to_owned(),
    };
    let mut roots = Roots::default();
    let home = PathBuf::from("/home/u");
    let resolved = ResolvedRoot::new(SymbolicRoot::Home, home.clone(), RootSource::Env("HOME"));
    roots.insert(SymbolicRoot::Home, Ok(resolved));
    roots.insert(SymbolicRoot::ClaudeConfig, Err(malformed.clone()));
    let identity = |file| Some(FileIdentity { volume: 1, file });
    let held = |path: &Path| HeldPath {
        forms: vec![path.to_owned()],
        lineage: Lineage(vec![identity(2)]),
        note: None,
    };
    let data = home.join(".local/share/kipple");
    let protections = Protections::of_host(&roots, Some(&data), &descriptors(), &held);

    let finding = home.join("code/app/target");
    let lineage = Lineage(vec![identity(1)]);
    let found = protections.check(
        &finding,
        &lineage,
        IntegrationId("cargo"),
        &Owners::default(),
    );
    assert_eq!(found, Some(Protection::Subresource));

    let mut doctor = report(io::ErrorKind::NotFound);
    doctor.roots.push(RootCheck {
        root: SymbolicRoot::ClaudeConfig,
        status: Err(malformed),
    });
    let output = printed(&doctor);
    assert!(
        output.contains("CLAUDE_CONFIG_DIR is not an absolute path"),
        "expected doctor to name the setting, got {output}"
    );
}
