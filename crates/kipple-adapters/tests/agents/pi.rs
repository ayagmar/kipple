//! Pi's logs and store: S5 Pi §7 and pinned 35.

use std::fs;

use kipple_core::{
    Activity, Caution, EnvPath, FactValue, Hold, IntegrationId, Level, OwnerStatus, Platform,
    Proposal, Protection, SymbolicRoot, Unread, Verdict,
};

use super::host::{DAY, Generated, Host, Names, Scan, running};
use super::pi_v1_5_0::{self as fixture, MARKER, VERSION};
use super::tree::{FixtureTree, Holds};

const LOGS: &str = "builtin.pi.closed-logs";
const STORE: &str = "builtin.pi.store-inventory";

const PI: IntegrationId = IntegrationId("pi");
const AGENT: &str = "home/.pi/agent";

const SUPPORTED: Option<FactValue> = Some(FactValue::Enum("supported"));
const UNKNOWN_LAYOUT: Option<FactValue> = Some(FactValue::Enum("unknown"));
const UNKNOWN: Option<FactValue> = Some(FactValue::Unknown);

fn at(name: &str) -> String {
    format!("{AGENT}/{name}")
}

fn debug_log() -> String {
    at("pi-debug.log")
}

/// A managed install of the verified version.
fn managed() -> FixtureTree {
    let mut tree = FixtureTree::new();
    fixture::store(&mut tree, AGENT);
    tree
}

fn scan(tree: FixtureTree) -> Scan {
    Host::new(tree).scan()
}

/// A store with `contents` written over `name` in `install/`, scanned.
fn scan_with(name: &str, contents: &[u8]) -> Scan {
    let mut tree = managed();
    tree.file(&at(&format!("install/{name}")), contents);
    scan(tree)
}

/// What any store but a verified one must come to: no candidate, its root closed, and the
/// directory reported whole.
fn assert_closed(scan: &Scan) {
    assert_eq!(scan.found(LOGS), [""; 0]);
    assert_eq!(scan.found(STORE), [AGENT]);
    assert_eq!(scan.fact(AGENT, "layout"), UNKNOWN_LAYOUT);
    assert_eq!(scan.owner(PI), OwnerStatus::Unsupported);
    assert_eq!(
        scan.protection(&debug_log(), PI),
        Some(Protection::Subresource)
    );
}

/// Catches a log of the wrong store, or none, being offered, or a log that should go being
/// held: each of the four is one finding, reviewed, and let through by the host.
#[test]
fn a_managed_store_offers_its_four_logs() {
    let scan = scan(managed());

    let logs = [
        "mcp.log.1",
        "pi-debug.log",
        "pi-tui-crash.log",
        "pi-tui-debug.log",
    ];
    assert_eq!(scan.found(LOGS), logs.map(at));
    for log in logs.map(at) {
        assert_eq!(scan.fact(&log, "layout"), SUPPORTED);
        assert_eq!(scan.fact(&log, "pi.version_supported"), SUPPORTED);
        assert_eq!(
            scan.fact(&log, "installer_activity"),
            Some(FactValue::Enum("idle"))
        );
        assert_eq!(scan.protection(&log, PI), None);
        for level in Level::ALL {
            assert!(
                matches!(scan.verdict_at(&log, *level), Verdict::Review(_)),
                "expected {log} in Review at {level}"
            );
        }
    }
}

/// Catches a store with no install marker being trusted: an npm install writes the same logs
/// and kipple can't tell what else it holds.
#[test]
fn non_managed_store_keeps_its_logs() {
    let mut tree = FixtureTree::new();
    fixture::logs(&mut tree, AGENT);

    assert_closed(&scan(tree));
}

/// Catches a marker that isn't this layout's reading as one: missing, cut short, or a later
/// layout's.
#[test]
fn an_unknown_marker_closes_the_store() {
    let mut missing = FixtureTree::new();
    fixture::logs(&mut missing, AGENT);
    missing.file(&at("install/current-version"), b"1.5.0\n");
    assert_closed(&scan(missing));

    assert_closed(&scan_with("managed-install.json", b"{\"kind\":"));
    let later = MARKER.replace("releases-v1", "releases-v2");
    assert_closed(&scan_with("managed-install.json", later.as_bytes()));
}

/// Catches Windows' PowerShell marker being refused: its BOM is part of a valid file.
#[test]
fn a_marker_with_a_bom_is_accepted() {
    let bytes = [b"\xef\xbb\xbf", MARKER.as_bytes()].concat();
    let scan = scan_with("managed-install.json", &bytes);

    assert_eq!(scan.fact(&debug_log(), "layout"), SUPPORTED);
}

/// Catches an active version that can't be named, or wasn't verified, passing as this one.
#[test]
fn an_unknown_current_version_closes_the_store() {
    for (contents, version) in [
        (&b""[..], UNKNOWN),
        (b"..\n", UNKNOWN),
        (b"9.9.9\n", Some(FactValue::Enum("unsupported"))),
    ] {
        let scan = scan_with("current-version", contents);

        assert_closed(&scan);
        assert_eq!(scan.fact(AGENT, "pi.version_supported"), version);
    }
}

/// Catches an update stopped halfway passing as an install: the version file is ahead of
/// what it points at.
#[test]
fn a_partial_install_closes_the_store() {
    let mut tree = FixtureTree::new();
    fixture::logs(&mut tree, AGENT);
    tree.file(&at("install/managed-install.json"), MARKER.as_bytes())
        .file(&at("install/current-version"), b"1.5.0\n")
        .file(&at("install/releases/1.5.0/package.json"), b"{}\n");

    assert_closed(&scan(tree));
}

/// Catches an older store's leftovers being opened, or failing the scan.
#[test]
fn a_legacy_tool_stays_closed() {
    let mut tree = managed();
    tree.file(&at("tools/fd"), b"\x7fELF");

    let scan = scan(tree);

    assert_eq!(scan.omissions(PI).len(), 0, "expected no omission");
    assert_eq!(
        scan.protection(&at("tools/fd"), PI),
        Some(Protection::Subresource)
    );
}

/// Catches a log being taken in the middle of an update, which is when Pi's installer owns
/// the store.
#[test]
fn update_lock_blocks() {
    let mut tree = managed();
    tree.dir(&at("install/update.lock"));

    let scan = scan(tree);

    assert_eq!(
        scan.fact(&debug_log(), "installer_activity"),
        Some(FactValue::Enum("busy"))
    );
    assert_eq!(scan.holds(&debug_log()), [Hold::FactUnmet]);
}

/// Catches an update that starts between the scan and the removal being missed.
#[test]
fn an_update_started_after_the_scan_holds_at_apply() {
    let scan = scan(managed());
    assert_eq!(
        scan.now(&debug_log()).map(|now| now.unmet.len()).ok(),
        Some(0)
    );

    fs::create_dir(scan.path(&at("install/update.lock"))).unwrap();

    let now = scan.now(&debug_log()).ok().unwrap();
    let activity = now.supplied.values.get("installer_activity").copied();
    assert_eq!(activity, Some(FactValue::Enum("busy")));
    assert_eq!(now.unmet.len(), 1, "expected installer_activity unmet");
}

/// Catches apply trusting the scan's store once an update swapped the version.
#[test]
fn a_version_changed_after_the_scan_holds_at_apply() {
    let scan = scan(managed());

    fs::write(scan.path(&at("install/current-version")), b"1.6.0\n").unwrap();

    let now = scan.now(&debug_log()).ok().unwrap();
    assert_eq!(now.supplied.values.get("layout").copied(), UNKNOWN_LAYOUT);
    assert!(
        !now.unmet.is_empty(),
        "expected the layout unmet, got no unmet predicate"
    );
}

/// Catches a Pi that started after the scan not counting.
#[test]
fn a_new_pi_holds_at_apply() {
    let scan = scan(managed());

    scan.show(|processes| processes.running.push(running(6003, "pi", "/usr/bin/node")));

    let now = scan.now(&debug_log()).ok().unwrap();
    assert!(
        matches!(now.activity, Activity::InUse { pid: 6003, .. }),
        "expected Pi in use, got {:?}",
        now.activity
    );
}

/// Catches what sits in `releases/` being lost from the report or proposed: a name outside
/// the grammar is reported as unknown, and an inactive release never goes, since which one
/// runs can't be proved.
#[test]
fn inactive_releases_are_only_reported() {
    let mut tree = managed();
    tree.file(&at("install/releases/not-a-version/x"), b"\n");

    let scan = scan(tree);

    for release in ["1.4.0", "not-a-version"] {
        let release = at(&format!("install/releases/{release}"));
        assert_eq!(scan.proposal(&release), Proposal::Report);
    }
    assert_eq!(
        scan.holds(&at("install/releases/1.4.0")),
        [Hold::ReportOnly]
    );
}

/// Catches a link or a directory of a log's name being taken for the log: Pi writes neither,
/// so what's there is someone else's.
#[test]
fn a_linked_log_or_directory_is_no_candidate() {
    let mut linked = FixtureTree::new();
    fixture::install(&mut linked, AGENT, VERSION);
    linked.file("home/mine.txt", b"mine\n").link(
        &debug_log(),
        "home/mine.txt",
        Holds::AbsoluteInTree,
    );
    let mut directory = FixtureTree::new();
    fixture::install(&mut directory, AGENT, VERSION);
    directory.file(&at("pi-debug.log/inner"), b"\n");

    for tree in [linked, directory] {
        assert_eq!(scan(tree).found(LOGS), [""; 0]);
    }
}

/// Catches a `*.log` match: another tool's log beside the store, or a log Pi doesn't name.
#[test]
fn third_party_log_beside_the_store_is_no_candidate() {
    let mut tree = managed();
    tree.file("home/.pi/logs/superpowers-plus.log", b"\n")
        .file(&at("foo.log"), b"\n");

    let scan = scan(tree);

    assert_eq!(scan.found(LOGS).len(), 4, "expected the four logs alone");
    assert_eq!(
        scan.protection(&at("foo.log"), PI),
        Some(Protection::Subresource)
    );
    assert_eq!(
        scan.protection("home/.pi/logs/superpowers-plus.log", PI),
        Some(Protection::Subresource)
    );
}

/// Catches a name the rule doesn't list reaching it on a disk that folds case: the probe of
/// `pi-debug.log` lands on this file, and the lowercase path it reports is one the host's
/// exact area opens. Pinned 35 names literal names.
#[test]
fn a_log_in_another_case_is_no_candidate() {
    let mut tree = FixtureTree::new();
    fixture::install(&mut tree, AGENT, VERSION);
    tree.file(&at("PI-DEBUG.LOG"), b"not pi's\n");
    let mut host = Host::new(tree);
    host.names = Names::Folded;

    let scan = host.scan();

    assert_eq!(scan.found(LOGS), [""; 0]);
    assert_eq!(
        scan.protection(&at("PI-DEBUG.LOG"), PI),
        Some(Protection::Subresource)
    );
}

/// Catches the active MCP log being removed under a process that's writing it.
#[test]
fn only_the_rotated_mcp_log_is_eligible() {
    let scan = scan(managed());

    assert_eq!(scan.finding(&at("mcp.log")).rule.0, STORE);
    assert_eq!(scan.finding(&at("mcp.log.1")).rule.0, LOGS);
    assert_eq!(
        scan.protection(&at("mcp.log"), PI),
        Some(Protection::Subresource)
    );
}

/// Catches a log just written being treated as an old one.
#[test]
fn a_fresh_log_is_reviewed_as_recent() {
    let mut host = Host::new(managed());
    host.tree.modified(&debug_log(), host.now - DAY);

    let scan = host.scan();

    assert!(
        matches!(scan.verdict(&debug_log()), Verdict::Review(ref why) if why.contains(&Caution::Recent)),
        "expected Review as recent, got {:?}",
        scan.verdict(&debug_log())
    );
}

/// Catches Pi running under Node going unseen: it shows only by the title it gives itself.
#[test]
fn a_process_titled_pi_holds() {
    let mut host = Host::new(managed());
    host.processes
        .running
        .push(running(6001, "pi", "/usr/bin/node"));

    assert_eq!(host.scan().holds(&debug_log()), [Hold::InUse]);
}

/// Catches a Pi under another name, which only its environment ties to the store.
#[test]
fn a_process_with_the_store_in_its_environment_holds() {
    let mut host = Host::new(managed());
    let mut process = running(6002, "node", "/usr/bin/node");
    process.env_paths = Ok(vec![EnvPath {
        var: "PI_CODING_AGENT_DIR".into(),
        path: host.path(AGENT),
    }]);
    host.processes.running.push(process);

    assert_eq!(host.scan().holds(&debug_log()), [Hold::InUse]);
}

/// Catches an unreadable environment being read as an empty one: that's the only place such
/// a process could show the store.
#[test]
fn unreadable_environment_blocks() {
    let mut host = Host::new(managed());
    let mut process = running(6004, "node", "/usr/bin/node");
    process.env_paths = Err(Unread::Failed(std::sync::Arc::new(
        std::io::ErrorKind::PermissionDenied.into(),
    )));
    host.processes.running.push(process);

    assert_eq!(host.scan().holds(&debug_log()), [Hold::UnknownActivity]);
}

/// Catches `~/.pi/agent` being read once `PI_CODING_AGENT_DIR` moved the store: logs resolve
/// at the new place, and the old one is left alone and closed.
#[test]
fn moved_agent_dir_is_the_only_store_read() {
    let moved = "home/elsewhere";
    let mut tree = managed();
    fixture::store(&mut tree, moved);
    let mut host = Host::new(tree);
    host.root(SymbolicRoot::PiAgent, &[moved]);

    let scan = host.scan();

    let log = format!("{moved}/pi-debug.log");
    assert_eq!(scan.fact(&log, "layout"), SUPPORTED);
    assert_eq!(scan.protection(&log, PI), None);
    assert!(
        !scan.read().iter().any(|path| path.starts_with("home/.pi/")),
        "expected nothing of ~/.pi read, got {:?}",
        scan.read()
    );
    assert_eq!(
        scan.protection(&debug_log(), PI),
        Some(Protection::Subresource)
    );
}

/// Catches sessions moved by `--session-dir` losing their closure: nothing opens that root.
#[test]
fn moved_session_dir_stays_closed() {
    let moved = "home/pi-sessions";
    let mut tree = managed();
    tree.file(&format!("{moved}/--home-x--/2026_a.jsonl"), b"{}\n");
    let mut host = Host::new(tree);
    host.root(SymbolicRoot::PiSessionDir, &[moved]);

    let scan = host.scan();

    let session = format!("{moved}/--home-x--/2026_a.jsonl");
    assert_eq!(scan.protection(&session, PI), Some(Protection::Subresource));
    assert!(
        !scan
            .all(PI)
            .iter()
            .any(|found| found.contains("pi-sessions")),
        "expected no finding there, got {:?}",
        scan.all(PI)
    );
}

/// Catches a second reading of `pi.agent`, not a store, closing the first or being read as
/// one: the check is per path.
#[test]
fn a_second_agent_path_is_judged_alone() {
    let other = "home/.config/other";
    let mut tree = managed();
    tree.file(&format!("{other}/pi-debug.log"), b"someone else's\n");
    let mut host = Host::new(tree);
    host.root(SymbolicRoot::PiAgent, &[AGENT, other]);

    let scan = host.scan();

    assert_eq!(scan.fact(&debug_log(), "layout"), SUPPORTED);
    assert_eq!(scan.protection(&debug_log(), PI), None);
    assert_eq!(scan.fact(other, "layout"), UNKNOWN_LAYOUT);
    assert_eq!(scan.found(LOGS).len(), 4, "expected only the store's logs");
    assert_eq!(scan.finding(other).rule.0, STORE);
}

/// Catches a path of the root that didn't resolve being dropped: the agent directory may be
/// where it points, so nothing is shown to be outside it.
#[test]
fn an_unresolved_agent_path_holds_every_log() {
    let mut host = Host::new(managed());
    host.unresolved(SymbolicRoot::PiAgent);

    let scan = host.scan();

    assert_eq!(
        scan.protection(&debug_log(), PI),
        Some(Protection::Subresource)
    );
}

/// Catches a person's sessions, credentials or settings reaching a finding or a removal.
#[test]
fn what_a_person_keeps_is_never_a_candidate() {
    let scan = scan(managed());

    for name in [
        "auth.json",
        "mcp-auth.json",
        "settings.json",
        "trust.json",
        "models-store.json",
        "AGENTS.md",
        "sessions/--home-u-code--/2026-11-02T10-00-00_a.jsonl",
        "extensions/mine/index.ts",
        "git/.gitignore",
        "npm/package.json",
        "bin/pi",
    ] {
        let closed = at(name);

        assert_eq!(
            scan.protection(&closed, PI),
            Some(Protection::Subresource),
            "{name}"
        );
        assert!(
            !scan.all(PI).iter().any(|found| found.ends_with(&closed)),
            "expected no finding at {name}"
        );
    }
}

/// Catches a read of anything but the two install files: `auth.json` and the sessions hold
/// what must never reach a report.
#[test]
fn only_the_install_files_are_read() {
    let scan = scan(managed());

    let mut read = scan.read();
    read.sort();
    read.dedup();
    let install = ["current-version", "managed-install.json"];
    assert_eq!(read, install.map(|file| at(&format!("install/{file}"))));
    assert!(
        !format!("{:?}", scan.report).contains("credential-value"),
        "expected no credential in the report"
    );
}

/// Catches the removal reaching an OS it wasn't verified on: how macOS and Windows show
/// `process.title` is unverified, so their logs are only reported (pinned 35, 39).
#[test]
fn logs_are_only_reported_off_linux() {
    for platform in [Platform::Macos, Platform::Windows] {
        let mut host = Host::new(managed());
        host.platform = platform;

        let scan = host.scan();

        assert_eq!(scan.found(LOGS), [""; 0]);
        assert_eq!(scan.proposal(&debug_log()), Proposal::Report);
        assert_eq!(scan.finding(&debug_log()).rule.0, STORE);
    }
}

/// Catches the inventory losing what the spec keeps in view.
#[test]
fn the_inventory_reports_the_store() {
    let scan = scan(managed());

    let found = scan.found(STORE);
    for name in [
        "mcp.log",
        "crashes.json",
        "bin/fd",
        "bin/rg",
        "install/releases/1.4.0",
        "install/staging/update-a1B2c3",
        "tmp/extensions",
    ] {
        assert!(found.contains(&at(name)), "expected {name} in {found:?}");
    }
}

/// Catches a damaged marker that still passes as this layout's: `layout` is supported only
/// when all three keys say so.
#[test]
fn generated_markers_verify_only_the_marker() {
    for seed in 0..48 {
        let bytes = Generated::seeded(seed).mutated(MARKER.as_bytes());
        let scan = scan_with("managed-install.json", &bytes);

        let parsed: Option<serde_json::Value> = serde_json::from_slice(&bytes).ok();
        let key = |name: &str| parsed.as_ref().and_then(|value| value.get(name)).cloned();
        let valid = key("kind") == Some("pi-managed-install".into())
            && key("schemaVersion") == Some(1.into())
            && key("layout") == Some("releases-v1".into());
        assert_eq!(
            scan.owner(PI) == OwnerStatus::Supported,
            valid,
            "seed {seed}"
        );
        assert_eq!(scan.omissions(PI).len(), 0, "seed {seed} cost an omission");
    }
}

/// Catches a damaged `current-version` passing as the verified one, or reaching a path: it
/// only ever equals a listed version.
#[test]
fn generated_versions_verify_only_the_version() {
    for seed in 0..48 {
        let bytes = Generated::seeded(seed).mutated(b"1.5.0\n");
        let scan = scan_with("current-version", &bytes);

        let valid = [&b"1.5.0"[..], b"1.5.0\n", b"1.5.0\r\n"].contains(&bytes.as_slice());
        assert_eq!(
            scan.owner(PI) == OwnerStatus::Supported,
            valid,
            "seed {seed}"
        );
        assert_eq!(scan.omissions(PI).len(), 0, "seed {seed} cost an omission");
    }
}
