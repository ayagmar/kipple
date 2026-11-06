//! Codex (`docs/06-adapters-and-rules.md` §4): its two release stores, its one legacy log,
//! its plugin catalog cache and what the rest of its home holds. Read from upstream at
//! `rust-v0.165.0` and `rust-v0.166.0`, with a fixture for each.
//!
//! Its home stays closed unless every selected release is a version `verified` lists, so a
//! Codex newer than that reports and removes nothing. Retention isn't decided here: each
//! release is supplied with what its store says about it, and the core settles the group.
//!
//! Only the key `log_dir` is read from a config file, and `auth.json`, sessions and history
//! are never opened. A parser's error can quote what it read, so none is kept.

use std::collections::BTreeMap;
use std::path::{Component as Part, Path, PathBuf};

use kipple_core::{
    ActivityEvidence, Advice, Area, Claim, Consequence, Cost, DirEntryMeta, EntryKind, Evidence,
    FactKind, FactSpec, FactValue, FindingSink, GroupGap, Integration, IntegrationDescriptor,
    IntegrationError, IntegrationId, IntegrationSelector, Lens, Loss, OpenArea, OwnerStatus,
    Platform, ProbeError, Program, ProtectedEntry, Recovery, ReleaseFacts, ReleaseGroup,
    ReleaseRole, ReleaseState, ReleaseVersion, Rule, ScanContext, Selector, StateDeclaration,
    Subject, Supplied, SymbolicRoot, Unresolved,
};
use serde_json::Value as Json;
use toml::de::{DeTable, DeValue};

const RELEASES: &str = "verified-release-directories";
const RELEASE_ENTRIES: &str = "codex-release-entries";
const TUI_LOG: &str = "codex-legacy-tui-log";
const CATALOG: &str = "codex-plugin-catalog-files";
const STORE_ENTRIES: &str = "codex-store-entries";

const SELECTORS: &[IntegrationSelector] = &[
    IntegrationSelector {
        name: RELEASES,
        meaning: "inactive release directories of a verified store",
    },
    IntegrationSelector {
        name: RELEASE_ENTRIES,
        meaning: "everything in the release store that is reported and never removed",
    },
    IntegrationSelector {
        name: TUI_LOG,
        meaning: "the TUI log in the default log directory",
    },
    IntegrationSelector {
        name: CATALOG,
        meaning: "cached remote plugin catalog files",
    },
    IntegrationSelector {
        name: STORE_ENTRIES,
        meaning: "entries of the home that are reported and never removed",
    },
];

/// The upstream versions each with a fixture. A version is supported only when it equals a
/// row exactly, so adding one is a fixture file and a row here.
const VERIFIED: &[&str] = &["Codex 0.165.0", "Codex 0.166.0"];

/// What every row of [`VERIFIED`] starts with.
const VERIFIED_PREFIX: &str = "Codex ";

/// The oldest version known to leave `<home>/log/codex-tui.log` alone with `log_dir` unset.
/// Which release stopped writing it isn't known, only that this one doesn't.
const TUI_LOG_SILENT_FROM: &str = "0.166.0";

const LAYOUT: &str = "layout";
const VERSION_SUPPORTED: &str = "codex.version_supported";
const INVENTORY: &str = "inventory";
const INSTALLER_ACTIVITY: &str = "installer_activity";
const LOG_DIR_CONFIGURED: &str = "log_dir.configured";
const TUI_LOG_WRITER: &str = "tui_log.writer";

const FACTS: &[FactSpec] = &[
    FactSpec {
        name: LAYOUT,
        kind: FactKind::Enum(&["supported"]),
        selector: None,
        meaning: "only known components, each selecting a release of a verified version",
    },
    FactSpec {
        name: VERSION_SUPPORTED,
        kind: FactKind::Enum(&["supported", "unsupported"]),
        selector: None,
        meaning: "whether every selected release is a version kipple was verified against",
    },
    FactSpec {
        name: INVENTORY,
        kind: FactKind::Enum(&["complete"]),
        selector: Some(RELEASES),
        meaning: "every entry of the release's store could be read and classified",
    },
    FactSpec {
        name: INSTALLER_ACTIVITY,
        kind: FactKind::Enum(&["idle", "busy"]),
        selector: Some(RELEASES),
        meaning: "whether the store holds a marker of an install in progress",
    },
    FactSpec {
        name: LOG_DIR_CONFIGURED,
        kind: FactKind::Bool,
        selector: Some(TUI_LOG),
        meaning: "whether a config file in the Codex home sets `log_dir`",
    },
    FactSpec {
        name: TUI_LOG_WRITER,
        kind: FactKind::Enum(&["none", "possible"]),
        selector: Some(TUI_LOG),
        meaning: "whether a selected release may still append to the log",
    },
];

/// Upstream's own test for a process of a store: its names, and any file below a release.
const RELEASE_ACTIVITY: ActivityEvidence = ActivityEvidence::ReleaseStore {
    names: &[
        Program::Prefix("codex"),
        Program::Exact("rg"),
        Program::Exact("bwrap"),
    ],
};

/// Nothing shows who has a log or a cache open but a process of the tool. A `codex` from
/// outside the store, such as an npm install, writes the same home.
const HOME_ACTIVITY: ActivityEvidence = ActivityEvidence::ToolRunning {
    programs: &[Program::Prefix("codex")],
    scripts: &[],
    runtimes: &[],
    stores: &[SymbolicRoot::CodexReleaseStore],
};

/// A release comes back with an install, and until then Codex can't roll back to it offline.
const REDOWNLOADED: Consequence = Consequence {
    recovery: Recovery::Redownload,
    loss: &[Loss::OfflineAbility],
    cost: Cost::Unknown,
};

/// Old diagnostics are gone for good, which is why a person always reviews them.
const DIAGNOSTICS: Consequence = Consequence {
    recovery: Recovery::None,
    loss: &[Loss::History],
    cost: Cost::Low,
};

/// Codex fetches the catalog again on its next refresh, and a missing file is a plain miss.
const REFETCHED: Consequence = Consequence {
    recovery: Recovery::Automatic,
    loss: &[Loss::CacheWarmth],
    cost: Cost::Low,
};

const OWN: &[IntegrationId] = &[IntegrationId("codex")];

/// What may be reached in the home. Everything else in it stays closed, and that includes
/// the `<name>.config.toml` profiles, which no [`ProtectedEntry`] can spell.
const OPEN: &[OpenArea] = &[
    OpenArea {
        at: Area::Inside("packages/standalone/releases"),
        integrations: OWN,
    },
    OpenArea {
        at: Area::Inside("packages/app-server-daemon/releases"),
        integrations: OWN,
    },
    OpenArea {
        at: Area::Exact("log/codex-tui.log"),
        integrations: OWN,
    },
    OpenArea {
        at: Area::Inside("cache/remote_plugin_catalog"),
        integrations: OWN,
    },
    OpenArea {
        at: Area::Inside("worktrees"),
        integrations: &[IntegrationId::GIT],
    },
];

/// Every protected row of S5 codex §5 and §8. `.tmp` holds live plugin sources despite its
/// name, and it isn't `tmp`. `app-server-daemon` holds the logs and pid files that decide
/// which store a daemon resolves to.
const PROTECTED: &[ProtectedEntry] = &[
    ProtectedEntry::Prefix("packages/standalone/releases/.staging."),
    ProtectedEntry::Prefix("packages/standalone/releases/.retired."),
    ProtectedEntry::Prefix("packages/standalone/releases/local-"),
    ProtectedEntry::Prefix("packages/app-server-daemon/releases/.staging."),
    ProtectedEntry::Prefix("packages/app-server-daemon/releases/.retired."),
    ProtectedEntry::Prefix("packages/app-server-daemon/releases/local-"),
    ProtectedEntry::Exact("packages/standalone/current"),
    ProtectedEntry::Exact("packages/standalone/selection-history"),
    ProtectedEntry::Exact("packages/standalone/auto-update-version"),
    ProtectedEntry::Exact("packages/standalone/install.lock"),
    ProtectedEntry::Exact("packages/standalone/install.lock.d"),
    ProtectedEntry::Prefix("packages/standalone/install-lock-"),
    ProtectedEntry::Prefix("packages/standalone/.current."),
    ProtectedEntry::Exact("packages/app-server-daemon/current"),
    ProtectedEntry::Exact("packages/app-server-daemon/selection-history"),
    ProtectedEntry::Exact("packages/app-server-daemon/auto-update-version"),
    ProtectedEntry::Exact("packages/app-server-daemon/install.lock"),
    ProtectedEntry::Exact("packages/app-server-daemon/install.lock.d"),
    ProtectedEntry::Exact("packages/app-server-daemon/.migration-current"),
    ProtectedEntry::Prefix("packages/app-server-daemon/install-lock-"),
    ProtectedEntry::Prefix("packages/app-server-daemon/.current."),
    ProtectedEntry::Exact("app-server-daemon"),
    ProtectedEntry::Exact("app-server-control"),
    ProtectedEntry::Prefix("state_5.sqlite"),
    ProtectedEntry::Prefix("goals_1.sqlite"),
    ProtectedEntry::Prefix("memories_1.sqlite"),
    ProtectedEntry::Prefix("memories_v2_1.sqlite"),
    ProtectedEntry::Prefix("queue_1.sqlite"),
    ProtectedEntry::Prefix("thread_history_1.sqlite"),
    ProtectedEntry::Prefix("agent_message_board_1.sqlite"),
    ProtectedEntry::Prefix("message_board_"),
    ProtectedEntry::Exact("db-backups"),
    ProtectedEntry::Exact("sqlite"),
    ProtectedEntry::Exact("version.json"),
    ProtectedEntry::Exact(".tmp"),
    ProtectedEntry::Exact("plugins"),
    ProtectedEntry::Exact("skills"),
    ProtectedEntry::Exact("rules"),
    ProtectedEntry::Exact("hooks.json"),
    ProtectedEntry::Exact("config.toml"),
    ProtectedEntry::Exact("requirements.toml"),
    ProtectedEntry::Exact("managed_config.toml"),
    ProtectedEntry::Exact("AGENTS.md"),
    ProtectedEntry::Exact("auth.json"),
    ProtectedEntry::Exact("auth.lock"),
    ProtectedEntry::Exact(".env"),
    ProtectedEntry::Exact("secrets"),
    ProtectedEntry::Prefix(".sandbox"),
    ProtectedEntry::Exact("cap_sid"),
    ProtectedEntry::Exact("installation_id"),
    ProtectedEntry::Exact("history.jsonl"),
    ProtectedEntry::Exact("sessions"),
    ProtectedEntry::Exact("archived_sessions"),
    ProtectedEntry::Prefix("session_index.jsonl"),
    ProtectedEntry::Exact("rollout-migrations"),
    ProtectedEntry::Exact("tui-thread-reference-capabilities"),
    ProtectedEntry::Exact("attachments"),
    ProtectedEntry::Exact("visualizations"),
    ProtectedEntry::Exact("generated_images"),
    ProtectedEntry::Exact("memories"),
    ProtectedEntry::Exact("memories_v2"),
    ProtectedEntry::Exact("memories_extensions"),
    ProtectedEntry::Exact("memories.lock"),
    ProtectedEntry::Exact("ambient-suggestions"),
    ProtectedEntry::Exact("code-review-plugin"),
    ProtectedEntry::Exact("computer-use"),
    ProtectedEntry::Exact("node_repl"),
    ProtectedEntry::Exact("project-metadata-locks"),
    ProtectedEntry::Exact("vendor_imports"),
    ProtectedEntry::Prefix(".codex-global-state.json"),
];

/// `$CODEX_HOME` may hold credentials anywhere, so one that can't be used holds everything.
const STATE: &[StateDeclaration] = &[
    StateDeclaration {
        root: SymbolicRoot::CodexHome,
        protected: PROTECTED,
        open: OPEN,
        delegates: &[],
        unresolved: Unresolved::HoldAll,
    },
    StateDeclaration {
        root: SymbolicRoot::CodexDefaultHome,
        protected: &[],
        open: &[],
        delegates: &[SymbolicRoot::CodexHome],
        unresolved: Unresolved::HoldOwn,
    },
    StateDeclaration {
        root: SymbolicRoot::CodexSqliteHome,
        protected: &[],
        open: &[],
        delegates: &[],
        unresolved: Unresolved::HoldOwn,
    },
];

/// The components of the store, which are all `packages/` may hold.
const COMPONENTS: [&str; 2] = ["standalone", "app-server-daemon"];

/// The component whose releases also need the daemon's lifecycle lock.
const DAEMON: &str = "app-server-daemon";

/// The targets a store may hold, by upstream's list. The `-gnu` ones are left out for lack
/// of a fixture, so such a release is kept and a store selecting one stays closed.
const TARGETS: [&str; 6] = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
];

/// A release's manifest, the signature of everything else.
const MANIFEST: &str = "codex-package.json";

/// Larger than any manifest: the real one is 246 bytes.
const MANIFEST_LIMIT: u64 = 64 * 1024;

/// Larger than any `selection-history`: two file names and a line of numbers.
const HISTORY_LIMIT: u64 = 4 * 1024;

/// The largest config file read, as when the platform reads these files for `sqlite_home`.
const CONFIG_LIMIT: u64 = 1024 * 1024;

/// The config files in the home that can set a key, past the `<name>.config.toml` profiles.
const CONFIG_FILES: [&str; 3] = ["config.toml", "managed_config.toml", "requirements.toml"];

/// How a profile's file name ends.
const PROFILE_SUFFIX: &str = ".config.toml";

/// The key that moves the logs. Nothing else in a config file is looked at.
const LOG_DIR_KEY: &str = "log_dir";

/// The logs of one log directory that are reported.
const LOG_FILE: &str = "codex-tui.log";
const LOGIN_LOG: &str = "codex-login.log";

/// The catalog cache, below `cache/`.
const CATALOG_DIR: &str = "remote_plugin_catalog";

/// Entries directly in the home that are reported and never removed.
const HOME_ENTRIES: [&str; 4] = [
    "models_cache.json",
    "shell_snapshots",
    "thread-writer-locks",
    "tmp",
];

/// The two log databases, each with its `-wal` and `-shm` beside it.
const LOG_DATABASES: [&str; 2] = ["logs_1.sqlite", "logs_2.sqlite"];

/// The Codex integration.
#[derive(Debug)]
pub(crate) struct Codex;

impl Integration for Codex {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: IntegrationId("codex"),
            lens: Lens::Agents,
            platforms: Platform::ALL,
            roots: &[SymbolicRoot::CodexHome],
            selectors: SELECTORS,
            artifacts: &[],
            owner_files: &[],
            verified: VERIFIED,
            consequences: &[REDOWNLOADED, DIAGNOSTICS, REFETCHED],
            facts: FACTS,
            state: STATE,
            evidences: &[RELEASE_ACTIVITY, HOME_ACTIVITY],
            revision: 2,
        }
    }

    fn discover(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let homes: Vec<PathBuf> = cx
            .roots_of(SymbolicRoot::CodexHome)
            .map(|root| root.path.clone())
            .collect();
        let mut supported = !homes.is_empty();
        for path in homes {
            let home = Home::read(cx, path)?;
            home.add_findings(cx, sink)?;
            supported = supported && home.store.is_supported();
        }
        if supported {
            sink.set_owner_status(OwnerStatus::Supported);
            sink.advise(advice());
        }
        Ok(())
    }
}

/// The rule using `selector`, when it applies on this platform.
fn rule_of<'cx>(cx: &'cx ScanContext<'_>, selector: &'static str) -> Option<&'cx Rule> {
    cx.rules()
        .iter()
        .find(|rule| rule.selector == Selector::Integration(selector))
}

/// The whole of the file at `path`, `None` when it is larger than `limit`: a file read only
/// in part must never pass for one read whole.
fn whole(cx: &ScanContext<'_>, path: &Path, limit: u64) -> Result<Option<Vec<u8>>, ProbeError> {
    let bytes = cx.read_file(path, limit + 1)?;
    Ok((bytes.len() as u64 <= limit).then_some(bytes))
}

/// Whether `error` says only that nothing, or nothing of that kind, is there. Anything else
/// leaves what is there unknown.
fn is_absent(error: &ProbeError) -> bool {
    use std::io::ErrorKind;
    matches!(
        error.io_kind(),
        Some(ErrorKind::NotFound | ErrorKind::InvalidInput)
    )
}

/// What `<version>-<target>` or `local-<hash>-<target>` claims about a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Named<'name> {
    /// The version the name holds. A local build's name holds none.
    version: Option<&'name str>,
    /// The target it holds.
    target: &'static str,
}

impl<'name> Named<'name> {
    /// What `name` claims, `None` when it follows neither grammar.
    fn of(name: &'name str) -> Option<Self> {
        TARGETS.iter().find_map(|&target| {
            let before = name.strip_suffix(target)?.strip_suffix('-')?;
            let local = before.strip_prefix("local-");
            let valid = local.map_or_else(|| is_version(before), is_hash);
            valid.then_some(Self {
                version: local.is_none().then_some(before),
                target,
            })
        })
    }

    /// Whether `manifest` says what this name says.
    fn agrees_with(self, manifest: &Manifest) -> bool {
        let version = self.version.unwrap_or(&manifest.version);
        version == manifest.version
            && is_version(&manifest.version)
            && self.target == manifest.target
            && manifest.entrypoint == entrypoint(self.target)
    }
}

/// Where a release of `target` keeps its program.
fn entrypoint(target: &str) -> &'static str {
    if is_windows(target) {
        "bin/codex.exe"
    } else {
        "bin/codex"
    }
}

/// Whether `target` is one with no `codex` alias at the top of a release.
fn is_windows(target: &str) -> bool {
    target.ends_with("-pc-windows-msvc")
}

/// A digest in hexadecimal, as upstream names a local build.
fn is_hash(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Upstream's grammar: `<n>.<n>.<n>`, then `-alpha` with up to two more numbers, or `-beta`
/// with up to one, so `0.166.0-alpha.5.1` is real.
fn is_version(text: &str) -> bool {
    let (core, prerelease) = match text.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (text, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers.iter().all(|n| is_number(n))
        && prerelease.is_none_or(is_prerelease)
}

fn is_prerelease(text: &str) -> bool {
    let mut fields = text.split('.');
    let most = match fields.next() {
        Some("alpha") => 2,
        Some("beta") => 1,
        _ => return false,
    };
    let numbers: Vec<&str> = fields.collect();
    numbers.len() <= most && numbers.iter().all(|n| is_number(n))
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// What a release's manifest says. Upstream ignores `layoutVersion`, but only 1 was read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Manifest {
    version: String,
    target: String,
    entrypoint: String,
}

impl Manifest {
    /// `bytes` as a manifest of the one layout kipple knows, `None` otherwise. `execServer`
    /// is absent before 0.166.0, so nothing depends on it.
    fn parse(bytes: &[u8]) -> Option<Self> {
        let document: Json = serde_json::from_slice(bytes).ok()?;
        let manifest = document.as_object()?;
        let text = |key: &str| manifest.get(key)?.as_str().map(str::to_owned);
        (manifest.get("layoutVersion")?.as_u64()? == 1).then_some(())?;
        Some(Self {
            version: text("version")?,
            target: text("target")?,
            entrypoint: text("entrypoint")?,
        })
    }
}

/// What the three lines of a `selection-history` say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct History<'text> {
    /// The release selected when it was written.
    selected: &'text str,
    /// The store's rollback, when it has one.
    previous: Option<&'text str>,
    /// What the OS said of the `current` link when it was written.
    stamp: &'text str,
}

impl<'text> History<'text> {
    /// `bytes` as a history, `None` when it isn't exactly three lines, each ended, naming
    /// something a selection could be. A real `previous` holds dots, so only `.` and `..`
    /// themselves are refused.
    fn parse(bytes: &'text [u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?.strip_suffix('\n')?;
        let mut lines = text.split('\n');
        let (selected, previous, stamp) = (lines.next()?, lines.next()?, lines.next()?);
        let unusable = previous.contains('/') || matches!(previous, "." | "..");
        if lines.next().is_some() || selected.is_empty() || stamp.is_empty() || unusable {
            return None;
        }
        Some(Self {
            selected,
            previous: (!previous.is_empty()).then_some(previous),
            stamp,
        })
    }
}

/// A release that passed the signature.
#[derive(Debug)]
struct Release {
    name: String,
    path: PathBuf,
    version: String,
    target: String,
    files: Vec<PathBuf>,
}

/// One classified entry of a `releases/` directory.
#[derive(Debug)]
enum Entry {
    /// A `<version>-<target>` release.
    Published(Release),
    /// A `local-<hash>-<target>` release, which has no download to come back from.
    Local(Release),
    /// A release set aside for deletion, or what a crash left of one.
    Retired(Retired),
    /// Anything else: kept and reported, and it blocks nothing.
    Kept(PathBuf),
    /// One that couldn't be read, so it might be anything.
    Unread(PathBuf),
}

impl Entry {
    /// Whether something in it couldn't be read, so what it is isn't all known.
    const fn is_unread(&self) -> bool {
        match self {
            Self::Unread(_) => true,
            Self::Retired(retired) => !retired.whole,
            Self::Published(_) | Self::Local(_) | Self::Kept(_) => false,
        }
    }
}

/// A `.retired.*` entry.
#[derive(Debug)]
struct Retired {
    path: PathBuf,
    /// What its `release/` says it was, when that survives.
    manifest: Option<Manifest>,
    files: Vec<PathBuf>,
    /// Whether all of it could be listed.
    whole: bool,
}

/// What walking a release found.
#[derive(Debug, Default)]
struct Walked {
    files: Vec<PathBuf>,
    /// Whether it holds only files, directories and the `codex` alias, on one mount.
    ordinary: bool,
}

/// Lists everything below `top`, stopping at nothing a link leads to.
fn walk(cx: &ScanContext<'_>, top: &Path, mount: Option<u64>) -> Result<Walked, IntegrationError> {
    let mut walked = Walked {
        files: Vec::new(),
        ordinary: true,
    };
    let mut pending = vec![top.to_owned()];
    while let Some(dir) = pending.pop() {
        if cx.is_cancelled() {
            return Err(IntegrationError::Stopped);
        }
        let volume = cx.metadata(&dir)?.identity.map(|identity| identity.volume);
        walked.ordinary = walked.ordinary && volume.is_some() && volume == mount;
        for entry in cx.read_dir(&dir)? {
            let path = dir.join(&entry.name);
            match entry.kind {
                EntryKind::Dir => pending.push(path),
                EntryKind::File => walked.files.push(path),
                EntryKind::Symlink if path == top.join("codex") => {}
                EntryKind::Symlink | EntryKind::Other => walked.ordinary = false,
            }
        }
    }
    walked.files.sort();
    Ok(walked)
}

/// What `dir`'s manifest says, `None` when it has no usable one.
fn read_manifest(cx: &ScanContext<'_>, dir: &Path) -> Result<Option<Manifest>, ProbeError> {
    match whole(cx, &dir.join(MANIFEST), MANIFEST_LIMIT) {
        Ok(bytes) => Ok(bytes.as_deref().and_then(Manifest::parse)),
        Err(error) if is_absent(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Classifies the directory `releases/<name>`, by upstream's two checks and §3's signature:
/// a manifest that agrees with the name, the entrypoint a regular file, the alias reading
/// `bin/codex`, and nothing else but files and directories on the store's mount.
fn inspect(
    cx: &ScanContext<'_>,
    path: PathBuf,
    name: &str,
    mount: Option<u64>,
) -> Result<Entry, IntegrationError> {
    let Some(named) = Named::of(name) else {
        return Ok(Entry::Kept(path));
    };
    let manifest = match read_manifest(cx, &path) {
        Ok(Some(manifest)) if named.agrees_with(&manifest) => manifest,
        Ok(_) => return Ok(Entry::Kept(path)),
        Err(_) => return Ok(Entry::Unread(path)),
    };
    let walked = match walk(cx, &path, mount) {
        Ok(walked) => walked,
        Err(IntegrationError::Probe(_)) => return Ok(Entry::Unread(path)),
        Err(stopped) => return Err(stopped),
    };
    let program = path.join(&manifest.entrypoint);
    let aliased = match read_alias(cx, &path, named.target) {
        Ok(aliased) => aliased,
        Err(_) => return Ok(Entry::Unread(path)),
    };
    if !walked.ordinary || !aliased || !walked.files.contains(&program) {
        return Ok(Entry::Kept(path));
    }
    let release = Release {
        name: name.to_owned(),
        path,
        version: manifest.version,
        target: manifest.target,
        files: walked.files,
    };
    Ok(match named.version {
        Some(_) => Entry::Published(release),
        None => Entry::Local(release),
    })
}

/// Whether the release at `path` has the alias its target calls for.
fn read_alias(cx: &ScanContext<'_>, path: &Path, target: &str) -> Result<bool, ProbeError> {
    if is_windows(target) {
        return Ok(true);
    }
    match cx.read_link(&path.join("codex")) {
        Ok(alias) => Ok(alias == Path::new("bin/codex")),
        Err(error) if is_absent(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Reads a `.retired.*` directory. Upstream may be deleting it, so a part that can't be
/// read says only that it can't be shown unused.
fn read_retired(
    cx: &ScanContext<'_>,
    path: PathBuf,
    mount: Option<u64>,
) -> Result<Entry, IntegrationError> {
    let (files, listed) = match walk(cx, &path, mount) {
        Ok(walked) => (walked.files, true),
        Err(IntegrationError::Probe(_)) => (Vec::new(), false),
        Err(stopped) => return Err(stopped),
    };
    let manifest = read_manifest(cx, &path.join("release"));
    Ok(Entry::Retired(Retired {
        whole: listed && manifest.is_ok(),
        path,
        manifest: manifest.ok().flatten(),
        files,
    }))
}

/// `target` of a link in `component`, as the name of an entry of its `releases/`: upstream
/// writes an absolute path, and any link that lands there is as good. The directories below
/// the home were checked not to be links, so `..` in a relative target folds. One from a
/// root could pass through a link kipple can't see, so it has to be plain.
fn landed(component: &Path, target: &Path) -> Option<String> {
    let mut at = PathBuf::new();
    for part in component.join(target).components() {
        match part {
            Part::ParentDir if target.has_root() => return None,
            Part::ParentDir => {
                at.pop();
            }
            Part::CurDir => {}
            Part::Normal(_) | Part::RootDir | Part::Prefix(_) => at.push(part),
        }
    }
    let releases = component.join("releases");
    let name = at.file_name()?.to_str()?;
    (at.parent() == Some(releases.as_path())).then(|| name.to_owned())
}

/// One component's part of the store.
#[derive(Debug)]
struct Component {
    name: &'static str,
    dir: PathBuf,
    entries: Vec<Entry>,
    /// What the component holds that upstream doesn't write.
    strays: Vec<PathBuf>,
    /// The name `current` selects, when it selects a release that passed its signature.
    active: Option<String>,
    previous: Option<String>,
    pending: Option<String>,
    /// Why nothing in it can be relied on, if something says so.
    gap: Option<GroupGap>,
    /// Whether an installer left a marker.
    busy: bool,
    /// Whether its directory and `releases/` could be listed.
    listed: bool,
}

impl Component {
    /// Reads the component at `dir`. Only a cancel fails: what can't be read is a gap.
    fn read(
        cx: &ScanContext<'_>,
        name: &'static str,
        dir: PathBuf,
        mount: Option<u64>,
    ) -> Result<Self, IntegrationError> {
        let mut component = Self {
            name,
            dir,
            entries: Vec::new(),
            strays: Vec::new(),
            active: None,
            previous: None,
            pending: None,
            gap: None,
            busy: false,
            listed: true,
        };
        match cx.read_dir(&component.dir) {
            Ok(entries) => component.sort_markers(&entries),
            Err(_) => component.unlisted(),
        }
        component.read_releases(cx, mount)?;
        component.read_selection(cx);
        Ok(component)
    }

    /// Nothing can be said of what wasn't listed, and it may hold anything.
    const fn unlisted(&mut self) {
        self.listed = false;
        self.block(GroupGap::Unreadable);
    }

    /// Records why its groups can't be settled, keeping the first reason.
    const fn block(&mut self, gap: GroupGap) {
        if self.gap.is_none() {
            self.gap = Some(gap);
        }
    }

    /// Takes in the entries beside `releases/`. Any file an install writes on its way
    /// counts as an install under way, as a crash's leftover does until the next one.
    fn sort_markers(&mut self, entries: &[DirEntryMeta]) {
        for entry in entries {
            let Some(name) = entry.name.to_str() else {
                self.strays.push(self.dir.join(&entry.name));
                continue;
            };
            match name {
                "current"
                | "selection-history"
                | "auto-update-version"
                | "install.lock"
                | "releases" => {}
                _ if is_install_marker(name) => self.busy = true,
                _ => self.strays.push(self.dir.join(name)),
            }
        }
    }

    /// Classifies everything in `releases/`. A store whose `releases` is a link is refused
    /// by upstream's prune, and nothing below it is followed.
    fn read_releases(
        &mut self,
        cx: &ScanContext<'_>,
        mount: Option<u64>,
    ) -> Result<(), IntegrationError> {
        let releases = self.dir.join("releases");
        let listing = match cx.metadata(&releases) {
            Ok(meta) if meta.kind == EntryKind::Dir => cx.read_dir(&releases),
            Ok(_) => {
                self.block(GroupGap::Selection);
                self.strays.push(releases);
                return Ok(());
            }
            Err(error) if is_absent(&error) => {
                self.block(GroupGap::Selection);
                return Ok(());
            }
            Err(error) => Err(error),
        };
        let Ok(mut listing) = listing else {
            self.unlisted();
            self.entries.push(Entry::Unread(releases));
            return Ok(());
        };
        listing.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in listing {
            self.read_entry(cx, &releases, &entry, mount)?;
        }
        Ok(())
    }

    /// Classifies one entry of `releases`.
    fn read_entry(
        &mut self,
        cx: &ScanContext<'_>,
        releases: &Path,
        entry: &DirEntryMeta,
        mount: Option<u64>,
    ) -> Result<(), IntegrationError> {
        let path = releases.join(&entry.name);
        let name = entry.name.to_str().unwrap_or_default();
        if name.starts_with(".staging.") {
            self.busy = true;
            return Ok(());
        }
        let classified = match (entry.kind, name) {
            (EntryKind::Dir, name) if name.starts_with(".retired.") => {
                read_retired(cx, path, mount)?
            }
            (EntryKind::Dir, name) if !name.is_empty() => inspect(cx, path, name, mount)?,
            _ => Entry::Kept(path),
        };
        if classified.is_unread() {
            self.block(GroupGap::Unreadable);
        }
        self.entries.push(classified);
        Ok(())
    }

    /// Reads what selects a release, and what the history says of it.
    fn read_selection(&mut self, cx: &ScanContext<'_>) {
        match self.selected(cx) {
            Ok(name) => {
                match read_history(cx, &self.dir, &name) {
                    Ok(previous) => self.previous = previous,
                    Err(gap) => self.block(gap),
                }
                self.active = Some(name);
            }
            Err(gap) => self.block(gap),
        }
        let migration = cx.read_link(&self.dir.join(".migration-current"));
        self.pending = migration.ok().and_then(|to| landed(&self.dir, &to));
    }

    /// The name `current` selects, when that is a release of this component.
    fn selected(&self, cx: &ScanContext<'_>) -> Result<String, GroupGap> {
        let unread = |error: ProbeError| {
            if is_absent(&error) {
                GroupGap::Selection
            } else {
                GroupGap::Unreadable
            }
        };
        // What reading a link says of anything else differs by OS, so its kind is asked.
        let link = self.dir.join("current");
        if cx.metadata(&link).map_err(unread)?.kind != EntryKind::Symlink {
            return Err(GroupGap::Selection);
        }
        let target = cx.read_link(&link).map_err(unread)?;
        let name = landed(&self.dir, &target).ok_or(GroupGap::Selection)?;
        let signed = self.releases().any(|release| release.name == name);
        signed.then_some(name).ok_or(GroupGap::Selection)
    }

    /// Every release of it that passed the signature.
    fn releases(&self) -> impl Iterator<Item = &Release> {
        self.entries.iter().filter_map(|entry| match entry {
            Entry::Published(release) | Entry::Local(release) => Some(release),
            Entry::Retired(_) | Entry::Kept(_) | Entry::Unread(_) => None,
        })
    }

    /// The version of the release it selects.
    fn active_version(&self) -> Option<&str> {
        let active = self.active.as_deref()?;
        self.releases()
            .find(|release| release.name == active)
            .map(|release| release.version.as_str())
    }

    /// Whether it is what upstream leaves: only what upstream writes, and a selection. What
    /// couldn't be listed or read may be anything, so it can't be called that.
    fn is_well_formed(&self) -> bool {
        self.listed
            && self.strays.is_empty()
            && self.active.is_some()
            && !self.entries.iter().any(Entry::is_unread)
    }

    /// What `name` is to this component.
    fn role(&self, name: &str) -> ReleaseRole {
        if self.active.as_deref() == Some(name) {
            ReleaseRole::Active
        } else if self.previous.as_deref() == Some(name) {
            ReleaseRole::Previous
        } else if self.pending.as_deref() == Some(name) {
            ReleaseRole::PendingInstall
        } else {
            ReleaseRole::Installed
        }
    }

    /// Whether what it records can be relied on. Everything here belongs to the component,
    /// so it answers for each of its target groups alike.
    fn state(&self) -> ReleaseState {
        let gap = self
            .gap
            .or_else(|| self.busy.then_some(GroupGap::InstallerBusy));
        gap.map_or(ReleaseState::Sound, ReleaseState::Blocked)
    }

    /// The locks upstream takes before it changes this component.
    fn locks(&self, home: &Path) -> Vec<PathBuf> {
        let install = self.dir.join("install.lock");
        if self.name == DAEMON {
            vec![install, home.join("app-server-daemon/daemon.lock")]
        } else {
            vec![install]
        }
    }

    /// The facts of its store that only its own releases can be asked about.
    fn release_values(&self) -> [(&'static str, FactValue); 2] {
        let complete = self.listed && self.gap != Some(GroupGap::Unreadable);
        let installer = match (self.busy, self.listed) {
            (true, _) => FactValue::Enum("busy"),
            (false, true) => FactValue::Enum("idle"),
            (false, false) => FactValue::Unknown,
        };
        let inventory = if complete {
            FactValue::Enum("complete")
        } else {
            FactValue::Unknown
        };
        [(INVENTORY, inventory), (INSTALLER_ACTIVITY, installer)]
    }
}

/// Whether `name`, beside `releases/`, is something only an install in progress leaves.
/// `.tmp*` is the daemon's temporary `selection-history`, and `*.tmp.<pid>` the installer's.
fn is_install_marker(name: &str) -> bool {
    matches!(name, ".migration-current" | "install.lock.d")
        || [
            ".current.",
            "install-lock-",
            ".tmp",
            "selection-history.tmp.",
            "auto-update-version.tmp.",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// The rollback `<dir>/selection-history` names for the selection `name`, when the history
/// is valid: exactly as upstream checks it, or nothing of the group can be settled.
fn read_history(cx: &ScanContext<'_>, dir: &Path, name: &str) -> Result<Option<String>, GroupGap> {
    let bytes = whole(cx, &dir.join("selection-history"), HISTORY_LIMIT).map_err(|error| {
        if error.io_kind() == Some(std::io::ErrorKind::NotFound) {
            GroupGap::HistoryMissing
        } else {
            GroupGap::Unreadable
        }
    })?;
    let history = bytes.as_deref().and_then(History::parse);
    let history = history.ok_or(GroupGap::HistoryMalformed)?;
    let stamp = cx.link_stamp(&dir.join("current"));
    let stamp = stamp.map_err(|_| GroupGap::Unreadable)?;
    if history.selected != name || history.stamp != stamp {
        return Err(GroupGap::HistoryStale);
    }
    Ok(history.previous.map(str::to_owned))
}

/// The release store of one home.
#[derive(Debug, Default)]
struct Store {
    components: Vec<Component>,
    /// What `packages/` holds that isn't a component, or `packages` itself when it can't be
    /// used. Either makes the whole store unknown.
    strays: Vec<PathBuf>,
}

impl Store {
    /// Reads `<home>/packages`. None there is no store: Codex may run from elsewhere.
    fn read(cx: &ScanContext<'_>, home: &Path) -> Result<Self, IntegrationError> {
        let packages = home.join("packages");
        let mut store = Self::default();
        let meta = match cx.metadata(&packages) {
            Ok(meta) => meta,
            Err(error) if is_absent(&error) => return Ok(store),
            Err(_) => {
                store.strays.push(packages);
                return Ok(store);
            }
        };
        let listing = (meta.kind == EntryKind::Dir).then(|| cx.read_dir(&packages));
        let Some(Ok(mut entries)) = listing else {
            store.strays.push(packages);
            return Ok(store);
        };
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let mount = meta.identity.map(|identity| identity.volume);
        for entry in entries {
            let path = packages.join(&entry.name);
            let known = COMPONENTS.iter().find(|name| entry.name == **name);
            match known {
                Some(name) if entry.kind == EntryKind::Dir => {
                    store
                        .components
                        .push(Component::read(cx, name, path, mount)?);
                }
                Some(_) | None => store.strays.push(path),
            }
        }
        Ok(store)
    }

    /// Whether every component selects a version with a fixture.
    fn version_supported(&self) -> FactValue {
        let versions: Vec<Option<&str>> = self
            .components
            .iter()
            .map(Component::active_version)
            .collect();
        if versions
            .iter()
            .flatten()
            .any(|version| !is_verified(version))
        {
            FactValue::Enum("unsupported")
        } else if versions.is_empty() || versions.contains(&None) {
            FactValue::Unknown
        } else {
            FactValue::Enum("supported")
        }
    }

    /// Whether what this store holds is what kipple was verified against.
    fn is_supported(&self) -> bool {
        self.strays.is_empty()
            && self.components.iter().all(Component::is_well_formed)
            && self.version_supported() == FactValue::Enum("supported")
    }

    /// Whether its releases may be told apart at all: its shape is known, and nothing
    /// selected is a version nobody read. A release of a component with a bad selection
    /// stays one, blocked, so that retention sees the whole group.
    fn is_readable(&self) -> bool {
        self.strays.is_empty()
            && self
                .components
                .iter()
                .all(|c| c.listed && c.strays.is_empty())
            && self.version_supported() != FactValue::Enum("unsupported")
    }

    /// Whether anything may still append to `<home>/log/codex-tui.log` with `log_dir` unset.
    fn tui_log_writer(&self) -> FactValue {
        let floor = ReleaseVersion(TUI_LOG_SILENT_FROM.to_owned());
        let silent = |version: &str| ReleaseVersion(version.to_owned()) >= floor;
        let versions: Vec<Option<&str>> = self
            .components
            .iter()
            .map(Component::active_version)
            .collect();
        if versions.iter().flatten().any(|version| !silent(version)) {
            FactValue::Enum("possible")
        } else if versions.is_empty() || versions.contains(&None) {
            FactValue::Unknown
        } else {
            FactValue::Enum("none")
        }
    }
}

/// Whether `version` equals a row of [`VERIFIED`].
fn is_verified(version: &str) -> bool {
    VERIFIED
        .iter()
        .any(|row| row.strip_prefix(VERIFIED_PREFIX) == Some(version))
}

/// What the config files in a home say of `log_dir`.
#[derive(Debug, PartialEq, Eq)]
enum LogDir {
    /// Each file there was read, and none sets it.
    Unset,
    /// Set, to these directories as far as they can be placed: a `~` can't be, since the
    /// home directory isn't one of this integration's roots.
    Set(Vec<PathBuf>),
    /// A file couldn't be read or parsed, or a value isn't a path.
    Unknown,
}

impl LogDir {
    /// Reads every config file among `entries` of `home`. A link to one counts as unread,
    /// since kipple doesn't follow it. What the system's own layers set is out of reach.
    fn read(cx: &ScanContext<'_>, home: &Path, entries: &[DirEntryMeta]) -> Self {
        let mut dirs = None;
        let configs = entries.iter().filter(|entry| is_config(entry));
        for entry in configs {
            let Some(values) = read_config(cx, &home.join(&entry.name)) else {
                return Self::Unknown;
            };
            if !values.is_empty() {
                let placed = values.iter().filter_map(|value| placed(home, value));
                dirs.get_or_insert_with(Vec::new).extend(placed);
            }
        }
        dirs.map_or(Self::Unset, |mut dirs: Vec<PathBuf>| {
            dirs.sort();
            dirs.dedup();
            Self::Set(dirs)
        })
    }

    const fn configured(&self) -> FactValue {
        match self {
            Self::Unset => FactValue::Bool(false),
            Self::Set(_) => FactValue::Bool(true),
            Self::Unknown => FactValue::Unknown,
        }
    }
}

/// Whether `entry` of a home is a file Codex may read its config from. These are the one
/// kind of name a person writes, and on a file system that ignores case Codex opens
/// `CONFIG.TOML` as `config.toml`, so case doesn't count. Reading one too many only holds more.
fn is_config(entry: &DirEntryMeta) -> bool {
    entry.name.to_str().is_some_and(|name| {
        let name = name.to_ascii_lowercase();
        CONFIG_FILES.contains(&name.as_str()) || name.ends_with(PROFILE_SUFFIX)
    })
}

/// Where a `log_dir` of a file in `home` leads. Codex reads a relative one from the file's
/// directory.
fn placed(home: &Path, value: &str) -> Option<PathBuf> {
    (!value.is_empty() && !value.starts_with('~')).then(|| home.join(value))
}

/// Every value the TOML file at `path` gives `log_dir`, at any depth since a profile's table
/// counts too. `None` when it can't be read or parsed, or a value isn't a string.
fn read_config(cx: &ScanContext<'_>, path: &Path) -> Option<Vec<String>> {
    let bytes = whole(cx, path, CONFIG_LIMIT).ok()??;
    let text = std::str::from_utf8(&bytes).ok()?;
    let document = DeTable::parse(text).ok()?;
    let mut found = Vec::new();
    in_table(document.get_ref(), &mut found);
    found.into_iter().collect()
}

/// The parser bounds how deep a document nests, so this recursion is bounded too.
fn in_table(table: &DeTable<'_>, found: &mut Vec<Option<String>>) {
    for (name, value) in table {
        if name.get_ref() == LOG_DIR_KEY {
            found.push(value.get_ref().as_str().map(str::to_owned));
        }
        in_value(value.get_ref(), found);
    }
}

fn in_value(value: &DeValue<'_>, found: &mut Vec<Option<String>>) {
    match value {
        DeValue::Table(table) => in_table(table, found),
        DeValue::Array(items) => {
            for item in items {
                in_value(item.get_ref(), found);
            }
        }
        DeValue::String(_)
        | DeValue::Integer(_)
        | DeValue::Float(_)
        | DeValue::Boolean(_)
        | DeValue::Datetime(_) => {}
    }
}

/// Whether `name` is one of the catalog's own, which are all a rule may remove.
fn is_catalog_file(name: &str) -> bool {
    name.strip_suffix(".json").is_some_and(|stem| {
        stem.len() == 16 && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Whether `name`, directly in the home, is reported.
fn is_home_entry(name: &str) -> bool {
    HOME_ENTRIES.contains(&name) || LOG_DATABASES.iter().any(|db| name.starts_with(db))
}

/// A home, as one scan read it.
#[derive(Debug)]
struct Home {
    path: PathBuf,
    entries: Vec<DirEntryMeta>,
    store: Store,
    log_dir: LogDir,
}

impl Home {
    /// Reads what its findings rest on.
    fn read(cx: &ScanContext<'_>, path: PathBuf) -> Result<Self, IntegrationError> {
        let entries = cx.read_dir(&path)?;
        let store = Store::read(cx, &path)?;
        let log_dir = LogDir::read(cx, &path, &entries);
        Ok(Self {
            path,
            entries,
            store,
            log_dir,
        })
    }

    /// What every finding of this home supplies.
    fn values(&self) -> BTreeMap<&'static str, FactValue> {
        let layout = if self.store.is_supported() {
            FactValue::Enum("supported")
        } else {
            FactValue::Unknown
        };
        BTreeMap::from([
            (LAYOUT, layout),
            (VERSION_SUPPORTED, self.store.version_supported()),
        ])
    }

    /// What a finding's layout rests on: nothing, when the home isn't verified.
    fn evidence(&self, signature: &str) -> Evidence {
        if self.store.is_supported() {
            Evidence::Signature(signature.to_owned())
        } else {
            Evidence::NoSignature
        }
    }

    /// `consequence`, when the home is verified: what a finding costs isn't known otherwise.
    fn cost(&self, consequence: Consequence) -> Option<Consequence> {
        self.store.is_supported().then_some(consequence)
    }

    /// A plain finding of this home, judged by `activity`.
    fn plain(&self, activity: ActivityEvidence) -> Supplied {
        Supplied {
            values: self.values(),
            activity,
            ..Supplied::default()
        }
    }

    fn add_findings(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        self.add_store(cx, sink)?;
        self.add_home_entries(cx, sink)?;
        self.add_logs(cx, sink)?;
        self.add_caches(cx, sink)?;
        self.add_daemon_logs(cx, sink)
    }

    /// Reports `path` under `selector`'s rule. The rule is off this platform when it has
    /// none, and then it isn't reported at all.
    fn report(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
        selector: &'static str,
        found: Reported<'_>,
    ) -> Result<(), IntegrationError> {
        let Some(rule) = rule_of(cx, selector) else {
            return Ok(());
        };
        let activity = if selector == RELEASE_ENTRIES {
            RELEASE_ACTIVITY
        } else {
            HOME_ACTIVITY
        };
        let claim = Claim {
            evidence: vec![found.evidence],
            consequence: None,
        };
        let supplied = Supplied {
            subject: found.subject,
            ..self.plain(activity)
        };
        sink.add_supplied(rule, found.path, claim, supplied)
    }

    /// What the store holds, each entry through the one rule it belongs to.
    fn add_store(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        for path in &self.store.strays {
            let found = Reported::unknown(path);
            self.report(cx, sink, RELEASE_ENTRIES, found)?;
        }
        for component in &self.store.components {
            for path in &component.strays {
                let found = Reported::unknown(path);
                self.report(cx, sink, RELEASE_ENTRIES, found)?;
            }
            for entry in &component.entries {
                self.add_release_entry(cx, sink, component, entry)?;
            }
        }
        Ok(())
    }

    fn add_release_entry(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
        component: &Component,
        entry: &Entry,
    ) -> Result<(), IntegrationError> {
        match entry {
            Entry::Published(release) => self.add_published(cx, sink, component, release),
            Entry::Local(release) => {
                let found = Reported {
                    path: &release.path,
                    evidence: self.evidence(MANIFEST),
                    subject: Subject::Plain,
                };
                self.report(cx, sink, RELEASE_ENTRIES, found)
            }
            Entry::Retired(retired) => {
                let found = Reported {
                    path: &retired.path,
                    evidence: Evidence::NoSignature,
                    subject: Subject::Release(self.retired_facts(component, retired)),
                };
                self.report(cx, sink, RELEASE_ENTRIES, found)
            }
            Entry::Kept(path) | Entry::Unread(path) => {
                self.report(cx, sink, RELEASE_ENTRIES, Reported::unknown(path))
            }
        }
    }

    /// A published release. Where the removing rule isn't, on another OS, every release but
    /// the selected one is reported, and in a store that isn't verified all of them are.
    fn add_published(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
        component: &Component,
        release: &Release,
    ) -> Result<(), IntegrationError> {
        if let Some(rule) = rule_of(cx, RELEASES).filter(|_| self.store.is_readable()) {
            let claim = Claim {
                evidence: vec![Evidence::Signature(MANIFEST.to_owned())],
                consequence: Some(REDOWNLOADED),
            };
            let mut values = self.values();
            values.extend(component.release_values());
            let supplied = Supplied {
                values,
                subject: Subject::Release(self.release_facts(component, release)),
                activity: RELEASE_ACTIVITY,
                advice: None,
            };
            return sink.add_supplied(rule, &release.path, claim, supplied);
        }
        let selected = component.role(&release.name) == ReleaseRole::Active;
        if selected && self.store.is_supported() {
            return Ok(());
        }
        let found = Reported {
            path: &release.path,
            evidence: self.evidence(MANIFEST),
            subject: Subject::Plain,
        };
        self.report(cx, sink, RELEASE_ENTRIES, found)
    }

    fn release_facts(&self, component: &Component, release: &Release) -> ReleaseFacts {
        ReleaseFacts {
            group: ReleaseGroup {
                component: component.name.to_owned(),
                target: release.target.clone(),
            },
            version: ReleaseVersion(release.version.clone()),
            role: component.role(&release.name),
            files: release.files.clone(),
            locks: component.locks(&self.path),
            state: component.state(),
        }
    }

    /// What a `.retired.*` entry can still say. What a crash left may name no version or
    /// target, and a process under it blocks its component whichever group it was in.
    fn retired_facts(&self, component: &Component, retired: &Retired) -> ReleaseFacts {
        let (version, target) = retired
            .manifest
            .as_ref()
            .map_or_else(Default::default, |read| {
                (read.version.clone(), read.target.clone())
            });
        ReleaseFacts {
            group: ReleaseGroup {
                component: component.name.to_owned(),
                target,
            },
            version: ReleaseVersion(version),
            role: ReleaseRole::Retired,
            files: retired.files.clone(),
            locks: component.locks(&self.path),
            state: component.state(),
        }
    }

    /// The entries directly in the home that are reported.
    fn add_home_entries(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        for entry in &self.entries {
            let Some(name) = entry.name.to_str().filter(|name| is_home_entry(name)) else {
                continue;
            };
            let path = self.path.join(name);
            let found = Reported::known(&path, self.evidence(name));
            self.report(cx, sink, STORE_ENTRIES, found)?;
        }
        Ok(())
    }

    /// The directory `name` of the home as it was listed, or `None` when it isn't there, or
    /// isn't a directory or can't be listed, either of which is reported as unknown.
    fn listed(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
        name: &str,
    ) -> Result<Option<Vec<DirEntryMeta>>, IntegrationError> {
        let Some(entry) = self.entries.iter().find(|entry| entry.name == name) else {
            return Ok(None);
        };
        let dir = self.path.join(name);
        let listing = (entry.kind == EntryKind::Dir).then(|| cx.read_dir(&dir));
        if let Some(Ok(entries)) = listing {
            return Ok(Some(entries));
        }
        self.report(cx, sink, STORE_ENTRIES, Reported::unknown(&dir))?;
        Ok(None)
    }

    /// The default log directory, then any other one the config names.
    fn add_logs(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let default = self.path.join("log");
        if let Some(entries) = self.listed(cx, sink, "log")? {
            for entry in &entries {
                self.add_default_log(cx, sink, &default, entry)?;
            }
        }
        let LogDir::Set(dirs) = &self.log_dir else {
            return Ok(());
        };
        for dir in dirs.iter().filter(|dir| **dir != default) {
            self.add_configured_logs(cx, sink, dir)?;
        }
        Ok(())
    }

    /// One entry of `<home>/log`: the legacy log when it is a regular file and its rule
    /// applies here, and otherwise only reported.
    fn add_default_log(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
        dir: &Path,
        entry: &DirEntryMeta,
    ) -> Result<(), IntegrationError> {
        let path = dir.join(&entry.name);
        let regular = entry.kind == EntryKind::File;
        if let Some(rule) = rule_of(cx, TUI_LOG).filter(|_| regular && entry.name == LOG_FILE) {
            let claim = Claim {
                evidence: vec![self.evidence(LOG_FILE)],
                consequence: self.cost(DIAGNOSTICS),
            };
            let mut supplied = self.plain(HOME_ACTIVITY);
            supplied
                .values
                .insert(LOG_DIR_CONFIGURED, self.log_dir.configured());
            supplied
                .values
                .insert(TUI_LOG_WRITER, self.store.tui_log_writer());
            return sink.add_supplied(rule, &path, claim, supplied);
        }
        if entry.name != LOG_FILE && entry.name != LOGIN_LOG {
            return Ok(());
        }
        let found = if regular {
            Reported::known(&path, self.evidence(&entry.name.to_string_lossy()))
        } else {
            Reported::unknown(&path)
        };
        self.report(cx, sink, STORE_ENTRIES, found)
    }

    /// The logs of a configured directory. Inside the home they are reported like any
    /// other. Outside it, or behind a link, nothing can be read: the directory is reported
    /// as it is written and nothing there is ever mutable.
    fn add_configured_logs(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
        dir: &Path,
    ) -> Result<(), IntegrationError> {
        let Ok(entries) = cx.read_dir(dir) else {
            let Some(rule) = rule_of(cx, STORE_ENTRIES) else {
                return Ok(());
            };
            let evidence = vec![Evidence::NoSignature];
            return sink.add_unread(rule, dir, evidence, self.plain(HOME_ACTIVITY));
        };
        let logs = entries
            .iter()
            .filter(|entry| entry.name == LOG_FILE || entry.name == LOGIN_LOG);
        for entry in logs {
            let path = dir.join(&entry.name);
            let evidence = self.evidence(&entry.name.to_string_lossy());
            self.report(cx, sink, STORE_ENTRIES, Reported::known(&path, evidence))?;
        }
        Ok(())
    }

    /// What `cache/` holds: the catalog's files through their own rule where it applies,
    /// and every other entry reported.
    fn add_caches(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let Some(entries) = self.listed(cx, sink, "cache")? else {
            return Ok(());
        };
        let dir = self.path.join("cache");
        for entry in &entries {
            let path = dir.join(&entry.name);
            let is_catalog = entry.name == CATALOG_DIR && entry.kind == EntryKind::Dir;
            if let Some(rule) = rule_of(cx, CATALOG).filter(|_| is_catalog) {
                self.add_catalog(cx, sink, rule, &path)?;
            } else {
                let evidence = self.evidence(&entry.name.to_string_lossy());
                self.report(cx, sink, STORE_ENTRIES, Reported::known(&path, evidence))?;
            }
        }
        Ok(())
    }

    /// One finding per file of the catalog. Any other entry there is only reported.
    fn add_catalog(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
        rule: &Rule,
        dir: &Path,
    ) -> Result<(), IntegrationError> {
        let Ok(entries) = cx.read_dir(dir) else {
            return self.report(cx, sink, STORE_ENTRIES, Reported::unknown(dir));
        };
        for entry in entries {
            let path = dir.join(&entry.name);
            let name = entry.name.to_str().filter(|name| is_catalog_file(name));
            if entry.kind == EntryKind::File && name.is_some() {
                let claim = Claim {
                    evidence: vec![self.evidence(CATALOG_DIR)],
                    consequence: self.cost(REFETCHED),
                };
                sink.add_supplied(rule, &path, claim, self.plain(HOME_ACTIVITY))?;
            } else {
                self.report(cx, sink, STORE_ENTRIES, Reported::unknown(&path))?;
            }
        }
        Ok(())
    }

    /// The rotated logs of the daemon. The live ones say which store a daemon runs from, so
    /// they stay protected and aren't reported.
    fn add_daemon_logs(
        &self,
        cx: &ScanContext<'_>,
        sink: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        let Some(entries) = self.listed(cx, sink, "app-server-daemon")? else {
            return Ok(());
        };
        let dir = self.path.join("app-server-daemon");
        for entry in &entries {
            let name = entry.name.to_string_lossy();
            if entry.kind == EntryKind::File && name.ends_with(".stderr.log.previous") {
                let path = dir.join(&entry.name);
                let found = Reported::known(&path, self.evidence(&name));
                self.report(cx, sink, STORE_ENTRIES, found)?;
            }
        }
        Ok(())
    }
}

/// One finding of a report rule.
#[derive(Debug)]
struct Reported<'path> {
    path: &'path Path,
    evidence: Evidence,
    subject: Subject,
}

impl<'path> Reported<'path> {
    /// Something kipple can't vouch for.
    const fn unknown(path: &'path Path) -> Self {
        Self {
            path,
            evidence: Evidence::NoSignature,
            subject: Subject::Plain,
        }
    }

    /// Something that is what its name says, as far as `evidence` goes.
    const fn known(path: &'path Path, evidence: Evidence) -> Self {
        Self {
            path,
            evidence,
            subject: Subject::Plain,
        }
    }
}

fn advice() -> Advice {
    Advice {
        title: "Cap the size of Codex's history".to_owned(),
        text: "history.max_bytes in config.toml caps history.jsonl, which otherwise grows \
               without limit: on its next write past the cap, Codex drops the oldest entries \
               until the file is at 80% of it, always keeping the newest. Don't set it to 0, \
               which turns trimming off rather than keeping nothing. It doesn't cover \
               sessions/ or archived_sessions/: Codex compresses a rollout after 7 days, and \
               nothing deletes one."
            .to_owned(),
        snippets: vec!["[history]\nmax_bytes = 104857600".to_owned()],
        references: vec!["https://learn.chatgpt.com/docs/config-file/config-reference".to_owned()],
    }
}
