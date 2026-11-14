//! npm the tool, apart from Node's project artifacts (`docs/06-adapters-and-rules.md` §5):
//! the download cache `_cacache` in its cache root, which only `npm cache clean --force`
//! empties, through the reviewed native path (spec M4 pinned 33). Verified against npm
//! 11.20.0 and 12.2.1 (S5 npm). Everything else in the root, `_npx` and `_logs` included,
//! is reported and stays closed.
//!
//! The cache's layout is told by cacache's own directories, `index-v5/` and `content-v2/`;
//! `CACHEDIR.TAG` proves nothing, since only cacache 21 writes it. The facts that decide
//! whether the command may run, the tool among them, are read again right before it does.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;

use kipple_core::{
    ActivityEvidence, Area, Claim, Consequence, Cost, DirEntryMeta, EntryKind, Evidence, FactKind,
    FactSpec, FactValue, Finding, FindingSink, Integration, IntegrationDescriptor,
    IntegrationError, IntegrationId, IntegrationSelector, Lens, Loss, NativeKind, NativeTool,
    OpenArea, OwnerStatus, Platform, Program, ProtectedEntry, Recovery, ResolvedTool, Rule,
    ScanContext, Selector, SemanticFacts, StateDeclaration, Subject, Supplied, SymbolicRoot,
    ToolGap, Unresolved,
};

const ID: IntegrationId = IntegrationId("npm");

const DOWNLOAD_CACHE: &str = "npm-download-cache";
const CACHE_ENTRIES: &str = "npm-cache-entries";

const SELECTORS: &[IntegrationSelector] = &[
    IntegrationSelector {
        name: DOWNLOAD_CACHE,
        meaning: "the `_cacache` directory of a verified cache root",
    },
    IntegrationSelector {
        name: CACHE_ENTRIES,
        meaning: "entries of the cache root that are reported and never removed",
    },
];

/// The npm versions verified, each with its layout fixture and contract test. The
/// platform's resolver runs only these (`kipple-platform/src/tools.rs`).
const VERIFIED: &[&str] = &["npm 11.20.0", "npm 12.2.1"];

/// The download cache's name in the cache root.
const CACACHE: &str = "_cacache";

/// What cacache keeps in `_cacache`, which no other layout has both of.
const INDEX: &str = "index-v5";
const CONTENT: &str = "content-v2";

/// The entries of the cache root the inventory reports.
const REPORTED: &[&str] = &["_npx", "_logs"];

const LAYOUT: &str = "layout";
const EXECUTABLE_VERIFIED: &str = "npm.executable_verified";
const VERSION_SUPPORTED: &str = "npm.version_supported";
const CACHE_IS_LINK: &str = "cache.is_link";

const SUPPORTED: FactValue = FactValue::Enum("supported");
const UNSUPPORTED: FactValue = FactValue::Enum("unsupported");
const UNKNOWN_LAYOUT: FactValue = FactValue::Enum("unknown");

const FACTS: &[FactSpec] = &[
    FactSpec {
        name: LAYOUT,
        kind: FactKind::Enum(&["supported", "unknown"]),
        selector: None,
        meaning: "whether `_cacache` is a directory holding cacache's `index-v5/` and \
                  `content-v2/`",
    },
    FactSpec {
        name: EXECUTABLE_VERIFIED,
        kind: FactKind::Bool,
        selector: Some(DOWNLOAD_CACHE),
        meaning: "whether the first npm on `PATH` of a verified version is its own \
                  `npm-cli.js` and the node beside it, neither of them a script",
    },
    FactSpec {
        name: VERSION_SUPPORTED,
        kind: FactKind::Enum(&["supported", "unsupported"]),
        selector: Some(DOWNLOAD_CACHE),
        meaning: "whether that npm is a verified version and its node is one it supports",
    },
    FactSpec {
        name: CACHE_IS_LINK,
        kind: FactKind::Bool,
        selector: Some(DOWNLOAD_CACHE),
        meaning: "whether `_cacache` itself is a link",
    },
];

/// npm downloads what it needs again, and installs need the network until it does.
const DOWNLOADS: Consequence = Consequence {
    recovery: Recovery::Redownload,
    loss: &[Loss::CacheWarmth, Loss::OfflineAbility],
    cost: Cost::Unknown,
};

/// What the cache root holds besides `_cacache`, which stays closed: `npx`'s installed
/// packages, the debug logs, and what npm keeps for itself.
const PROTECTED: &[ProtectedEntry] = &[
    ProtectedEntry::Exact("_npx"),
    ProtectedEntry::Exact("_logs"),
    ProtectedEntry::Exact("_prebuilds"),
    ProtectedEntry::Exact("_tuf"),
    ProtectedEntry::Exact("_update-notifier-last-checked"),
];

/// The cache root closes to everyone but npm's own `_cacache`. A root that can't be placed
/// holds npm's findings, and the default cache stays closed when configuration moves it.
const STATE: &[StateDeclaration] = &[
    StateDeclaration {
        root: SymbolicRoot::NpmCache,
        protected: PROTECTED,
        open: &[OpenArea {
            at: Area::Exact(CACACHE),
            integrations: &[ID],
        }],
        delegates: &[],
        unresolved: Unresolved::HoldOwn,
    },
    StateDeclaration {
        root: SymbolicRoot::NpmDefaultCache,
        protected: &[],
        open: &[],
        delegates: &[SymbolicRoot::NpmCache],
        unresolved: Unresolved::HoldOwn,
    },
];

/// The npm integration.
#[derive(Debug)]
pub(crate) struct Npm;

impl Integration for Npm {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: ID,
            lens: Lens::Dev,
            platforms: Platform::ALL,
            roots: &[],
            selectors: SELECTORS,
            artifacts: &[],
            owner_files: &[],
            verified: VERIFIED,
            consequences: &[DOWNLOADS],
            facts: FACTS,
            state: STATE,
            evidences: &[ActivityEvidence::ToolRunning {
                programs: &[Program::Prefix("npm"), Program::Prefix("npx")],
                scripts: &["npm-cli.js", "npx-cli.js"],
                runtimes: &["node"],
                stores: &[],
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
        for root in cx.roots_of(SymbolicRoot::NpmCache) {
            let entries = cx.read_dir(&root.path)?;
            let cache = read_cache(cx, &root.path, &entries);
            supported |= cache.layout == SUPPORTED;
            add_cache(cx, sink, &root.path, &cache)?;
            add_inventory(cx, sink, &root.path, &entries, cache.layout)?;
        }
        if supported {
            sink.set_owner_status(OwnerStatus::Supported);
        }
        Ok(())
    }

    /// The download cache's facts and its tool as they are now: an npm update or a swapped
    /// `_cacache` between the scan and the run must hold it. Anything else stays as the
    /// scan read it.
    fn revisit(
        &self,
        cx: &ScanContext<'_>,
        finding: &Finding,
    ) -> Result<Option<Supplied>, IntegrationError> {
        let download = rule_of(cx, DOWNLOAD_CACHE).is_some_and(|rule| rule.id == finding.rule);
        let (true, Some(facts)) = (download, finding.facts.as_ref()) else {
            return Ok(None);
        };
        let cache = finding.path.parent().map_or(Cache::UNREAD, |root| {
            let entries = cx.read_dir(root);
            entries.map_or(Cache::UNREAD, |entries| read_cache(cx, root, &entries))
        });
        let tool = cx.tool(NativeTool::Npm);
        Ok(Some(Supplied {
            values: cache_values(&cache, &tool),
            subject: semantic(tool),
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

/// What `_cacache` in a cache root is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cache {
    /// What the root's listing says it is, `None` when it isn't there.
    kind: Option<EntryKind>,
    /// The `layout` fact.
    layout: FactValue,
}

impl Cache {
    /// A cache whose root couldn't be listed.
    const UNREAD: Self = Self {
        kind: Some(EntryKind::Other),
        layout: FactValue::Unknown,
    };
}

/// `_cacache` as the listing of its root shows it, with its layout. Only a real directory
/// holding both of cacache's directories is a supported layout; a link is never followed.
fn read_cache(cx: &ScanContext<'_>, root: &Path, entries: &[DirEntryMeta]) -> Cache {
    let kind = entries
        .iter()
        .find(|entry| entry.name == OsStr::new(CACACHE))
        .map(|entry| entry.kind);
    let layout = match kind {
        Some(EntryKind::Dir) => match cx.read_dir(&root.join(CACACHE)) {
            Ok(inside) if holds_dir(&inside, INDEX) && holds_dir(&inside, CONTENT) => SUPPORTED,
            Ok(_) => UNKNOWN_LAYOUT,
            Err(_) => FactValue::Unknown,
        },
        Some(EntryKind::Symlink | EntryKind::File | EntryKind::Other) | None => UNKNOWN_LAYOUT,
    };
    Cache { kind, layout }
}

fn holds_dir(entries: &[DirEntryMeta], name: &str) -> bool {
    entries
        .iter()
        .any(|entry| entry.name == OsStr::new(name) && entry.kind == EntryKind::Dir)
}

/// What the download cache supplies with `tool` resolved as it is.
fn cache_values(
    cache: &Cache,
    tool: &Result<ResolvedTool, ToolGap>,
) -> BTreeMap<&'static str, FactValue> {
    let (executable, version) = tool_values(tool);
    let link = match cache.kind {
        Some(EntryKind::Symlink) => FactValue::Bool(true),
        Some(EntryKind::Dir | EntryKind::File) => FactValue::Bool(false),
        Some(EntryKind::Other) | None => FactValue::Unknown,
    };
    [
        (LAYOUT, cache.layout),
        (EXECUTABLE_VERIFIED, executable),
        (VERSION_SUPPORTED, version),
        (CACHE_IS_LINK, link),
    ]
    .into()
}

/// The `npm.executable_verified` and `npm.version_supported` facts of `tool`. A gap that
/// says nothing of one leaves it unknown.
const fn tool_values(tool: &Result<ResolvedTool, ToolGap>) -> (FactValue, FactValue) {
    match tool {
        Ok(_) => (FactValue::Bool(true), SUPPORTED),
        Err(ToolGap::UnverifiedVersion | ToolGap::UnsupportedRuntime) => {
            (FactValue::Bool(true), UNSUPPORTED)
        }
        Err(ToolGap::UnknownVersion) => (FactValue::Bool(true), FactValue::Unknown),
        Err(ToolGap::Missing | ToolGap::Script | ToolGap::UnknownLayout | ToolGap::NoRuntime) => {
            (FactValue::Bool(false), FactValue::Unknown)
        }
        Err(ToolGap::Unreadable | ToolGap::Changed) => (FactValue::Unknown, FactValue::Unknown),
    }
}

const fn semantic(tool: Result<ResolvedTool, ToolGap>) -> Subject {
    Subject::Semantic(SemanticFacts {
        kind: NativeKind::NpmCacheClean,
        tool,
    })
}

/// Adds `_cacache` when it is there: under the download cache's rule where it runs and
/// its layout is recognised, with the tool that would clean it, and under the inventory
/// otherwise. npm is looked for only then, so a link, a file or a layout kipple doesn't
/// know starts no process.
fn add_cache(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    root: &Path,
    cache: &Cache,
) -> Result<(), IntegrationError> {
    if cache.kind.is_none() {
        return Ok(());
    }
    let path = root.join(CACACHE);
    let recognised = cache.layout == SUPPORTED;
    let evidence = if recognised {
        Evidence::Signature(format!("{CACACHE}/{INDEX}"))
    } else {
        Evidence::NoSignature
    };
    let (Some(rule), true) = (rule_of(cx, DOWNLOAD_CACHE), recognised) else {
        return report(cx, sink, &path, evidence, cache.layout);
    };
    let tool = cx.tool(NativeTool::Npm);
    let claim = Claim {
        evidence: vec![evidence],
        consequence: Some(DOWNLOADS),
    };
    let supplied = Supplied {
        values: cache_values(cache, &tool),
        subject: semantic(tool),
        ..sink.supplied()
    };
    sink.add_supplied(rule, &path, claim, supplied)
}

/// Reports `_npx` and `_logs` when they are there. They stay: `_npx` holds packages `npx`
/// installed and runs again, and `_logs` is npm's own record.
fn add_inventory(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    root: &Path,
    entries: &[DirEntryMeta],
    layout: FactValue,
) -> Result<(), IntegrationError> {
    for name in REPORTED {
        if entries.iter().any(|entry| entry.name == OsStr::new(name)) {
            let evidence = Evidence::Signature((*name).to_owned());
            report(cx, sink, &root.join(name), evidence, layout)?;
        }
    }
    Ok(())
}

/// Adds `path` under the inventory, which only reports.
fn report(
    cx: &ScanContext<'_>,
    sink: &mut FindingSink<'_>,
    path: &Path,
    evidence: Evidence,
    layout: FactValue,
) -> Result<(), IntegrationError> {
    let Some(rule) = rule_of(cx, CACHE_ENTRIES) else {
        return Ok(());
    };
    let claim = Claim {
        evidence: vec![evidence],
        consequence: None,
    };
    let supplied = Supplied {
        values: [(LAYOUT, layout)].into(),
        ..sink.supplied()
    };
    sink.add_supplied(rule, path, claim, supplied)
}
