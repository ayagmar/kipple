//! Claude Code (`docs/06-adapters-and-rules.md` §5 and §6): the debug logs of closed sessions,
//! an inventory of what else it keeps, and advice on its own retention setting,
//! `cleanupPeriodDays`. Verified against Claude Code 2.1.318 on Linux, and against its
//! settings, `.claude` directory and install documentation on 2026-11-06.
//!
//! Of its files kipple reads two kinds: `settings.json`, for that setting and whether its
//! `env` moves the directory, and each session marker, for its `sessionId`. Nothing read from
//! either is kept past that, and credentials and key files are never opened.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use kipple_core::{
    ActivityEvidence, Advice, AdviceId, Area, Claim, Consequence, Cost, DirEntryMeta, EntryKind,
    Evidence, FactKind, FactSpec, FactValue, Finding, FindingSink, Integration,
    IntegrationDescriptor, IntegrationError, IntegrationId, IntegrationSelector, Lens, Loss,
    OpenArea, OwnerStatus, Platform, ProbeError, ProtectedEntry, Recovery, Rule, ScanContext,
    Selector, StateDeclaration, Supplied, SymbolicRoot, Unresolved,
};

const ID: IntegrationId = IntegrationId("claude-code");

const DEBUG_LOGS: &str = "claude-debug-logs";
const VERSIONS: &str = "claude-versions";
const STORE_ENTRIES: &str = "claude-store-entries";

const SELECTORS: &[IntegrationSelector] = &[
    IntegrationSelector {
        name: DEBUG_LOGS,
        meaning: "one regular file per session in the debug directory",
    },
    IntegrationSelector {
        name: VERSIONS,
        meaning: "the versions a native install keeps",
    },
    IntegrationSelector {
        name: STORE_ENTRIES,
        meaning: "entries of the configuration directory that are reported and never removed",
    },
];

/// What each row was verified as, which is what a version must equal to be supported.
const VERIFIED: &[&str] = &["Claude Code 2.1.318"];
const VERIFIED_PREFIX: &str = "Claude Code ";

const LAYOUT: &str = "layout";
const VERSION_SUPPORTED: &str = "claude.version_supported";
const SESSION_CLOSED: &str = "session_closed";
const LAUNCHER_TARGET: &str = "launcher_target";
const VERSION_RUNNING: &str = "version_running";

const SUPPORTED: FactValue = FactValue::Enum("supported");
const UNSUPPORTED: FactValue = FactValue::Enum("unsupported");
const UNKNOWN_LAYOUT: FactValue = FactValue::Enum("unknown");

const FACTS: &[FactSpec] = &[
    FactSpec {
        name: LAYOUT,
        kind: FactKind::Enum(&["supported", "unknown"]),
        selector: None,
        meaning: "whether the configuration directory is the one Claude Code uses, on a verified \
                  version",
    },
    FactSpec {
        name: VERSION_SUPPORTED,
        kind: FactKind::Enum(&["supported", "unsupported"]),
        selector: None,
        meaning: "whether the launcher's version equals a verified one; a non-native install \
                  shows none, which is unknown",
    },
    FactSpec {
        name: SESSION_CLOSED,
        kind: FactKind::Bool,
        selector: Some(DEBUG_LOGS),
        meaning: "whether every session marker was read and none names this log's session",
    },
    FactSpec {
        name: LAUNCHER_TARGET,
        kind: FactKind::Bool,
        selector: Some(VERSIONS),
        meaning: "whether the launcher points at this version",
    },
    FactSpec {
        name: VERSION_RUNNING,
        kind: FactKind::Bool,
        selector: Some(VERSIONS),
        meaning: "whether a lock file names this version; no lock proves nothing, so that is \
                  unknown",
    },
];

/// A debug log comes back with no session and isn't regenerated, and 04 §2a counts
/// diagnostics as history, so it is Review at every level.
const CLOSED_LOG: Consequence = Consequence {
    recovery: Recovery::None,
    loss: &[Loss::History],
    cost: Cost::Low,
};

/// For display: no version is ever proposed, and the installer prunes them itself.
const INSTALLED_VERSION: Consequence = Consequence {
    recovery: Recovery::Redownload,
    loss: &[Loss::RollbackAbility],
    cost: Cost::Unknown,
};

const fn closed(root: SymbolicRoot, delegates: &'static [SymbolicRoot]) -> StateDeclaration {
    StateDeclaration {
        root,
        protected: &[],
        open: &[],
        delegates,
        unresolved: Unresolved::HoldOwn,
    }
}

/// What stays closed in the configuration directory. Only `debug/` opens, so this says what
/// `rules --json` shows and keeps `debug/latest` out of that area.
const PROTECTED: &[ProtectedEntry] = &[
    ProtectedEntry::Exact(".credentials.json"),
    ProtectedEntry::Exact(".claude.json"),
    ProtectedEntry::Exact("settings.json"),
    ProtectedEntry::Exact("settings.local.json"),
    ProtectedEntry::Exact("projects"),
    ProtectedEntry::Exact("history.jsonl"),
    ProtectedEntry::Exact("file-history"),
    ProtectedEntry::Exact("sessions"),
    ProtectedEntry::Exact("jobs"),
    ProtectedEntry::Exact("daemon"),
    ProtectedEntry::Exact("backups"),
    ProtectedEntry::Exact("plugins"),
    ProtectedEntry::Exact("skills"),
    ProtectedEntry::Exact("agents"),
    ProtectedEntry::Exact("agent-memory"),
    ProtectedEntry::Exact("hooks"),
    ProtectedEntry::Exact("rules"),
    ProtectedEntry::Exact("commands"),
    ProtectedEntry::Exact("workflows"),
    ProtectedEntry::Exact("tasks"),
    ProtectedEntry::Exact("CLAUDE.md"),
    ProtectedEntry::Exact("remote-settings.json"),
    ProtectedEntry::Exact("policy-limits.json"),
    ProtectedEntry::Exact("debug/latest"),
];

/// Only `debug/` opens, and only to Claude Code's own rule. A `$CLAUDE_CONFIG_DIR` that can't
/// be used may hide credentials anywhere, so that holds everything. `~/.claude` stays declared
/// when that variable moves the rest, because upstream still writes there.
const STATE: &[StateDeclaration] = &[
    StateDeclaration {
        root: SymbolicRoot::ClaudeConfig,
        protected: PROTECTED,
        open: &[OpenArea {
            at: Area::Inside("debug"),
            integrations: &[ID],
        }],
        delegates: &[],
        unresolved: Unresolved::HoldAll,
    },
    closed(SymbolicRoot::ClaudeHome, &[SymbolicRoot::ClaudeConfig]),
    closed(SymbolicRoot::ClaudeConfigFile, &[]),
    closed(SymbolicRoot::ClaudeVersions, &[]),
    closed(SymbolicRoot::ClaudeInstallState, &[]),
];

/// Reported entries of the configuration directory its retention sweep covers.
const SWEPT: &[&str] = &[
    "shell-snapshots",
    "session-env",
    "plans",
    "todos",
    "statsig",
    "logs",
    "image-cache",
];

/// Reported entries of the configuration directory the sweep leaves.
const UNSWEPT: &[&str] = &["cache/changelog.md", "daemon.log"];

/// The prefix of a full copy of `.claude.json` a killed process left. Upstream's issue #87677.
const CONFIG_COPY: &str = ".claude.json.tmp.";

/// Where Claude Code's MCP logs are, under each OS's cache directory.
const CACHE_ROOTS: &[SymbolicRoot] = &[
    SymbolicRoot::XdgCache,
    SymbolicRoot::MacosCaches,
    SymbolicRoot::LocalAppData,
];
const MCP_LOGS: &str = "claude-cli-nodejs";

/// Claude Code's default retention, in days.
const DEFAULT_DAYS: u64 = 30;

/// More than any settings file should hold.
const SETTINGS_LIMIT: u64 = 1024 * 1024;

/// More than any session marker holds. One cut short here can't be parsed, which is unknown.
const MARKER_LIMIT: u64 = 64 * 1024;

/// The Claude Code integration.
#[derive(Debug)]
pub(crate) struct ClaudeCode;

impl Integration for ClaudeCode {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: ID,
            lens: Lens::Agents,
            platforms: Platform::ALL,
            roots: &[
                SymbolicRoot::Home,
                SymbolicRoot::XdgCache,
                SymbolicRoot::LocalAppData,
                SymbolicRoot::MacosCaches,
                SymbolicRoot::ClaudeConfig,
                SymbolicRoot::ClaudeVersions,
                SymbolicRoot::ClaudeInstallState,
            ],
            selectors: SELECTORS,
            artifacts: &[],
            owner_files: &[],
            verified: VERIFIED,
            consequences: &[CLOSED_LOG, INSTALLED_VERSION],
            facts: FACTS,
            state: STATE,
            evidences: &[ActivityEvidence::ScopedPaths],
            revision: 2,
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let launcher = launcher(cx);
        let stores = stores(cx, launcher.as_ref());
        for store in &stores {
            let advice = sink.advise(advice(&store.retention));
            let at = At { cx, store, advice };
            add_debug_entries(at, sink)?;
            add_store_entries(at, sink)?;
        }
        add_config_copies(cx, sink, &stores)?;
        add_mcp_logs(cx, sink)?;
        add_versions(cx, sink, launcher.as_ref())?;
        if stores.iter().any(|store| store.layout == SUPPORTED) {
            sink.set_owner_status(OwnerStatus::Supported);
        }
        Ok(())
    }

    /// A debug log's facts as they are now, since a session can start between the scan and
    /// the removal. Anything else stays as the scan read it.
    fn revisit(
        &self,
        cx: &ScanContext<'_>,
        finding: &Finding,
    ) -> Result<Option<Supplied>, IntegrationError> {
        let debug_log = rule_of(cx, DEBUG_LOGS).is_some_and(|rule| rule.id == finding.rule);
        let (true, Some(facts)) = (debug_log, finding.facts.as_ref()) else {
            return Ok(None);
        };
        let launcher = launcher(cx);
        let stores = stores(cx, launcher.as_ref());
        let store = finding
            .path
            .parent()
            .and_then(Path::parent)
            .and_then(|config| stores.iter().find(|store| store.path == config));
        let session = finding.path.file_name().and_then(session_of);
        let (Some(store), Some(session)) = (store, session) else {
            return Ok(Some(Supplied {
                values: [(LAYOUT, FactValue::Unknown)].into(),
                ..facts.supplied.clone()
            }));
        };
        Ok(Some(Supplied {
            values: log_values(store, session),
            ..facts.supplied.clone()
        }))
    }
}

/// The first of the integration's rules on this platform using `selector`.
fn rule_of<'cx>(cx: &'cx ScanContext<'_>, selector: &'static str) -> Option<&'cx Rule> {
    cx.rules()
        .iter()
        .find(|rule| rule.selector == Selector::Integration(selector))
}

/// The launcher of a native install, which is what names the version in use.
#[derive(Debug)]
struct Launcher {
    /// The directory of versions it points into.
    versions: PathBuf,
    /// The version it points at, a name of the version grammar.
    version: String,
}

impl Launcher {
    fn supported(&self) -> FactValue {
        let verified = VERIFIED
            .iter()
            .any(|row| row.strip_prefix(VERIFIED_PREFIX) == Some(self.version.as_str()));
        if verified { SUPPORTED } else { UNSUPPORTED }
    }
}

/// The launcher as a native install leaves it: `~/.local/bin/claude`, a link holding the
/// absolute path of a version file in `claude.versions`. Anything else, a custom launcher
/// included, is no native install, so it shows no version.
fn launcher(cx: &ScanContext<'_>) -> Option<Launcher> {
    cx.roots_of(SymbolicRoot::Home).find_map(|home| {
        let link = home.path.join(".local").join("bin").join("claude");
        let target = cx.read_link(&link).ok()?;
        let name = target.file_name()?.to_str().filter(|n| is_version(n))?;
        let versions = target.parent()?;
        let in_store = cx
            .roots_of(SymbolicRoot::ClaudeVersions)
            .any(|store| versions == store.path.join("versions"));
        let installed = cx
            .metadata(&target)
            .is_ok_and(|meta| meta.kind == EntryKind::File);
        (in_store && installed).then(|| Launcher {
            versions: versions.to_owned(),
            version: name.to_owned(),
        })
    })
}

/// `^[0-9]+\.[0-9]+\.[0-9]+$`, the names upstream gives its versions.
fn is_version(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Whether a configuration directory is where Claude Code keeps its state, as far as its
/// settings say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Located {
    /// Nothing in its settings moves it.
    Here,
    /// Its `env` sets `CLAUDE_CONFIG_DIR`, which kipple's resolver can't see.
    Moved,
    /// Its settings can't be read, so that can't be said.
    Unknown,
}

/// What one configuration directory's `settings.json` says.
#[derive(Debug)]
struct Settings {
    /// `cleanupPeriodDays` in words.
    retention: String,
    /// Where it says the directory is.
    located: Located,
    /// Whether the file is there.
    present: bool,
}

fn read_settings(cx: &ScanContext<'_>, config: &Path) -> Settings {
    let file = config.join("settings.json");
    let default = || format!("{DEFAULT_DAYS} days, the default");
    match cx.read_file(&file, SETTINGS_LIMIT) {
        Ok(bytes) => {
            let parsed = serde_json::from_slice(&bytes).ok();
            let retention = match cleanup_period(parsed.as_ref()) {
                Setting::Unset => default(),
                Setting::Days(days) => format!("{days} days, from {}", file.display()),
                Setting::Unusable => format!("unknown: {} can't be used", file.display()),
            };
            Settings {
                retention,
                located: located(parsed.as_ref()),
                present: true,
            }
        }
        Err(error) if error.io_kind() == Some(ErrorKind::NotFound) => Settings {
            retention: default(),
            located: Located::Here,
            present: false,
        },
        Err(error) => Settings {
            retention: format!("unknown: {error}"),
            located: Located::Unknown,
            present: true,
        },
    }
}

/// `cleanupPeriodDays` as a settings file sets it.
enum Setting {
    /// Not set, so the default applies.
    Unset,
    /// Set to a valid number of days.
    Days(u64),
    /// The file isn't a JSON object, or the value isn't a number of days.
    Unusable,
}

fn cleanup_period(settings: Option<&serde_json::Value>) -> Setting {
    let Some(serde_json::Value::Object(settings)) = settings else {
        return Setting::Unusable;
    };
    let Some(days) = settings.get("cleanupPeriodDays") else {
        return Setting::Unset;
    };
    days.as_u64()
        .filter(|&days| days >= 1)
        .map_or(Setting::Unusable, Setting::Days)
}

/// What `settings` says of the directory. Only the key's presence is read, never its value.
fn located(settings: Option<&serde_json::Value>) -> Located {
    let Some(serde_json::Value::Object(settings)) = settings else {
        return Located::Unknown;
    };
    match settings.get("env") {
        Some(serde_json::Value::Object(env)) if env.contains_key("CLAUDE_CONFIG_DIR") => {
            Located::Moved
        }
        None | Some(serde_json::Value::Object(_)) => Located::Here,
        Some(_) => Located::Unknown,
    }
}

/// One path of the configuration directory, as this scan read it.
#[derive(Debug)]
struct Store<'cx> {
    path: &'cx Path,
    /// `cleanupPeriodDays` in words.
    retention: String,
    /// The `layout` fact.
    layout: FactValue,
    /// The `claude.version_supported` fact.
    version: FactValue,
    /// The sessions its markers name, in lowercase. `None` when one can't be read, since
    /// that marker might name any session.
    open_sessions: Option<BTreeSet<String>>,
}

/// Every path of the configuration directory. They are readings of one root, so what any of
/// them says about where the root is applies to all.
fn stores<'cx>(cx: &'cx ScanContext<'_>, launcher: Option<&Launcher>) -> Vec<Store<'cx>> {
    let read: Vec<(&Path, Settings)> = cx
        .roots_of(SymbolicRoot::ClaudeConfig)
        .map(|root| (root.path.as_path(), read_settings(cx, &root.path)))
        .collect();
    let located = read
        .iter()
        .map(|(_, settings)| settings.located)
        .max()
        .unwrap_or(Located::Here);
    let version = launcher.map_or(FactValue::Unknown, Launcher::supported);
    read.into_iter()
        .map(|(path, settings)| {
            let signature = if settings.present || config_file_present(cx, path) {
                Signature::Present
            } else {
                Signature::Absent
            };
            Store {
                path,
                layout: layout(located, signature, version),
                version,
                open_sessions: open_sessions(cx, path),
                retention: settings.retention,
            }
        })
        .collect()
}

/// Whether `.claude.json` is inside `config`, as with `$CLAUDE_CONFIG_DIR`, or beside it.
fn config_file_present(cx: &ScanContext<'_>, config: &Path) -> bool {
    let inside = config.join(".claude.json");
    let beside = config.parent().map(|dir| dir.join(".claude.json"));
    [Some(inside), beside]
        .iter()
        .flatten()
        .any(|file| cx.metadata(file).is_ok())
}

/// Whether a configuration directory holds what Claude Code writes there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signature {
    /// `settings.json`, or `.claude.json` in it or beside it.
    Present,
    /// Neither.
    Absent,
}

/// The `layout` fact: known only when its settings and its version are, and `supported` only
/// with the directory's signature, on a verified version, unmoved.
fn layout(located: Located, signature: Signature, version: FactValue) -> FactValue {
    match (located, signature, version) {
        (Located::Unknown, _, _) | (_, _, FactValue::Unknown) => FactValue::Unknown,
        (Located::Here, Signature::Present, SUPPORTED) => SUPPORTED,
        (Located::Here | Located::Moved, _, _) => UNKNOWN_LAYOUT,
    }
}

/// The session IDs in `<config>/sessions/*.json`. Missing, unreadable, or any marker that
/// can't be read or has no `sessionId` is `None`: the key's name is observed only, so an
/// upstream rename must block. `*.key` files sit beside them and are never opened.
fn open_sessions(cx: &ScanContext<'_>, config: &Path) -> Option<BTreeSet<String>> {
    let dir = config.join("sessions");
    let Listing::Entries(entries) = listing(cx, &dir) else {
        return None;
    };
    entries
        .iter()
        .filter(|entry| is_marker(&entry.name))
        .map(|marker| {
            let bytes = cx.read_file(&dir.join(&marker.name), MARKER_LIMIT).ok()?;
            session_id(&bytes)
        })
        .collect()
}

/// Whether `name` is a marker's. Any case counts: an unread marker reads as a closed session.
fn is_marker(name: &OsStr) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

/// The `sessionId` of a marker, in lowercase, and nothing else of it.
fn session_id(marker: &[u8]) -> Option<String> {
    let marker: serde_json::Value = serde_json::from_slice(marker).ok()?;
    let id = marker.get("sessionId")?.as_str()?;
    (!id.is_empty()).then(|| id.to_ascii_lowercase())
}

/// What a directory holds.
#[derive(Debug)]
enum Listing {
    /// Nothing is there.
    Absent,
    /// Something is, which isn't a directory or can't be listed.
    Unread,
    /// Its entries.
    Entries(Vec<DirEntryMeta>),
}

fn listing(cx: &ScanContext<'_>, dir: &Path) -> Listing {
    match cx.metadata(dir) {
        Ok(meta) if meta.kind == EntryKind::Dir => {
            cx.read_dir(dir).map_or(Listing::Unread, Listing::Entries)
        }
        Err(error) if error.io_kind() == Some(ErrorKind::NotFound) => Listing::Absent,
        Ok(_) | Err(_) => Listing::Unread,
    }
}

/// The session a debug log's file name names: `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-
/// [0-9a-f]{4}-[0-9a-f]{12}\.txt$` without the extension.
fn session_of(name: &OsStr) -> Option<&str> {
    name.to_str()?.strip_suffix(".txt").filter(|id| is_uuid(id))
}

fn is_uuid(id: &str) -> bool {
    let lengths = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = id.split('-').collect();
    parts.len() == lengths.len()
        && parts.iter().zip(lengths).all(|(part, length)| {
            part.len() == length
                && part
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// What a log of `store` supplies about `session`.
fn log_values(store: &Store<'_>, session: &str) -> BTreeMap<&'static str, FactValue> {
    let closed = store
        .open_sessions
        .as_ref()
        .map_or(FactValue::Unknown, |open| {
            FactValue::Bool(!open.contains(session))
        });
    [
        (LAYOUT, store.layout),
        (VERSION_SUPPORTED, store.version),
        (SESSION_CLOSED, closed),
    ]
    .into()
}

/// Where a finding of one configuration directory is being added from.
#[derive(Debug, Clone, Copy)]
struct At<'at, 'cx> {
    cx: &'at ScanContext<'cx>,
    store: &'at Store<'at>,
    advice: AdviceId,
}

/// Reports `path` under the store inventory, pointing to `advice`.
fn report(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    path: &Path,
    evidence: Evidence,
    advice: Option<AdviceId>,
) -> Result<(), IntegrationError> {
    let Some(rule) = rule_of(cx, STORE_ENTRIES) else {
        return Ok(());
    };
    let claim = Claim {
        evidence: vec![evidence],
        consequence: None,
    };
    let supplied = Supplied {
        advice,
        ..sink.supplied()
    };
    sink.add_supplied(rule, path, claim, supplied)
}

/// Adds everything in `debug/`. No directory is no finding: none has run with debug logging.
/// One that can't be listed is reported whole, since it could hold anything.
fn add_debug_entries(at: At<'_, '_>, sink: &mut FindingSink<'_>) -> Result<(), IntegrationError> {
    let debug = at.store.path.join("debug");
    match listing(at.cx, &debug) {
        Listing::Absent => Ok(()),
        Listing::Unread => report(at.cx, sink, &debug, Evidence::NoSignature, Some(at.advice)),
        Listing::Entries(entries) => entries
            .iter()
            .try_for_each(|entry| add_debug_entry(at, sink, &debug, entry)),
    }
}

/// Adds one entry of `debug/`. `latest` is upstream's link to the last session's log, which
/// is neither a candidate nor anything to report. Every other name is kept, and a log the
/// removing rule doesn't run for on this OS is reported.
fn add_debug_entry(
    at: At<'_, '_>,
    sink: &mut FindingSink<'_>,
    debug: &Path,
    entry: &DirEntryMeta,
) -> Result<(), IntegrationError> {
    if entry.name == "latest" && entry.kind == EntryKind::Symlink {
        return Ok(());
    }
    let path = debug.join(&entry.name);
    let log = session_of(&entry.name).filter(|_| entry.kind == EntryKind::File);
    let signature = || Evidence::Signature("<session>.txt".to_owned());
    match (log, rule_of(at.cx, DEBUG_LOGS)) {
        (Some(session), Some(rule)) => add_log(at, sink, rule, &path, session),
        (Some(_), None) => report(at.cx, sink, &path, signature(), Some(at.advice)),
        (None, _) => report(at.cx, sink, &path, Evidence::NoSignature, Some(at.advice)),
    }
}

fn add_log(
    at: At<'_, '_>,
    sink: &mut FindingSink<'_>,
    rule: &Rule,
    path: &Path,
    session: &str,
) -> Result<(), IntegrationError> {
    let verified = at.store.layout == SUPPORTED;
    let claim = Claim {
        evidence: vec![if verified {
            Evidence::Signature("<session>.txt".to_owned())
        } else {
            Evidence::NoSignature
        }],
        consequence: verified.then_some(CLOSED_LOG),
    };
    let supplied = Supplied {
        values: log_values(at.store, session),
        advice: Some(at.advice),
        ..sink.supplied()
    };
    sink.add_supplied(rule, path, claim, supplied)
}

/// What a report-only path holds. No failure here stops the integration: the removing rule
/// doesn't rest on any of these.
enum Reportable {
    /// Nothing, or nothing that can be there.
    Nothing,
    /// A file, or a directory with something in it.
    Something,
    /// The OS wouldn't say, so it is listed as unread.
    Unread,
}

fn reportable(cx: &ScanContext<'_>, path: &Path) -> Reportable {
    let holds = cx.metadata(path).and_then(|meta| {
        if meta.kind == EntryKind::Dir {
            cx.read_dir(path).map(|entries| !entries.is_empty())
        } else {
            Ok(true)
        }
    });
    match holds {
        Ok(true) => Reportable::Something,
        Ok(false) => Reportable::Nothing,
        Err(error) if cannot_be_there(&error) => Reportable::Nothing,
        Err(_) => Reportable::Unread,
    }
}

/// Whether a failed probe shows the path can't exist as upstream lays it out: nothing is
/// there, something on the way is a file, or it runs through a link kipple doesn't follow.
fn cannot_be_there(error: &ProbeError) -> bool {
    matches!(error, ProbeError::ThroughLink { .. })
        || matches!(
            error.io_kind(),
            Some(ErrorKind::NotFound | ErrorKind::NotADirectory)
        )
}

/// Lists `path` under `rule` as something the OS wouldn't describe.
fn add_refused(
    sink: &mut FindingSink<'_>,
    rule: &Rule,
    path: &Path,
) -> Result<(), IntegrationError> {
    let supplied = sink.supplied();
    sink.add_unread(rule, path, vec![Evidence::NoSignature], supplied)
}

/// Reports `path` under the store inventory, when there is anything to report.
fn report_entry(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    path: &Path,
    evidence: Evidence,
    advice: Option<AdviceId>,
) -> Result<(), IntegrationError> {
    match (reportable(cx, path), rule_of(cx, STORE_ENTRIES)) {
        (Reportable::Something, _) => report(cx, sink, path, evidence, advice),
        (Reportable::Unread, Some(rule)) => add_refused(sink, rule, path),
        (Reportable::Nothing, _) | (Reportable::Unread, None) => Ok(()),
    }
}

/// Adds what the inventory reports inside the configuration directory.
fn add_store_entries(at: At<'_, '_>, sink: &mut FindingSink<'_>) -> Result<(), IntegrationError> {
    let swept = SWEPT.iter().map(|name| (*name, Some(at.advice)));
    let unswept = UNSWEPT.iter().map(|name| (*name, None));
    for (name, advice) in swept.chain(unswept) {
        let path = name
            .split('/')
            .fold(at.store.path.to_owned(), |at, part| at.join(part));
        let evidence = Evidence::Signature(name.to_owned());
        report_entry(at.cx, sink, &path, evidence, advice)?;
    }
    Ok(())
}

/// Adds the leftover copies of `.claude.json`, inside each configuration directory and in
/// the home directory beside it. These are full copies of a config, so they are only ever
/// listed: a name and a size.
fn add_config_copies(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    stores: &[Store<'_>],
) -> Result<(), IntegrationError> {
    let homes: Vec<&Path> = cx
        .roots_of(SymbolicRoot::Home)
        .map(|home| home.path.as_path())
        .collect();
    let beside = stores
        .iter()
        .filter_map(|store| store.path.parent())
        .filter(|parent| homes.contains(parent));
    let dirs: BTreeSet<&Path> = stores
        .iter()
        .map(|store| store.path)
        .chain(beside)
        .collect();
    for dir in dirs {
        let copies = cx.read_dir(dir)?;
        let copies = copies.iter().filter(|entry| {
            entry.kind == EntryKind::File
                && entry
                    .name
                    .to_str()
                    .is_some_and(|name| name.starts_with(CONFIG_COPY))
        });
        for copy in copies {
            let evidence = Evidence::Signature(format!("{CONFIG_COPY}*"));
            report(cx, sink, &dir.join(&copy.name), evidence, None)?;
        }
    }
    Ok(())
}

/// Adds the directory of MCP logs under each cache directory there is.
fn add_mcp_logs(cx: &ScanContext<'_>, sink: &mut FindingSink<'_>) -> Result<(), IntegrationError> {
    for root in CACHE_ROOTS {
        for cache in cx.roots_of(*root) {
            let path = cache.path.join(MCP_LOGS);
            let evidence = Evidence::Signature(MCP_LOGS.to_owned());
            report_entry(cx, sink, &path, evidence, None)?;
        }
    }
    Ok(())
}

/// The versions a lock file names in `locks/`, by file name alone. A lock is
/// `<version>.lock` or `<version>#<pid>.lock`. Upstream's stale locks can outlive a process,
/// so this says only that a version may run, and nothing about one with no lock.
fn locked_versions(cx: &ScanContext<'_>) -> BTreeSet<String> {
    let locks = cx
        .roots_of(SymbolicRoot::ClaudeInstallState)
        .filter_map(|state| cx.read_dir(&state.path.join("locks")).ok())
        .flatten();
    locks
        .filter_map(|lock| {
            let name = lock.name.to_str()?.strip_suffix(".lock")?;
            let version = name.split_once('#').map_or(name, |(version, _)| version);
            is_version(version).then(|| version.to_owned())
        })
        .collect()
}

/// Adds each version of a native install, and whatever is staged. No launcher into the store
/// means no native install, which reports nothing, and only a regular file of the version
/// grammar is a version.
fn add_versions(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    launcher: Option<&Launcher>,
) -> Result<(), IntegrationError> {
    let (Some(rule), Some(launcher)) = (rule_of(cx, VERSIONS), launcher) else {
        return Ok(());
    };
    let running = locked_versions(cx);
    for entry in cx.read_dir(&launcher.versions)? {
        let version = entry.name.to_str().filter(|name| is_version(name));
        let (Some(version), EntryKind::File) = (version, entry.kind) else {
            continue;
        };
        let claim = Claim {
            evidence: vec![Evidence::Signature("versions/<version>".to_owned())],
            consequence: Some(INSTALLED_VERSION),
        };
        let running = running.contains(version).then_some(FactValue::Bool(true));
        let supplied = Supplied {
            values: [
                (
                    LAUNCHER_TARGET,
                    FactValue::Bool(version == launcher.version),
                ),
                (VERSION_RUNNING, running.unwrap_or(FactValue::Unknown)),
            ]
            .into(),
            ..sink.supplied()
        };
        sink.add_supplied(rule, &launcher.versions.join(&entry.name), claim, supplied)?;
    }
    add_staging(cx, sink, rule)
}

/// Adds the installer's staged downloads, when there are any.
fn add_staging(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    rule: &Rule,
) -> Result<(), IntegrationError> {
    for cache in cx.roots_of(SymbolicRoot::XdgCache) {
        let staging = cache.path.join("claude").join("staging");
        match reportable(cx, &staging) {
            Reportable::Nothing => {}
            Reportable::Unread => add_refused(sink, rule, &staging)?,
            Reportable::Something => {
                let evidence = vec![Evidence::Signature("staging".to_owned())];
                sink.add(rule, &staging, evidence, None)?;
            }
        }
    }
    Ok(())
}

fn advice(setting: &str) -> Advice {
    Advice {
        title: "Claude Code's own transcript retention".to_owned(),
        text: format!(
            "cleanupPeriodDays in your user settings: {setting}. Claude Code deletes \
             transcripts, file history, plans, debug logs and other session data older than \
             this, and the same cutoff removes the orphaned worktrees it made. The default is \
             30 and the minimum 1; 0 fails validation. Managed settings, and project or \
             command-line settings, override the user setting, and kipple doesn't read them. \
             It never deletes auto memory files (versions before v2.1.228 could delete old \
             files below a memory directory) and leaves history.jsonl. Since v2.1.248 it keeps \
             Claude Desktop and Cowork transcripts unless desktopSessionCleanupPeriodDays is \
             set, which deletes only past both limits, and a managed cleanupPeriodDays or \
             HIPAA setting overrides that. claude -p --bare doesn't sweep, and when it can't \
             determine the retention period it pauses the sweep, which deletes nothing."
        ),
        snippets: vec!["{ \"cleanupPeriodDays\": 14 }".to_owned()],
        references: vec![
            "https://code.claude.com/docs/en/claude-directory#cleaned-up-automatically".to_owned(),
            "https://code.claude.com/docs/en/settings".to_owned(),
        ],
    }
}
