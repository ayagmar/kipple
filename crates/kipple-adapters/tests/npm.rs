//! npm against its upstream cache layouts built with `FixtureTree`, through the real engine
//! with injected roots, processes and tool (spec M4 pinned 33, S5 npm §3 and §7): what each
//! entry of the cache root is reported as, the facts supplied with `_cacache`, what stays
//! closed, and what apply reads again. Each test names the bug it keeps out.
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that can't be set up fails the test"
)]

#[path = "agents/host.rs"]
mod host;
#[path = "fixtures/npm/v11_20_0.rs"]
mod npm_v11_20_0;
#[path = "fixtures/npm/v12_2_1.rs"]
mod npm_v12_2_1;
#[path = "fixtures/tree.rs"]
mod tree;

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use host::{Host, Scan, abs, running};
use kipple_core::{
    Activity, Evidence, FactValue, FileIdentity, Fingerprint, IntegrationId, Inventory, NativeKind,
    NativeTool, OwnerStatus, Platform, Proposal, ResolvedTool, SemanticFacts, Stamp, Subject,
    SymbolicRoot, ToolFile, ToolGap, Unread, Verdict,
};
use tree::{FixtureTree, Holds};

// Sizes and times don't decide anything here, the layouts' constants are for the
// resolver's tests, and the host serves suites that ask more of it.
const _: () = {
    let _ = FixtureTree::sized;
    let _ = npm_v11_20_0::install;
    let _ = npm_v12_2_1::install;
    let _ = (npm_v11_20_0::NODE, npm_v12_2_1::NODE);
    let _ = (npm_v11_20_0::ENGINES, npm_v12_2_1::ENGINES);
    let _ = (npm_v11_20_0::VERSION, npm_v12_2_1::VERSION);
    let _ = host::Generated::seeded;
    let _ = host::Generated::mutated;
    let _ = host::sealed;
    let _ = (Host::unresolved, Host::path);
    let _ = (
        Scan::verdict_at,
        Scan::holds,
        Scan::omissions,
        Scan::advice,
        Scan::read,
        Scan::all,
    );
};

const NPM: IntegrationId = IntegrationId("npm");
const CACHE: &str = "builtin.npm.cache";
const INVENTORY: &str = "builtin.npm.inventory";

const ROOT: &str = "home/.npm";
const CACACHE: &str = "home/.npm/_cacache";

const SUPPORTED: FactValue = FactValue::Enum("supported");
const UNKNOWN_LAYOUT: FactValue = FactValue::Enum("unknown");

/// npm 11.20.0 as the platform resolves it: its script and the node beside it.
fn npm() -> ResolvedTool {
    let file = |at: &str, file: u128| {
        let stamp = Stamp::new(1_793_458_575, 0).unwrap();
        let fingerprint = Fingerprint {
            changed: stamp,
            modified: stamp,
            born: None,
            size: 1,
        };
        (abs(at), FileIdentity { volume: 9, file }, fingerprint)
    };
    let (path, identity, fingerprint) = file("/usr/lib/node_modules/npm/bin/npm-cli.js", 1);
    let (node, node_identity, node_fingerprint) = file("/usr/bin/node", 2);
    ResolvedTool {
        tool: NativeTool::Npm,
        path,
        identity,
        fingerprint,
        version: "11.20.0".to_owned(),
        runtime: Some(ToolFile {
            path: node,
            identity: node_identity,
            fingerprint: node_fingerprint,
        }),
    }
}

/// A Linux host with `tree`, its npm cache root at `~/.npm`, and npm 11.20.0 on `PATH`.
fn host(tree: FixtureTree) -> Host {
    let mut host = Host::new(tree);
    host.npm = Ok(npm());
    host
}

/// A cache as npm 11.20.0 leaves it after an install and an `npx` run.
fn v11() -> FixtureTree {
    let mut tree = FixtureTree::new();
    npm_v11_20_0::cache(&mut tree, ROOT);
    tree
}

fn subject(scan: &Scan, relative: &str) -> Subject {
    scan.finding(relative)
        .facts
        .as_ref()
        .unwrap()
        .supplied
        .subject
        .clone()
}

/// Catches `CACHEDIR.TAG` taken as cacache's signature: npm 11's cacache doesn't write it,
/// so the cache of the npm most people run would never be recognised.
#[test]
fn signature_needs_no_cachedir_tag() {
    for (version, layout) in [
        ("11.20.0", npm_v11_20_0::cache as fn(&mut FixtureTree, &str)),
        ("12.2.1", npm_v12_2_1::cache),
    ] {
        let mut tree = FixtureTree::new();
        layout(&mut tree, ROOT);

        let scan = host(tree).scan();

        assert_eq!(scan.found(CACHE), [CACACHE], "{version}");
        assert_eq!(scan.fact(CACACHE, "layout"), Some(SUPPORTED), "{version}");
        let evidence = &scan.finding(CACACHE).evidence;
        assert_eq!(
            evidence,
            &[Evidence::Signature("_cacache/index-v5".to_owned())],
            "{version}"
        );
        assert_eq!(scan.owner(NPM), OwnerStatus::Supported, "{version}");
    }
}

/// Catches `CACHEDIR.TAG` alone read as a cache: any tool can write one, and the directory
/// may hold nothing of cacache's.
#[test]
fn tag_alone_is_unknown() {
    let mut tree = FixtureTree::new();
    tree.file(
        "home/.npm/_cacache/CACHEDIR.TAG",
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .file("home/.npm/_cacache/mine.txt", b"not npm's");

    let scan = host(tree).scan();

    assert_eq!(scan.fact(CACACHE, "layout"), Some(UNKNOWN_LAYOUT));
    assert_eq!(scan.finding(CACACHE).evidence, [Evidence::NoSignature]);
    assert_eq!(scan.owner(NPM), OwnerStatus::Unsupported);
    assert!(
        scan.protection(CACACHE, NPM).is_some(),
        "it stays closed, even to npm"
    );
}

/// Catches a linked `_cacache` read through its link: npm would empty the directory it
/// leads to, wherever that is.
#[test]
fn linked_cacache_blocks() {
    let mut tree = FixtureTree::new();
    npm_v11_20_0::cache(&mut tree, "home/elsewhere");
    tree.dir(ROOT)
        .link(CACACHE, "../elsewhere/_cacache", Holds::AsWritten);

    let scan = host(tree).scan();

    assert_eq!(
        scan.fact(CACACHE, "cache.is_link"),
        Some(FactValue::Bool(true))
    );
    assert_eq!(scan.fact(CACACHE, "layout"), Some(UNKNOWN_LAYOUT));
    assert_eq!(scan.owner(NPM), OwnerStatus::Unsupported);
}

/// Catches a later cacache's layout, or anything else in `_cacache`, taken for the one
/// the contract test saw npm clean.
#[test]
fn unknown_cache_format_blocks() {
    let mut tree = FixtureTree::new();
    tree.file("home/.npm/_cacache/index-v6/1f/2e/entry", b"")
        .file("home/.npm/_cacache/content-v2/sha512/9b/4e/blob", b"")
        .file("home/.npm/_cacache/index-v5", b"a file, not the index");

    let scan = host(tree).scan();

    assert_eq!(scan.fact(CACACHE, "layout"), Some(UNKNOWN_LAYOUT));
    assert_eq!(
        scan.fact(CACACHE, "cache.is_link"),
        Some(FactValue::Bool(false))
    );
    assert_eq!(scan.owner(NPM), OwnerStatus::Unsupported);
}

/// Catches `_npx` or `_logs` offered with the cache: `_npx` holds the packages `npx` runs
/// again, and npm's logs are its record. Only `_cacache` opens, and only to npm.
#[test]
fn npx_and_logs_survive() {
    let scan = host(v11()).scan();

    assert_eq!(scan.found(CACHE), [CACACHE]);
    assert_eq!(scan.found(INVENTORY), ["home/.npm/_logs", "home/.npm/_npx"]);
    for kept in ["home/.npm/_npx", "home/.npm/_logs"] {
        assert_eq!(scan.proposal(kept), Proposal::Report, "{kept}");
        assert!(scan.protection(kept, NPM).is_some(), "{kept} is closed");
    }
    let notifier = "home/.npm/_update-notifier-last-checked";
    assert!(
        scan.protection(notifier, NPM).is_some(),
        "npm's own file is closed"
    );
    assert_eq!(
        scan.protection(CACACHE, NPM),
        None,
        "the download cache opens to npm"
    );
    let node = IntegrationId("node");
    assert!(
        scan.protection(CACACHE, node).is_some(),
        "but to no one else"
    );
}

/// Catches an `npx` run's `node_modules` read as a project's dependencies: the Node
/// integration would offer packages npm keeps to run again, which its cache can't restore
/// offline. Node may see it below a project root, but the cache root holds it.
#[test]
fn npx_node_modules_is_no_node_candidate() {
    let mut host = host(v11());
    host.root(SymbolicRoot::ProjectRoots, &["home"]);

    let scan = host.scan();

    let modules = "home/.npm/_npx/0123456789abcdef/node_modules";
    let node = IntegrationId("node");
    assert!(
        scan.protection(modules, node).is_some(),
        "the cache root holds it"
    );
    let verdict = scan.verdict(modules);
    assert!(
        matches!(verdict, Verdict::Protected(_)),
        "never offered, found {verdict:?}"
    );
}

/// Catches the default cache opened because a setting moved npm's cache elsewhere: what is
/// left in `~/.npm` is still npm's, and npm may use it again.
#[test]
fn default_cache_stays_closed_when_moved() {
    let mut tree = v11();
    npm_v12_2_1::cache(&mut tree, "home/code/npm-cache");
    let mut host = host(tree);
    host.root(SymbolicRoot::NpmCache, &["home/code/npm-cache"]);

    let scan = host.scan();

    assert_eq!(scan.found(CACHE), ["home/code/npm-cache/_cacache"]);
    assert_eq!(scan.protection("home/code/npm-cache/_cacache", NPM), None);
    assert!(
        scan.protection(CACACHE, NPM).is_some(),
        "the default cache stays closed"
    );
}

/// Catches a cache cleaned while npm or `npx` runs, at the scan or by apply: npm writes
/// into `_cacache` as it installs, and a half-removed index fails a concurrent install.
#[test]
fn running_npm_holds() {
    let by_name = running(4100, "npm", "/usr/bin/node");
    let mut by_script = running(4101, "node", "/usr/bin/node");
    by_script.argv = Ok(["node", "/usr/lib/node_modules/npm/bin/npx-cli.js", "tool"]
        .into_iter()
        .map(OsString::from)
        .collect());
    for process in [by_name, by_script] {
        let scan = host(v11()).scan();
        let idle = scan.activity(CACACHE);
        scan.show(|shown| shown.running.push(process.clone()));
        let mut busy = host(v11());
        busy.processes.running.push(process);

        let at_apply = scan.now(CACACHE).unwrap().activity;
        let at_scan = busy.scan().activity(CACACHE);

        assert!(
            matches!(idle, Activity::IdleExceptSystem { .. }),
            "idle but for the session's sealed processes first, found {idle:?}"
        );
        for activity in [at_scan, at_apply] {
            assert!(
                matches!(activity, Activity::InUse { .. }),
                "npm in use, found {activity:?}"
            );
        }
    }
}

/// Catches a node whose command line can't be read taken as idle: it may be running npm.
#[test]
fn unreadable_node_process_blocks() {
    let mut node = running(4100, "node", "/usr/bin/node");
    node.argv = Err(Unread::Failed(Arc::new(
        io::ErrorKind::PermissionDenied.into(),
    )));
    let mut host = host(v11());
    host.processes.running.push(node);

    let scan = host.scan();

    let activity = scan.activity(CACACHE);
    assert!(
        matches!(activity, Activity::Unknown(_)),
        "unknown, found {activity:?}"
    );
}

/// Catches a process listing that came back short read as complete: npm may be among the
/// processes it missed.
#[test]
fn failed_process_probe_blocks() {
    let mut host = host(v11());
    host.processes.inventory = Inventory::Partial {
        reason: "the listing stopped".to_owned(),
    };

    let scan = host.scan();

    let activity = scan.activity(CACACHE);
    assert!(
        matches!(activity, Activity::Unknown(_)),
        "unknown, found {activity:?}"
    );
}

/// Catches the clean offered where it was never verified: macOS and Windows report the
/// download cache, ask nothing of npm, and give it no native subject.
#[test]
fn macos_and_windows_are_report_only() {
    for platform in [Platform::Macos, Platform::Windows] {
        let mut host = host(v11());
        host.platform = platform;

        let scan = host.scan();

        assert_eq!(scan.found(CACHE), Vec::<String>::new(), "{platform:?}");
        let reported = ["home/.npm/_cacache", "home/.npm/_logs", "home/.npm/_npx"];
        assert_eq!(scan.found(INVENTORY), reported, "{platform:?}");
        assert_eq!(scan.proposal(CACACHE), Proposal::Report, "{platform:?}");
        assert_eq!(subject(&scan, CACACHE), Subject::Plain, "{platform:?}");
    }
}

/// Catches a tool gap supplied as a fact that lets the clean through, or a gap lost: the
/// facts and the semantic subject say which npm would run, or why none can.
#[test]
fn the_tool_is_supplied_as_the_facts_that_decide() {
    let cases = [
        (Ok(npm()), FactValue::Bool(true), SUPPORTED),
        (
            Err(ToolGap::UnverifiedVersion),
            FactValue::Bool(true),
            FactValue::Enum("unsupported"),
        ),
        (
            Err(ToolGap::Missing),
            FactValue::Bool(false),
            FactValue::Unknown,
        ),
        (
            Err(ToolGap::Script),
            FactValue::Bool(false),
            FactValue::Unknown,
        ),
        (
            Err(ToolGap::Unreadable),
            FactValue::Unknown,
            FactValue::Unknown,
        ),
    ];
    for (tool, executable, version) in cases {
        let mut host = host(v11());
        host.npm = tool.clone();

        let scan = host.scan();

        let found = (
            scan.fact(CACACHE, "npm.executable_verified"),
            scan.fact(CACACHE, "npm.version_supported"),
        );
        assert_eq!(found, (Some(executable), Some(version)), "{tool:?}");
        let semantic = Subject::Semantic(SemanticFacts {
            kind: NativeKind::NpmCacheClean,
            tool: tool.clone(),
        });
        assert_eq!(subject(&scan, CACACHE), semantic, "{tool:?}");
    }
}

/// Catches apply trusting the scan's npm: one updated, removed or swapped since must be
/// read again, and the cache with it.
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test changes the cache between the scan and apply's reading"
)]
fn apply_reads_the_tool_and_the_cache_again() {
    let scan = host(v11()).scan();
    scan.resolve_npm(Err(ToolGap::UnverifiedVersion));

    let now = scan.now(CACACHE).unwrap();

    let version = now.supplied.values.get("npm.version_supported").copied();
    assert_eq!(version, Some(FactValue::Enum("unsupported")));
    let semantic = Subject::Semantic(SemanticFacts {
        kind: NativeKind::NpmCacheClean,
        tool: Err(ToolGap::UnverifiedVersion),
    });
    assert_eq!(now.supplied.subject, semantic);

    let gone: PathBuf = scan.path("home/.npm/_cacache/index-v5");
    std::fs::remove_dir_all(&gone).unwrap();
    let layout = scan
        .now(CACACHE)
        .unwrap()
        .supplied
        .values
        .get("layout")
        .copied();
    assert_eq!(layout, Some(UNKNOWN_LAYOUT), "the layout is read again");
}
