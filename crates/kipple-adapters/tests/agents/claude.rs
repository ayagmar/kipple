//! Claude Code's debug logs, versions and store: S5 Claude §7 and pinned 38.

use kipple_core::{
    Activity, Caution, FactValue, Hold, IntegrationId, Inventory, Level, OwnerStatus, Platform,
    ProcessOwner, Proposal, Protection, Seen, SymbolicRoot, Verdict,
};

use super::claude_v2_1_318::{self as fixture, CLOSED_SESSION, OPEN_SESSION, VERSION, marker};
use super::host::{DAY, Generated, Host, Names, Scan, running, sealed};
use super::tree::{FixtureTree, Holds};

const LOGS: &str = "builtin.claude-code.closed-debug-logs";
const VERSIONS: &str = "builtin.claude-code.versions";
const STORE: &str = "builtin.claude-code.store-inventory";

const CLAUDE: IntegrationId = IntegrationId("claude-code");
const CONFIG: &str = "home/.claude";
const VERSIONS_DIR: &str = "home/.local/share/claude/versions";

/// A session ID in the wrong case, with no lowercase twin in the fixture: macOS and Windows
/// fold case, so a twin would be the same file.
const UPPERCASE_SESSION: &str = "ABCDEF01-2345-4678-9ABC-DEF012345678";

const SUPPORTED: Option<FactValue> = Some(FactValue::Enum("supported"));
const UNKNOWN_LAYOUT: Option<FactValue> = Some(FactValue::Enum("unknown"));
const UNKNOWN: Option<FactValue> = Some(FactValue::Unknown);
const CLOSED: Option<FactValue> = Some(FactValue::Bool(true));
const OPEN: Option<FactValue> = Some(FactValue::Bool(false));

fn log(session: &str) -> String {
    format!("{CONFIG}/debug/{session}.txt")
}

fn closed_log() -> String {
    log(CLOSED_SESSION)
}

/// A verified native install with its configuration directory.
fn installed() -> FixtureTree {
    let mut tree = FixtureTree::new();
    fixture::config(&mut tree, CONFIG);
    fixture::install(&mut tree, "home", VERSION);
    tree
}

/// A verified native install whose configuration directory holds its settings and one log,
/// and no `sessions/` yet.
fn no_sessions() -> FixtureTree {
    let mut tree = FixtureTree::new();
    tree.file(&format!("{CONFIG}/settings.json"), b"{}\n")
        .file(&closed_log(), b"[DEBUG] closed\n");
    fixture::install(&mut tree, "home", VERSION);
    tree
}

fn scan(tree: FixtureTree) -> Scan {
    Host::new(tree).scan()
}

/// `tree` with `path` below the configuration directory written, scanned.
fn scan_with(mut tree: FixtureTree, path: &str, contents: &[u8]) -> Scan {
    tree.file(&format!("{CONFIG}/{path}"), contents);
    scan(tree)
}

fn proposes_removal(scan: &Scan, relative: &str) -> bool {
    matches!(
        scan.proposal(relative),
        Proposal::Remove(_) | Proposal::RemoveRelease(_) | Proposal::Native(_)
    )
}

/// Catches a log being offered while its session runs, or a closed one never reaching the
/// removal: the rule's whole purpose (§7.1). Going through the policy and the host's
/// protection both, it's what lets a person remove the log and nothing else. The removal
/// itself is the executor's: `kipple`'s `tui.rs` runs one end to end.
#[test]
fn closed_debug_log_removed_open_one_kept() {
    let scan = scan(installed());
    let (closed, open) = (closed_log(), log(OPEN_SESSION));

    assert_eq!(scan.fact(&closed, "session_closed"), CLOSED);
    assert_eq!(scan.fact(&closed, "layout"), SUPPORTED);
    assert_eq!(scan.fact(&closed, "claude.version_supported"), SUPPORTED);
    assert!(
        matches!(scan.verdict(&closed), Verdict::Review(_)),
        "expected the closed log in Review, got {:?}",
        scan.verdict(&closed)
    );
    assert_eq!(scan.protection(&closed, CLAUDE), None);

    assert_eq!(scan.fact(&open, "session_closed"), OPEN);
    assert_eq!(scan.holds(&open), [Hold::FactUnmet]);
}

/// Catches a session with no marker here being taken for closed: one started with
/// `CLAUDE_CONFIG_DIR` elsewhere writes its marker there, and a process is all that shows it.
#[test]
fn no_marker_with_a_process_on_the_path_holds() {
    let mut host = Host::new(installed());
    let held = closed_log();
    let mut reader = running(5001, "tail", "/usr/bin/tail");
    let path = host.path(&held);
    reader.argv = Ok(vec!["tail".into(), path.clone().into_os_string()]);
    host.processes.running.push(reader);

    let scan = host.scan();

    assert_eq!(scan.fact(&held, "session_closed"), CLOSED);
    assert_eq!(scan.holds(&held), [Hold::InUse]);
    assert!(
        matches!(
            scan.activity(&held),
            Activity::InUse { seen: Seen::Argument(ref seen), .. } if *seen == path
        ),
        "expected the argument to show it in use, got {:?}",
        scan.activity(&held)
    );
}

/// Catches a rule that can never fire on a real host: if a desktop session's sealed
/// processes left a log's activity unknown, nothing would ever be removed.
#[test]
fn a_desktop_session_leaves_a_closed_log_idle() {
    let scan = scan(installed());

    assert!(
        matches!(
            scan.activity(&closed_log()),
            Activity::IdleExceptSystem { .. }
        ),
        "expected idle but for root's processes, got {:?}",
        scan.activity(&closed_log())
    );
}

/// Catches a sealed process of the current user being excused when its command line, the
/// one thing Linux still shows of it, names the log.
#[test]
fn a_sealed_process_of_ours_naming_the_log_holds() {
    let mut host = Host::new(installed());
    let held = closed_log();
    let path = host.path(&held);
    let mut sealed = sealed(5002, ProcessOwner::CurrentUser, &["/usr/bin/less"]);
    sealed.argv = Ok(vec!["less".into(), path.into_os_string()]);
    host.processes.running.push(sealed);

    assert_eq!(host.scan().holds(&held), [Hold::InUse]);
}

/// Catches a bad marker being skipped: it might name any session, so every log waits (§7.5).
#[test]
fn malformed_marker_blocks_every_log() {
    let scan = scan_with(installed(), "sessions/7.json", b"{\"pid\":7,\"sessionId");

    for held in [closed_log(), log(OPEN_SESSION)] {
        assert_eq!(scan.fact(&held, "session_closed"), UNKNOWN);
        assert_eq!(scan.holds(&held), [Hold::FactUnknown]);
    }
}

/// Catches an absent `sessions/` being read as "no session is open": kipple can't tell a
/// directory upstream moved from one nobody made.
#[test]
fn missing_sessions_dir_blocks() {
    let scan = scan(no_sessions());

    assert_eq!(scan.fact(&closed_log(), "session_closed"), UNKNOWN);
    assert_eq!(scan.holds(&closed_log()), [Hold::FactUnknown]);
}

/// Catches an upstream rename of the key, which is observed and undocumented, opening every
/// log, or a value that isn't a session ID reading as one that names no log: such a marker
/// might belong to any session. A log's name is the ID as upstream writes it, in lowercase,
/// so another case is no ID either.
#[test]
fn marker_without_session_id_blocks() {
    let cut_short: String = OPEN_SESSION.chars().take(35).collect();
    for id in [
        format!("sess_{OPEN_SESSION}"),
        cut_short,
        format!(" {OPEN_SESSION}"),
        UPPERCASE_SESSION.to_owned(),
    ] {
        let contents = format!("{{\"pid\":7,\"sessionId\":\"{id}\"}}\n");
        let scan = scan_with(no_sessions(), "sessions/7.json", contents.as_bytes());

        assert_eq!(scan.fact(&closed_log(), "session_closed"), UNKNOWN, "{id}");
    }
    for contents in [
        &b"{\"pid\":7,\"session\":\"a\"}\n"[..],
        b"{\"pid\":7,\"sessionId\":\"\"}\n",
        b"{\"pid\":7,\"sessionId\":7}\n",
        b"[]\n",
    ] {
        let scan = scan_with(installed(), "sessions/7.json", contents);

        assert_eq!(scan.fact(&closed_log(), "session_closed"), UNKNOWN);
    }
}

/// Catches a marker going unread because of its extension's case: what a folding disk keeps
/// is whatever case wrote it first, and an unread marker reads as a closed session.
#[test]
fn a_marker_in_another_case_still_counts() {
    let mut tree = no_sessions();
    tree.file(
        &format!("{CONFIG}/sessions/7.JSON"),
        marker(CLOSED_SESSION).as_bytes(),
    );

    assert_eq!(scan(tree).fact(&closed_log(), "session_closed"), OPEN);
}

/// Catches an empty `sessions/` blocking forever: with nothing running that is every
/// machine's state, and its logs are closed.
#[test]
fn empty_sessions_dir_closes_every_log() {
    let mut tree = no_sessions();
    tree.dir(&format!("{CONFIG}/sessions"));

    assert_eq!(scan(tree).fact(&closed_log(), "session_closed"), CLOSED);
}

/// Catches a marker that can't be opened counting as no marker: a directory of that name, or
/// a `sessions` that is a link, whose files are somewhere kipple doesn't follow.
#[test]
fn unreadable_markers_block() {
    let mut directory = no_sessions();
    directory.dir(&format!("{CONFIG}/sessions/7.json"));
    let mut linked = no_sessions();
    linked.file("home/elsewhere/4242.json", marker(OPEN_SESSION).as_bytes());
    linked.link(
        &format!("{CONFIG}/sessions"),
        "home/elsewhere",
        Holds::AbsoluteInTree,
    );

    for tree in [directory, linked] {
        assert_eq!(scan(tree).fact(&closed_log(), "session_closed"), UNKNOWN);
    }
}

/// Catches a session that just ended being offered at a level with a short recent window:
/// the rule's own 14 days apply at every level.
#[test]
fn recent_log_is_held() {
    let mut host = Host::new(installed());
    let recent = closed_log();
    host.tree.modified(&recent, host.now - 13 * DAY);

    let scan = host.scan();

    assert_eq!(scan.holds(&recent), [Hold::FactUnmet]);
    assert_eq!(
        scan.verdict_at(&recent, Level::Thorough),
        Verdict::Protected(vec![Hold::FactUnmet])
    );
}

/// Catches a diagnostic log turning Safe at a level: it comes back with no session, so a
/// person always reviews it.
#[test]
fn debug_log_is_never_safe() {
    let scan = scan(installed());

    for level in Level::ALL {
        assert!(
            matches!(scan.verdict_at(&closed_log(), *level), Verdict::Review(ref why) if !why.contains(&Caution::Recent)),
            "expected Review at {level}, got {:?}",
            scan.verdict_at(&closed_log(), *level)
        );
    }
}

/// Catches a name kipple doesn't know reaching the removing rule because it looks like a
/// log (§7.2), or a newer layout's subdirectory going missing from the report (§7.11).
#[test]
fn unknown_debug_name_is_kept() {
    let mut tree = installed();
    let names = [
        format!("{CONFIG}/debug/notes.txt"),
        format!("{CONFIG}/debug/{CLOSED_SESSION}.txt.bak"),
        format!("{CONFIG}/debug/{UPPERCASE_SESSION}.txt"),
        format!("{CONFIG}/debug/subdirectory"),
    ];
    for name in &names[..3] {
        tree.file(name, b"\n");
    }
    tree.file(&format!("{}/inner.txt", names[3]), b"\n");

    let scan = scan(tree);

    for name in &names {
        assert_eq!(scan.finding(name).rule.0, STORE);
        assert_eq!(scan.holds(name), [Hold::Advice]);
    }
}

/// Catches a directory the rule doesn't name reaching it on a disk that folds case: the probe
/// of `debug` lands on `DEBUG`, and its logs come out under the lowercase path the host's
/// area opens.
#[test]
fn a_debug_directory_in_another_case_is_not_read() {
    let mut tree = FixtureTree::new();
    tree.file(&format!("{CONFIG}/settings.json"), b"{}\n")
        .dir(&format!("{CONFIG}/sessions"))
        .file(
            &format!("{CONFIG}/DEBUG/{CLOSED_SESSION}.txt"),
            b"not claude's\n",
        );
    fixture::install(&mut tree, "home", VERSION);
    let mut host = Host::new(tree);
    host.names = Names::Folded;

    assert_eq!(host.scan().found(LOGS), [""; 0]);
}

/// Catches a link named like a log being taken for one: removing it says nothing about where
/// it points, and what it points at isn't Claude Code's (§7.3).
#[test]
fn linked_log_is_no_candidate() {
    let mut tree = installed();
    let linked = log("00000000-0000-4000-8000-000000000000");
    tree.file("home/mine.txt", b"mine\n")
        .link(&linked, "home/mine.txt", Holds::AbsoluteInTree);

    let scan = scan(tree);

    assert_eq!(scan.finding(&linked).rule.0, STORE);
    assert!(
        !proposes_removal(&scan, &linked),
        "expected a report, got {:?}",
        scan.proposal(&linked)
    );
}

/// Catches `debug/latest` being offered, or blocking its siblings, whether it points at a
/// log or at nothing (§7.4).
#[test]
fn latest_link_blocks_nothing() {
    let mut dangling = no_sessions();
    dangling
        .dir(&format!("{CONFIG}/sessions"))
        .bad_link(&format!("{CONFIG}/debug/latest"));

    for tree in [installed(), dangling] {
        let scan = scan(tree);
        let latest = format!("{CONFIG}/debug/latest");

        assert_eq!(scan.fact(&closed_log(), "session_closed"), CLOSED);
        assert!(
            !scan
                .all(CLAUDE)
                .iter()
                .any(|found| found.ends_with("/latest")),
            "expected no finding at latest, got {:?}",
            scan.all(CLAUDE)
        );
        assert_eq!(
            scan.protection(&latest, CLAUDE),
            Some(Protection::Subresource)
        );
    }
}

/// Catches a name alone hiding an entry: only upstream's link is left out, so a later layout's
/// `debug/latest/`, or a file of that name, stays in the report (§7.11).
#[test]
fn a_latest_that_is_no_link_is_reported() {
    let latest = format!("{CONFIG}/debug/latest");
    let mut file = no_sessions();
    file.file(&latest, b"\n");
    let mut directory = no_sessions();
    directory.file(&format!("{latest}/inner.txt"), b"\n");

    for tree in [file, directory] {
        let scan = scan(tree);

        assert_eq!(scan.finding(&latest).rule.0, STORE);
        assert_eq!(scan.proposal(&latest), Proposal::Advice);
    }
}

/// Catches a cut-short process listing passing for an idle host (§7.6).
#[test]
fn incomplete_inventory_blocks_logs() {
    let mut host = Host::new(installed());
    host.processes.inventory = Inventory::Partial {
        reason: "listing stopped".to_owned(),
    };

    assert_eq!(host.scan().holds(&closed_log()), [Hold::UnknownActivity]);
}

/// Catches any other version being trusted with this layout: the rule closes, and through
/// the owner status so does the root.
#[test]
fn unverified_claude_version_holds_logs() {
    let mut tree = FixtureTree::new();
    fixture::config(&mut tree, CONFIG);
    fixture::install(&mut tree, "home", "2.1.319");

    let scan = scan(tree);

    let held = closed_log();
    assert_eq!(
        scan.fact(&held, "claude.version_supported"),
        Some(FactValue::Enum("unsupported"))
    );
    assert_eq!(scan.fact(&held, "layout"), UNKNOWN_LAYOUT);
    assert_eq!(
        scan.holds(&held),
        [
            Hold::UnknownLayout,
            Hold::UnknownConsequence,
            Hold::FactUnmet
        ]
    );
    assert_eq!(scan.owner(CLAUDE), OwnerStatus::Unsupported);
    assert_eq!(
        scan.protection(&held, CLAUDE),
        Some(Protection::Subresource)
    );
}

/// Catches a version nobody can name being taken for a verified one: a launcher that is a
/// script, an npm install or none at all shows no version.
#[test]
fn non_native_install_holds_logs() {
    let mut script = FixtureTree::new();
    fixture::config(&mut script, CONFIG);
    script.file("home/.local/bin/claude", b"#!/bin/sh\n");
    let mut none = FixtureTree::new();
    fixture::config(&mut none, CONFIG);

    for tree in [script, none] {
        let scan = scan(tree);

        let held = closed_log();
        assert_eq!(scan.fact(&held, "claude.version_supported"), UNKNOWN);
        assert_eq!(scan.fact(&held, "layout"), UNKNOWN);
        assert_eq!(scan.owner(CLAUDE), OwnerStatus::Unsupported);
        assert_eq!(
            scan.protection(&held, CLAUDE),
            Some(Protection::Subresource)
        );
    }
}

/// Catches `~/.claude` being read as the store once `CLAUDE_CONFIG_DIR` moved it: its files
/// aren't this session's to judge, and upstream still writes there (§7.7).
#[test]
fn override_scans_only_the_resolved_root() {
    let moved = "home/elsewhere";
    let mut tree = installed();
    fixture::config(&mut tree, moved);
    let mut host = Host::new(tree);
    host.root(SymbolicRoot::ClaudeConfig, &[moved]);

    let scan = host.scan();

    let found = format!("{moved}/debug/{CLOSED_SESSION}.txt");
    assert_eq!(scan.fact(&found, "session_closed"), CLOSED);
    assert_eq!(scan.protection(&found, CLAUDE), None);
    assert!(
        !scan
            .all(CLAUDE)
            .iter()
            .any(|entry| entry.contains("/.claude/")),
        "expected nothing of ~/.claude, got {:?}",
        scan.all(CLAUDE)
    );
    assert_eq!(
        scan.protection(&closed_log(), CLAUDE),
        Some(Protection::Subresource)
    );
}

/// Catches a move kipple's resolver can't see going unnoticed: `env.CLAUDE_CONFIG_DIR` in
/// `settings.json` means this may not be the store at all (§7.8). Its value stays unread.
#[test]
fn settings_env_override_makes_the_root_unknown() {
    let settings = b"{\"env\":{\"CLAUDE_CONFIG_DIR\":\"/secret-value\"}}";
    let scan = scan_with(installed(), "settings.json", settings);

    let held = closed_log();
    assert_eq!(scan.fact(&held, "layout"), UNKNOWN_LAYOUT);
    assert_eq!(
        scan.holds(&held),
        [
            Hold::UnknownLayout,
            Hold::UnknownConsequence,
            Hold::FactUnmet
        ]
    );
    assert_eq!(scan.owner(CLAUDE), OwnerStatus::Unsupported);
    assert!(
        !format!("{:?}", scan.report).contains("secret-value"),
        "expected the override's value to stay out of the report"
    );
}

/// Catches `settings.json` being read from the first path of a root that has two: what any
/// reading says about where the root is holds for every path.
#[test]
fn a_second_config_path_moving_the_root_holds_the_first() {
    let second = "home/.config/claude";
    let mut tree = installed();
    tree.file(
        &format!("{second}/settings.json"),
        b"{\"env\":{\"CLAUDE_CONFIG_DIR\":\"/x\"}}",
    );
    let mut host = Host::new(tree);
    host.root(SymbolicRoot::ClaudeConfig, &[CONFIG, second]);

    let scan = host.scan();

    assert_eq!(scan.fact(&closed_log(), "layout"), UNKNOWN_LAYOUT);
    assert_eq!(scan.owner(CLAUDE), OwnerStatus::Unsupported);
}

/// Catches a path of the root that didn't resolve being dropped: a store of credentials may
/// be where it points, so nothing is shown to be outside it, whatever the other path says.
#[test]
fn an_unresolved_config_path_holds_every_log() {
    let mut host = Host::new(installed());
    host.unresolved(SymbolicRoot::ClaudeConfig);

    let scan = host.scan();

    assert_eq!(scan.fact(&closed_log(), "layout"), SUPPORTED);
    assert_eq!(
        scan.protection(&closed_log(), CLAUDE),
        Some(Protection::Subresource)
    );
}

/// Catches settings that can't be read being taken as settings that move nothing.
#[test]
fn unusable_settings_leave_the_layout_unknown() {
    for settings in [&b"{\"env\":"[..], b"[]", b"{\"env\":\"x\"}"] {
        let scan = scan_with(installed(), "settings.json", settings);

        assert_eq!(scan.fact(&closed_log(), "layout"), UNKNOWN);
        assert_eq!(
            scan.holds(&closed_log()),
            [
                Hold::UnknownLayout,
                Hold::UnknownConsequence,
                Hold::FactUnknown
            ]
        );
    }
}

/// Catches a machine without debug logging showing an error or an empty finding (§7.9).
#[test]
fn no_debug_dir_is_no_finding() {
    let mut tree = FixtureTree::new();
    tree.file(&format!("{CONFIG}/settings.json"), b"{}\n")
        .dir(&format!("{CONFIG}/sessions"));
    fixture::install(&mut tree, "home", VERSION);

    let scan = scan(tree);

    assert_eq!(scan.found(LOGS), [""; 0]);
    assert_eq!(scan.omissions(CLAUDE).len(), 0, "expected no omission");
    assert_eq!(scan.owner(CLAUDE), OwnerStatus::Supported);
}

/// Catches a version, or anything beside one, ever being proposed: the installer prunes
/// them, and which one runs can't be proved (§7.10).
#[test]
fn versions_never_proposed() {
    let scan = scan(installed());

    let active = format!("{VERSIONS_DIR}/{VERSION}");
    let rollback = format!("{VERSIONS_DIR}/2.1.316");
    assert_eq!(scan.found(VERSIONS), [rollback.clone(), active.clone()]);
    assert_eq!(
        scan.fact(&active, "launcher_target"),
        Some(FactValue::Bool(true))
    );
    assert_eq!(
        scan.fact(&active, "version_running"),
        Some(FactValue::Bool(true))
    );
    assert_eq!(
        scan.fact(&rollback, "launcher_target"),
        Some(FactValue::Bool(false))
    );
    assert_eq!(scan.fact(&rollback, "version_running"), UNKNOWN);
    for version in [active, rollback] {
        assert_eq!(scan.proposal(&version), Proposal::Report);
    }
}

/// Catches a launcher that isn't a native install's reaching the versions: a regular file, a
/// dangling link, or a link to a name outside the grammar (§7.10).
#[test]
fn a_launcher_that_is_no_native_install_reports_no_versions() {
    let bin = "home/.local/bin/claude";
    let mut plain = FixtureTree::new();
    plain.file(bin, b"#!/bin/sh\n");
    let mut dangling = FixtureTree::new();
    dangling.bad_link(bin);
    let mut odd = FixtureTree::new();
    odd.file(&format!("{VERSIONS_DIR}/latest"), b"\x7fELF")
        .link(
            bin,
            &format!("{VERSIONS_DIR}/latest"),
            Holds::AbsoluteInTree,
        );

    for mut tree in [plain, dangling, odd] {
        fixture::config(&mut tree, CONFIG);
        tree.file(&format!("{VERSIONS_DIR}/{VERSION}"), b"\x7fELF");

        assert_eq!(scan(tree).found(VERSIONS), [""; 0]);
    }
}

/// Catches what sits beside the versions being listed as one: a directory of a version's
/// name, or a partial download (§7.10).
#[test]
fn only_a_regular_file_of_the_grammar_is_a_version() {
    let mut tree = installed();
    tree.file(&format!("{VERSIONS_DIR}/2.1.300/inner"), b"\n")
        .file(&format!("{VERSIONS_DIR}/2.1.301.partial"), b"\x7f");

    let found = scan(tree).found(VERSIONS);

    assert_eq!(
        found.len(),
        2,
        "expected the two versions alone, got {found:?}"
    );
}

/// Catches anything of a person's own reaching a finding or a removal: everything outside
/// `debug/` is closed to Claude Code's own rules as well.
#[test]
fn sessions_memory_and_settings_never_candidates() {
    let scan = scan(installed());

    for name in [
        "settings.json",
        "settings.local.json",
        ".credentials.json",
        "CLAUDE.md",
        "history.jsonl",
        "sessions/4242.json",
        "daemon/control.key",
        "projects/-home-u-code/transcript.jsonl",
        "projects/-home-u-code/memory/MEMORY.md",
        "file-history/session/a.txt",
        "backups/.claude.json.backup.1793000000000",
        "plugins/installed_plugins.json",
        "skills/mine/SKILL.md",
        "telemetry/1p_failed_events.json",
    ] {
        let closed = format!("{CONFIG}/{name}");

        assert_eq!(
            scan.protection(&closed, CLAUDE),
            Some(Protection::Subresource),
            "{name}"
        );
        assert!(
            !scan
                .all(CLAUDE)
                .iter()
                .any(|found| found.ends_with(&closed)),
            "expected no finding at {name}"
        );
    }
}

/// Catches a read of anything but what the two facts need, and any value of a file reaching
/// the report: credentials, keys and history aren't opened, and a marker gives up only its
/// session.
#[test]
fn key_files_are_never_read() {
    let scan = scan(installed());

    let mut read = scan.read();
    read.sort();
    read.dedup();
    assert_eq!(
        read,
        [
            format!("{CONFIG}/sessions/4242.json"),
            format!("{CONFIG}/settings.json")
        ]
    );
    let report = format!("{:?}", scan.report);
    for secret in [
        "credential-value",
        "key-value",
        "history-value",
        "procStart",
        "812734",
    ] {
        assert!(
            !report.contains(secret),
            "expected {secret} to stay out of the report"
        );
    }
}

/// Catches advice that sells the setting as doing more than it does. Each line was checked
/// against upstream's docs, and each exception decides what a person loses.
#[test]
fn advice_states_the_checked_exceptions() {
    let scan = scan(installed());

    let advice = scan.advice(CLAUDE);
    assert_eq!(advice.len(), 1, "expected one advice per directory");
    for exception in [
        "The default is 30 and the minimum 1; 0 fails validation",
        "Managed settings",
        "never deletes auto memory files",
        "v2.1.228",
        "leaves history.jsonl, except under a HIPAA setting",
        "desktopSessionCleanupPeriodDays",
        "claude -p --bare doesn't sweep",
        "pauses the sweep",
    ] {
        assert!(
            advice[0].text.contains(exception),
            "expected {exception} in the advice"
        );
    }
}

/// Catches a session started between the scan and the removal slipping through: apply's
/// reading of the markers has to find it.
#[test]
fn a_session_opened_after_the_scan_holds_at_apply() {
    let scan = scan(installed());
    let held = closed_log();
    assert_eq!(scan.now(&held).map(|now| now.unmet.len()).ok(), Some(0));

    let marker = scan.path(&format!("{CONFIG}/sessions/5151.json"));
    std::fs::write(marker, super::claude_v2_1_318::marker(CLOSED_SESSION)).unwrap();

    let now = scan.now(&held).ok().unwrap();
    assert_eq!(now.supplied.values.get("session_closed").copied(), OPEN);
    assert_eq!(now.unmet.len(), 1, "expected session_closed unmet");
}

/// Catches apply trusting the scan's answer once a marker can no longer be read.
#[test]
fn a_marker_gone_bad_after_the_scan_holds_at_apply() {
    let scan = scan(installed());

    std::fs::write(scan.path(&format!("{CONFIG}/sessions/4242.json")), b"{").unwrap();

    let now = scan.now(&closed_log()).ok().unwrap();
    assert_eq!(now.supplied.values.get("session_closed").copied(), UNKNOWN);
    assert_eq!(now.unmet.len(), 1, "expected session_closed unknown");
}

/// Catches a process that started after the scan not counting.
#[test]
fn a_new_process_on_the_log_holds_at_apply() {
    let scan = scan(installed());
    let held = closed_log();
    let path = scan.path(&held);

    scan.show(|processes| {
        let mut reader = running(5003, "tail", "/usr/bin/tail");
        reader.cwd = Ok(path.parent().unwrap().to_owned());
        reader.argv = Ok(vec!["tail".into(), path.clone().into_os_string()]);
        processes.running.push(reader);
    });

    let now = scan.now(&held).ok().unwrap();
    assert!(
        matches!(now.activity, Activity::InUse { pid: 5003, .. }),
        "expected the new process to show it in use, got {:?}",
        now.activity
    );
}

/// Catches the removal reaching an OS it wasn't verified on: macOS and Windows have the
/// logs only as a report (pinned 39), with the same advice.
#[test]
fn logs_are_only_reported_off_linux() {
    for platform in [Platform::Macos, Platform::Windows] {
        let mut host = Host::new(installed());
        host.platform = platform;

        let scan = host.scan();

        let held = closed_log();
        assert_eq!(scan.found(LOGS), [""; 0]);
        assert_eq!(scan.finding(&held).rule.0, STORE);
        assert_eq!(scan.proposal(&held), Proposal::Advice);
    }
}

/// Catches the inventory reporting what the retention sweep doesn't cover as if it did, or
/// losing the entries the spec keeps in view.
#[test]
fn the_inventory_reports_with_advice_only_what_the_sweep_covers() {
    let mut tree = installed();
    tree.file(&format!("{CONFIG}/.claude.json.tmp.1793"), b"{}")
        .file("home/.claude.json.tmp.1794", b"{}")
        .file("home/.cache/claude-cli-nodejs/-home-u/mcp.jsonl", b"\n");

    let scan = scan(tree);

    for swept in ["shell-snapshots", "session-env", "plans"] {
        assert_eq!(
            scan.proposal(&format!("{CONFIG}/{swept}")),
            Proposal::Advice,
            "{swept}"
        );
    }
    for unswept in [
        "home/.claude/cache/changelog.md",
        "home/.claude/daemon.log",
        "home/.claude/.claude.json.tmp.1793",
        "home/.claude.json.tmp.1794",
        "home/.cache/claude-cli-nodejs",
    ] {
        assert_eq!(scan.proposal(unswept), Proposal::Report, "{unswept}");
    }
}

/// Catches a report-only entry stopping the integration, which took the removing rule and
/// the rest of the inventory with it: a file where a directory was, or a link kipple doesn't
/// follow, has nothing to report.
#[test]
fn an_inventory_path_that_cant_be_there_stops_nothing() {
    let mut file = no_sessions();
    file.dir(&format!("{CONFIG}/sessions"))
        .file(&format!("{CONFIG}/cache"), b"a file, not a directory\n");
    let mut linked = no_sessions();
    linked
        .dir(&format!("{CONFIG}/sessions"))
        .dir("home/elsewhere")
        .link(
            "home/.cache/claude",
            "home/elsewhere",
            Holds::AbsoluteInTree,
        );

    for tree in [file, linked] {
        let scan = scan(tree);

        assert_eq!(scan.omissions(CLAUDE).len(), 0, "expected no omission");
        assert_eq!(scan.owner(CLAUDE), OwnerStatus::Supported);
        assert_eq!(scan.fact(&closed_log(), "session_closed"), CLOSED);
    }
}

/// Catches a report-only entry the OS refuses stopping the integration, or vanishing from the
/// report: it is listed, with what kipple knows of it, which is nothing.
#[test]
fn a_refused_inventory_entry_is_listed_and_stops_nothing() {
    let refused = format!("{CONFIG}/plans");
    let mut host = Host::new(installed());
    host.denied.push(refused.clone());

    let scan = host.scan();

    assert_eq!(scan.omissions(CLAUDE).len(), 0, "expected no omission");
    assert_eq!(scan.owner(CLAUDE), OwnerStatus::Supported);
    assert_eq!(scan.finding(&refused).rule.0, STORE);
    assert_eq!(scan.proposal(&refused), Proposal::Report);
    assert_eq!(scan.fact(&closed_log(), "session_closed"), CLOSED);
}

/// Writes `valid`, mutated by each seed, at `at` below the configuration directory, and hands
/// `check` what was written and what the scan made of it.
fn each_mutation(valid: &[u8], at: &str, check: impl Fn(&[u8], &Scan)) {
    for seed in 0..48 {
        let bytes = Generated::seeded(seed).mutated(valid);
        let scan = scan_with(installed(), at, &bytes);
        assert_eq!(
            scan.omissions(CLAUDE).len(),
            0,
            "seed {seed} cost an omission"
        );
        check(&bytes, &scan);
    }
}

/// S5 §3's grammar, written here from the report and not from the code under test.
fn is_session_id(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(at, part)| {
            if [8, 13, 18, 23].contains(&at) {
                part == '-'
            } else {
                matches!(part, '0'..='9' | 'a'..='f')
            }
        })
}

/// `fact`, for a marker that reads as `bytes`, is what it must be: unknown unless the marker's
/// `sessionId` is a session ID.
fn assert_read(bytes: &[u8], fact: Option<FactValue>) {
    let parsed: Option<serde_json::Value> = serde_json::from_slice(bytes).ok();
    let named = parsed.as_ref().and_then(|value| value.get("sessionId"));
    let usable = named
        .and_then(serde_json::Value::as_str)
        .is_some_and(is_session_id);
    let shown = String::from_utf8_lossy(bytes);
    assert_eq!(fact == UNKNOWN, !usable, "marker {shown:?}");
}

/// Catches a damaged marker being read as no session: the log turns closed only if what's
/// left is a session ID, so the marker really names another session.
#[test]
fn generated_markers_never_close_a_session_they_might_name() {
    let marker = marker(OPEN_SESSION);
    each_mutation(marker.as_bytes(), "sessions/4242.json", |bytes, scan| {
        assert_read(bytes, scan.fact(&log(OPEN_SESSION), "session_closed"));
    });
}

/// The same with the damage in the value alone. Mutating a whole marker rarely leaves it
/// parsing with a bad ID, which is the case that would read as a closed session.
#[test]
fn generated_session_ids_never_close_a_session_they_might_name() {
    for seed in 0..48 {
        let id = Generated::seeded(seed).mutated(OPEN_SESSION.as_bytes());
        let id = String::from_utf8_lossy(&id);
        let marker = serde_json::json!({ "pid": 4242, "sessionId": id }).to_string();

        let scan = scan_with(no_sessions(), "sessions/4242.json", marker.as_bytes());

        assert_read(
            marker.as_bytes(),
            scan.fact(&closed_log(), "session_closed"),
        );
    }
}

/// Catches damaged settings being read as settings that move nothing: `layout` is supported
/// only for an object with no sign of the override.
#[test]
fn generated_settings_never_verify_a_layout_they_dont_show() {
    let settings = br#"{"env":{"CLAUDE_CONFIG_DIR":"/x","A":"b"},"cleanupPeriodDays":14}"#;
    each_mutation(settings, "settings.json", |bytes, scan| {
        let parsed: Option<serde_json::Value> = serde_json::from_slice(bytes).ok();
        let object = parsed.as_ref().is_some_and(serde_json::Value::is_object);
        let env = parsed.as_ref().and_then(|value| value.get("env"));
        let moved =
            env.is_some_and(|env| env.get("CLAUDE_CONFIG_DIR").is_some() || !env.is_object());
        if scan.fact(&closed_log(), "layout") == SUPPORTED {
            assert!(
                object && !moved,
                "settings {:?}",
                String::from_utf8_lossy(bytes)
            );
        }
    });
}
