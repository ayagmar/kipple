//! The Pi coding agent (`docs/06-adapters-and-rules.md` §5): the diagnostic logs of a managed
//! install, and an inventory of what else it keeps. Verified against Pi 1.5.0, from its source
//! and on Linux. Its logs are matched by exact name, never as `*.log`, because extensions write
//! their own beside them.
//!
//! Each path of its agent directory is checked on its own: a value kipple reads two ways can
//! leave one that isn't Pi's store at all. Anything but a managed install of a verified version
//! is reported whole as an unknown layout, and its files stay closed to everyone.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::Path;

use kipple_core::{
    ActivityEvidence, Area, Claim, Consequence, Cost, DirEntryMeta, EntryKind, Evidence, FactKind,
    FactSpec, FactValue, Finding, FindingSink, Integration, IntegrationDescriptor,
    IntegrationError, IntegrationId, IntegrationSelector, Lens, Loss, OpenArea, OwnerStatus,
    Platform, ProbeError, Program, ProtectedEntry, Recovery, Rule, ScanContext, Selector,
    StateDeclaration, Supplied, SymbolicRoot, Unresolved,
};

const ID: IntegrationId = IntegrationId("pi");

const CLOSED_LOGS: &str = "pi-closed-logs";
const STORE_ENTRIES: &str = "pi-store-entries";

const SELECTORS: &[IntegrationSelector] = &[
    IntegrationSelector {
        name: CLOSED_LOGS,
        meaning: "the diagnostic logs a verified store names, each by its exact name",
    },
    IntegrationSelector {
        name: STORE_ENTRIES,
        meaning: "entries of the agent directory that are reported and never removed",
    },
];

/// What each row was verified as, which is what a version must equal to be supported.
const VERIFIED: &[&str] = &["Pi 1.5.0"];
const VERIFIED_PREFIX: &str = "Pi ";

const LAYOUT: &str = "layout";
const VERSION_SUPPORTED: &str = "pi.version_supported";
const INSTALLER_ACTIVITY: &str = "installer_activity";

const SUPPORTED: FactValue = FactValue::Enum("supported");
const UNSUPPORTED: FactValue = FactValue::Enum("unsupported");
const UNKNOWN_LAYOUT: FactValue = FactValue::Enum("unknown");
const IDLE: FactValue = FactValue::Enum("idle");
const BUSY: FactValue = FactValue::Enum("busy");

const FACTS: &[FactSpec] = &[
    FactSpec {
        name: LAYOUT,
        kind: FactKind::Enum(&["supported", "unknown"]),
        selector: None,
        meaning: "whether the agent directory is a managed install of a verified version",
    },
    FactSpec {
        name: VERSION_SUPPORTED,
        kind: FactKind::Enum(&["supported", "unsupported"]),
        selector: None,
        meaning: "whether `install/current-version` equals a verified version; none that can \
                  be read is unknown",
    },
    FactSpec {
        name: INSTALLER_ACTIVITY,
        kind: FactKind::Enum(&["idle", "busy"]),
        selector: Some(CLOSED_LOGS),
        meaning: "whether `install/update.lock/` says an update is running",
    },
];

/// Pi doesn't write these again by itself, and they are diagnostics, which 04 §2a counts as
/// history, so they are Review at every level.
const CLOSED_LOG: Consequence = Consequence {
    recovery: Recovery::None,
    loss: &[Loss::History],
    cost: Cost::Low,
};

/// The logs that may go, each directly in the agent directory. Nothing holds one open between
/// writes, and `mcp.log.1` is written only by the rename that makes it.
const LOGS: &[&str] = &[
    "pi-debug.log",
    "pi-tui-crash.log",
    "pi-tui-debug.log",
    "mcp.log.1",
];

/// Files of the agent directory the inventory reports. `.exe` is what Windows has.
const REPORTED_FILES: &[&str] = &[
    "mcp.log",
    "crashes.json",
    "bin/fd",
    "bin/rg",
    "bin/fd.exe",
    "bin/rg.exe",
];

const fn open_to_pi(name: &'static str) -> OpenArea {
    OpenArea {
        at: Area::Exact(name),
        integrations: &[ID],
    }
}

/// What stays closed in the agent directory. Only [`LOGS`] open, so this says what `rules
/// --json` shows. `proper-lockfile` makes a `<file>.lock/` directory beside each file below.
const PROTECTED: &[ProtectedEntry] = &[
    ProtectedEntry::Exact("auth.json"),
    ProtectedEntry::Exact("auth.json.lock"),
    ProtectedEntry::Exact("mcp-auth.json"),
    ProtectedEntry::Exact("mcp-auth.json.lock"),
    ProtectedEntry::Prefix("mcp-auth-refresh-"),
    ProtectedEntry::Exact("settings.json"),
    ProtectedEntry::Exact("settings.json.lock"),
    ProtectedEntry::Exact("keybindings.json"),
    ProtectedEntry::Exact("mcp.json"),
    ProtectedEntry::Exact("models.json"),
    ProtectedEntry::Exact("models-store.json"),
    ProtectedEntry::Exact("models-store.json.lock"),
    ProtectedEntry::Exact("trust.json"),
    ProtectedEntry::Exact("trust.json.lock"),
    ProtectedEntry::Exact("crashes.json"),
    ProtectedEntry::Prefix("AGENTS"),
    ProtectedEntry::Prefix("CLAUDE"),
    ProtectedEntry::Exact("SYSTEM.md"),
    ProtectedEntry::Exact("APPEND_SYSTEM.md"),
    ProtectedEntry::Exact("sessions"),
    ProtectedEntry::Exact("extensions"),
    ProtectedEntry::Exact("skills"),
    ProtectedEntry::Exact("prompts"),
    ProtectedEntry::Exact("themes"),
    ProtectedEntry::Exact("git"),
    ProtectedEntry::Exact("npm"),
    ProtectedEntry::Exact("tmp"),
    ProtectedEntry::Exact("bin"),
    ProtectedEntry::Exact("install"),
    ProtectedEntry::Exact("win-git-bash"),
];

/// `$PI_CODING_AGENT_DIR` may hold credentials anywhere, so one that can't be used holds
/// everything.
const STATE: &[StateDeclaration] = &[
    StateDeclaration {
        root: SymbolicRoot::PiAgent,
        protected: PROTECTED,
        open: &[
            open_to_pi("pi-debug.log"),
            open_to_pi("pi-tui-crash.log"),
            open_to_pi("pi-tui-debug.log"),
            open_to_pi("mcp.log.1"),
        ],
        delegates: &[],
        unresolved: Unresolved::HoldAll,
    },
    StateDeclaration {
        root: SymbolicRoot::PiHome,
        protected: &[],
        open: &[],
        delegates: &[SymbolicRoot::PiAgent],
        unresolved: Unresolved::HoldOwn,
    },
    StateDeclaration {
        root: SymbolicRoot::PiSessionDir,
        protected: &[],
        open: &[],
        delegates: &[],
        unresolved: Unresolved::HoldOwn,
    },
];

/// More than `managed-install.json` holds.
const MARKER_LIMIT: u64 = 64 * 1024;

/// More than `current-version` holds. One cut short here matches no version.
const VERSION_LIMIT: u64 = 256;

/// What Windows PowerShell 5.1 can put at the start of the marker.
const BOM: &[u8] = b"\xef\xbb\xbf";

/// The Pi integration.
#[derive(Debug)]
pub(crate) struct Pi;

impl Integration for Pi {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: ID,
            lens: Lens::Agents,
            platforms: Platform::ALL,
            roots: &[],
            selectors: SELECTORS,
            artifacts: &[],
            owner_files: &[],
            verified: VERIFIED,
            consequences: &[CLOSED_LOG],
            facts: FACTS,
            state: STATE,
            evidences: &[ActivityEvidence::ToolRunning {
                programs: &[Program::Exact("pi"), Program::Exact("pi-rpc")],
                scripts: &[],
                runtimes: &[],
                stores: &[SymbolicRoot::PiAgent],
            }],
            revision: 2,
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let mut supported = false;
        for root in cx.roots_of(SymbolicRoot::PiAgent) {
            match read_store(cx, &root.path) {
                Store::Verified(version) => {
                    supported = true;
                    add_logs(cx, sink, &root.path)?;
                    add_inventory(cx, sink, &root.path, &version)?;
                }
                Store::Unknown(version) => add_unknown(cx, sink, &root.path, version)?,
            }
        }
        if supported {
            sink.set_owner_status(OwnerStatus::Supported);
        }
        Ok(())
    }

    /// A log's facts as they are now, since an update can start between the scan and the
    /// removal. Anything else stays as the scan read it.
    fn revisit(
        &self,
        cx: &ScanContext<'_>,
        finding: &Finding,
    ) -> Result<Option<Supplied>, IntegrationError> {
        let log = rule_of(cx, CLOSED_LOGS).is_some_and(|rule| rule.id == finding.rule);
        let (true, Some(facts)) = (log, finding.facts.as_ref()) else {
            return Ok(None);
        };
        let values = match finding
            .path
            .parent()
            .map(|agent| (agent, read_store(cx, agent)))
        {
            Some((agent, Store::Verified(_))) => log_values(cx, agent),
            Some((_, Store::Unknown(version))) => unknown_values(version),
            None => unknown_values(FactValue::Unknown),
        };
        Ok(Some(Supplied {
            values,
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

/// What one path of the agent directory is.
#[derive(Debug)]
enum Store {
    /// A managed install of this verified version, with every signature of S5 pi §3.
    Verified(String),
    /// Anything else, with the `pi.version_supported` fact.
    Unknown(FactValue),
}

fn read_store(cx: &ScanContext<'_>, agent: &Path) -> Store {
    let install = agent.join("install");
    if !is_managed(cx, &install) {
        return Store::Unknown(FactValue::Unknown);
    }
    let Some(version) = current_version(cx, &install) else {
        return Store::Unknown(FactValue::Unknown);
    };
    if !verified(&version) {
        let value = if is_release_name(&version) {
            UNSUPPORTED
        } else {
            FactValue::Unknown
        };
        return Store::Unknown(value);
    }
    if has_entry_point(cx, &install, &version) {
        Store::Verified(version)
    } else {
        Store::Unknown(SUPPORTED)
    }
}

/// Whether `install/managed-install.json` is the marker of a `releases-v1` layout.
fn is_managed(cx: &ScanContext<'_>, install: &Path) -> bool {
    let marker = install.join("managed-install.json");
    cx.read_file(&marker, MARKER_LIMIT)
        .is_ok_and(|bytes| is_marker(&bytes))
}

fn is_marker(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(BOM).unwrap_or(bytes);
    let Ok(marker) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    marker.get("kind").and_then(serde_json::Value::as_str) == Some("pi-managed-install")
        && marker
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            == Some(1)
        && marker.get("layout").and_then(serde_json::Value::as_str) == Some("releases-v1")
}

/// `install/current-version` without its line ending, which is `\r\n` on Windows.
fn current_version(cx: &ScanContext<'_>, install: &Path) -> Option<String> {
    let bytes = cx
        .read_file(&install.join("current-version"), VERSION_LIMIT)
        .ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    let line = text.strip_suffix('\n').unwrap_or(text);
    Some(line.strip_suffix('\r').unwrap_or(line).to_owned())
}

fn verified(version: &str) -> bool {
    VERIFIED
        .iter()
        .any(|row| row.strip_prefix(VERIFIED_PREFIX) == Some(version))
}

/// `^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$`, upstream's release directory
/// names.
fn is_release_name(name: &str) -> bool {
    let tail = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    };
    let (rest, build) = name
        .split_once('+')
        .map_or((name, None), |(a, b)| (a, Some(b)));
    let (core, prerelease) = rest
        .split_once('-')
        .map_or((rest, None), |(a, b)| (a, Some(b)));
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && prerelease.is_none_or(tail)
        && build.is_none_or(tail)
}

/// Whether the release `version` has what the launcher runs: `pi`, or `pi.cmd` on Windows. A
/// half-written install has neither.
fn has_entry_point(cx: &ScanContext<'_>, install: &Path, version: &str) -> bool {
    let bin = install
        .join("releases")
        .join(version)
        .join("node_modules")
        .join(".bin");
    ["pi", "pi.cmd"]
        .iter()
        .any(|name| cx.metadata(&bin.join(name)).is_ok())
}

/// The `installer_activity` fact: `update.lock/` is there while an update runs.
fn installer_activity(cx: &ScanContext<'_>, agent: &Path) -> FactValue {
    let lock = agent.join("install").join("update.lock");
    match cx.metadata(&lock) {
        Ok(_) => BUSY,
        Err(error) if error.io_kind() == Some(ErrorKind::NotFound) => IDLE,
        Err(_) => FactValue::Unknown,
    }
}

/// What a log of the verified store at `agent` supplies.
fn log_values(cx: &ScanContext<'_>, agent: &Path) -> BTreeMap<&'static str, FactValue> {
    [
        (LAYOUT, SUPPORTED),
        (VERSION_SUPPORTED, SUPPORTED),
        (INSTALLER_ACTIVITY, installer_activity(cx, agent)),
    ]
    .into()
}

/// What anything of a store that isn't verified supplies.
fn unknown_values(version: FactValue) -> BTreeMap<&'static str, FactValue> {
    [(LAYOUT, UNKNOWN_LAYOUT), (VERSION_SUPPORTED, version)].into()
}

/// Reports the store at `agent` whole, as a layout kipple doesn't know. An empty one is what
/// Pi's first run makes, and has nothing to report.
fn add_unknown(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    agent: &Path,
    version: FactValue,
) -> Result<(), IntegrationError> {
    let Some(rule) = rule_of(cx, STORE_ENTRIES) else {
        return Ok(());
    };
    if cx.read_dir(agent)?.is_empty() {
        return Ok(());
    }
    let claim = Claim {
        evidence: vec![Evidence::NoSignature],
        consequence: None,
    };
    let supplied = Supplied {
        values: unknown_values(version),
        ..sink.supplied()
    };
    sink.add_supplied(rule, agent, claim, supplied)
}

/// What `path` itself is, `None` when nothing is there.
fn kind_of(cx: &ScanContext<'_>, path: &Path) -> Result<Option<EntryKind>, ProbeError> {
    match cx.metadata(path) {
        Ok(meta) => Ok(Some(meta.kind)),
        Err(error) if error.io_kind() == Some(ErrorKind::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Adds each log that is a regular file: under the removing rule where it runs, and under the
/// inventory where it doesn't. A link or a directory of one of these names is none of Pi's.
/// Each is matched against the listing: on a disk that folds case, probing the literal name
/// would land on a file the rule doesn't name, under a path the host's exact area opens.
fn add_logs(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    agent: &Path,
) -> Result<(), IntegrationError> {
    let entries = cx.read_dir(agent)?;
    for name in LOGS {
        let path = agent.join(name);
        let is_log = |entry: &DirEntryMeta| entry.name == *name && entry.kind == EntryKind::File;
        if !entries.iter().any(is_log) {
            continue;
        }
        let Some(rule) = rule_of(cx, CLOSED_LOGS) else {
            report(cx, sink, &path, Evidence::Signature((*name).to_owned()))?;
            continue;
        };
        let claim = Claim {
            evidence: vec![Evidence::Signature((*name).to_owned())],
            consequence: Some(CLOSED_LOG),
        };
        let supplied = Supplied {
            values: log_values(cx, agent),
            ..sink.supplied()
        };
        sink.add_supplied(rule, &path, claim, supplied)?;
    }
    Ok(())
}

/// Reports `path` under the store inventory.
fn report(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    path: &Path,
    evidence: Evidence,
) -> Result<(), IntegrationError> {
    rule_of(cx, STORE_ENTRIES).map_or(Ok(()), |rule| sink.add(rule, path, vec![evidence], None))
}

/// Whether a failed probe shows the path can't exist as upstream lays it out: something on
/// the way is a file, or it runs through a link kipple doesn't follow.
fn cannot_be_there(error: &ProbeError) -> bool {
    matches!(error, ProbeError::ThroughLink { .. })
        || error.io_kind() == Some(ErrorKind::NotADirectory)
}

/// Takes a failed probe of the report-only `path`, which never stops the integration, since
/// the removing rule doesn't rest on it: what can't be there is nothing to report, and
/// anything else is listed as unread.
fn skip(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    path: &Path,
    error: &ProbeError,
) -> Result<(), IntegrationError> {
    let Some(rule) = rule_of(cx, STORE_ENTRIES).filter(|_| !cannot_be_there(error)) else {
        return Ok(());
    };
    let supplied = sink.supplied();
    sink.add_unread(rule, path, vec![Evidence::NoSignature], supplied)
}

/// The entries of the report-only directory at `dir`. None when nothing is there.
fn entries_of(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    dir: &Path,
) -> Result<Vec<DirEntryMeta>, IntegrationError> {
    let entries = kind_of(cx, dir).and_then(|kind| match kind {
        Some(EntryKind::Dir) => cx.read_dir(dir),
        Some(_) | None => Ok(Vec::new()),
    });
    match entries {
        Ok(entries) => Ok(entries),
        Err(error) => skip(cx, sink, dir, &error).map(|()| Vec::new()),
    }
}

/// Adds what the inventory reports of the verified store at `agent`, whose active release is
/// `version`. Which release runs can't be proved, so an inactive one is only ever reported.
fn add_inventory(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    agent: &Path,
    version: &str,
) -> Result<(), IntegrationError> {
    for name in REPORTED_FILES {
        let path = name
            .split('/')
            .fold(agent.to_owned(), |at, part| at.join(part));
        match kind_of(cx, &path) {
            Ok(Some(_)) => report(cx, sink, &path, Evidence::Signature((*name).to_owned()))?,
            Ok(None) => {}
            Err(error) => skip(cx, sink, &path, &error)?,
        }
    }
    let releases = agent.join("install").join("releases");
    for release in entries_of(cx, sink, &releases)? {
        if release.name != version {
            let known = release.name.to_str().is_some_and(is_release_name);
            let evidence = if known {
                Evidence::Signature("install/releases/<version>".to_owned())
            } else {
                Evidence::NoSignature
            };
            report(cx, sink, &releases.join(&release.name), evidence)?;
        }
    }
    let staging = agent.join("install").join("staging");
    for entry in entries_of(cx, sink, &staging)? {
        let evidence = Evidence::Signature("install/staging".to_owned());
        report(cx, sink, &staging.join(&entry.name), evidence)?;
    }
    let extensions = agent.join("tmp").join("extensions");
    if !entries_of(cx, sink, &extensions)?.is_empty() {
        report(
            cx,
            sink,
            &extensions,
            Evidence::Signature("tmp/extensions".to_owned()),
        )?;
    }
    Ok(())
}
