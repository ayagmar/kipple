//! Codex against upstream layouts built with `FixtureTree`: what each entry of its store and
//! home is reported as, the facts supplied with it, and what stays closed
//! (`docs/06-adapters-and-rules.md` §4, S5 codex §7). `builtin.codex.stale-releases` is
//! `report` here, so its tests assert what is supplied and never a verdict.
#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a fixture that can't be set up fails the test"
)]

#[path = "fixtures/tree.rs"]
mod tree;

#[path = "fixtures/codex/demo.rs"]
mod codex_demo;
#[path = "fixtures/codex/v0_165_0.rs"]
mod codex_v0_165_0;
#[path = "fixtures/codex/v0_166_0.rs"]
mod codex_v0_166_0;
#[path = "fixtures/codex/host.rs"]
mod host;

use std::fs;

use host::{DAEMON, HOME, MUSL, STANDALONE, Scan, Scene, at, component, name, release};
use kipple_core::{
    ActivityEvidence, Evidence, FactValue, GroupGap, Hold, IntegrationId, Inventory, Omission,
    OwnerStatus, Platform, Policy, Proposal, ReleaseRole, ReleaseState, UnresolvedRoot, Verdict,
};
use tree::Holds;

const V164: &str = "0.164.0";
const V165: &str = codex_v0_165_0::VERSION;
const V166: &str = codex_v0_166_0::VERSION;

const ACTIVE: &str = "packages/standalone/releases/0.166.0-x86_64-unknown-linux-musl";
const ROLLBACK: &str = "packages/standalone/releases/0.165.0-x86_64-unknown-linux-musl";
const OLD: &str = "packages/standalone/releases/0.164.0-x86_64-unknown-linux-musl";

const TUI_LOG: &str = "log/codex-tui.log";
const CATALOG: &str = "cache/remote_plugin_catalog/0123456789abcdef.json";

const SUPPORTED: FactValue = FactValue::Enum("supported");

/// A store as an update leaves one: selected, rollback, and one left behind.
fn verified() -> Scene {
    let mut scene = Scene::new();
    scene.store(STANDALONE, &[V164, V165, V166], V166, V165);
    scene
}

/// [`verified`], with the log and the catalog beside the store.
fn verified_with_files() -> Scene {
    let mut scene = verified();
    scene
        .tree
        .file(&at(TUI_LOG), b"old\n")
        .file(&at(CATALOG), b"{}");
    scene
}

const fn blocked(gap: GroupGap) -> ReleaseState {
    ReleaseState::Blocked(gap)
}

fn is_report(scan: &Scan, path: &str) -> bool {
    scan.facts(path).proposal == Proposal::Report
}

fn has_signature(scan: &Scan, path: &str) -> bool {
    let signed = |evidence: &Evidence| matches!(evidence, Evidence::Signature(_));
    scan.finding(path).evidence.iter().any(signed)
}

/// What one release of a store should be supplied with.
struct Expected<'path> {
    store: &'static str,
    role: ReleaseRole,
    version: &'static str,
    path: &'path str,
}

fn assert_release(scan: &Scan, expected: &Expected<'_>) {
    let path = expected.path;
    assert_eq!(scan.rule(path), "stale-releases", "{path}");
    let facts = scan.release(path);
    assert_eq!(facts.group.component, expected.store, "{path}");
    assert_eq!(facts.group.target, MUSL, "{path}");
    assert_eq!(facts.version.0, expected.version, "{path}");
    assert_eq!(facts.role, expected.role, "{path}");
    assert_eq!(facts.state, ReleaseState::Sound, "{path}");
    assert!(has_signature(scan, path), "expected a signature at {path}");
    for (fact, value) in [
        ("layout", SUPPORTED),
        ("codex.version_supported", SUPPORTED),
        ("inventory", FactValue::Enum("complete")),
        ("installer_activity", FactValue::Enum("idle")),
    ] {
        assert_eq!(scan.value(path, fact), Some(value), "{fact} at {path}");
    }
    assert!(
        matches!(
            scan.facts(path).supplied.activity,
            ActivityEvidence::ReleaseStore { .. }
        ),
        "expected {path} to be judged by its store's processes"
    );
}

/// Catches either store going unrecognised, 0.165.0's manifest without `execServer` read as
/// damage, a release's facts coming from the wrong component, or the daemon's store missing
/// its second lock.
#[test]
fn standalone_and_daemon_layouts_recognised() {
    let mut scene = verified();
    scene.store(DAEMON, &[V165, V166], V166, V165);

    let scan = scene.scan();

    assert_eq!(scan.owner(), OwnerStatus::Supported);
    let daemon_active = release(DAEMON, &name(V166));
    let daemon_rollback = release(DAEMON, &name(V165));
    for (store, role, version, path) in [
        (STANDALONE, ReleaseRole::Active, V166, ACTIVE),
        (STANDALONE, ReleaseRole::Previous, V165, ROLLBACK),
        (STANDALONE, ReleaseRole::Installed, V164, OLD),
        (DAEMON, ReleaseRole::Active, V166, daemon_active.as_str()),
        (
            DAEMON,
            ReleaseRole::Previous,
            V165,
            daemon_rollback.as_str(),
        ),
    ] {
        let expected = Expected {
            store,
            role,
            version,
            path,
        };
        assert_release(&scan, &expected);
    }
    assert_eq!(scan.release(ACTIVE).locks.len(), 1);
    assert_eq!(scan.release(&daemon_active).locks.len(), 2);
}

/// Catches a release listing only some of its files, which would leave a process running a
/// helper of it unseen.
#[test]
fn a_release_lists_every_file() {
    let scan = verified().scan();

    let files = &scan.release(ACTIVE).files;
    let home = scan.built.home();
    let helper = home.join(ACTIVE).join("codex-path/rg");
    assert!(
        files.contains(&helper),
        "expected the helper among {files:#?}"
    );
    assert_eq!(files.len(), 7, "{files:#?}");
}

fn only_reported_unknown(scan: &Scan) {
    assert_eq!(scan.owner(), OwnerStatus::Unsupported);
    assert_eq!(scan.advice().len(), 0);
    for finding in &scan.report.findings {
        assert_eq!(finding.evidence, [Evidence::NoSignature], "{finding:?}");
        let facts = finding.facts.as_ref().unwrap();
        assert_eq!(facts.consequence, None, "{finding:?}");
        let verdict = Policy::default().verdict(&finding.rule, &finding.path, facts);
        assert!(
            matches!(verdict, Verdict::Protected(_)),
            "expected {finding:?} to be protected, got {verdict:?}"
        );
    }
}

/// Catches anything in a store of an unknown shape being read as a release, which a later
/// rule would then act on: an unknown sibling, or a link anywhere a store must not have one.
#[test]
fn unknown_layout_is_no_signature() {
    let linked = |path: &str, scene: &mut Scene| {
        scene.tree.dir("elsewhere/x");
        scene.tree.link(path, "elsewhere/x", Holds::AbsoluteInTree);
    };
    let mut sibling = verified();
    sibling.tree.dir(&at("packages/foo"));
    let mut store = Scene::new();
    linked(&at("packages"), &mut store);
    let mut component_link = Scene::new();
    linked(&at("packages/standalone"), &mut component_link);
    let mut releases_link = Scene::new();
    releases_link.select(STANDALONE, &name(V166));
    linked(&at("packages/standalone/releases"), &mut releases_link);
    let mut stray = verified();
    stray.tree.file(&at("packages/standalone/notes.txt"), b"x");

    for (case, scene, at_path) in [
        ("an unknown sibling", sibling, "packages/foo"),
        ("a linked store", store, "packages"),
        ("a linked component", component_link, "packages/standalone"),
        (
            "linked releases",
            releases_link,
            "packages/standalone/releases",
        ),
        (
            "a stray in a component",
            stray,
            "packages/standalone/notes.txt",
        ),
    ] {
        let scan = scene.scan();
        assert_eq!(
            scan.get(at_path).map(|_| scan.rule(at_path)),
            Some("release-inventory"),
            "{case}: {:#?} {:#?}",
            scan.rows(),
            scan.omissions()
        );
        only_reported_unknown(&scan);
        assert!(
            !scan.rows().iter().any(|row| row.starts_with("stale-")),
            "{case}: nothing is a release: {:#?}",
            scan.rows()
        );
    }
}

/// Catches a directory of the store that can't be listed passing for one with nothing unknown
/// in it: it can hide a stray and every install marker while `current` and the releases still
/// answer, and the removing rules would run on a layout nobody saw.
#[test]
fn a_store_directory_that_cant_be_listed_is_an_unknown_layout() {
    for part in ["packages/standalone", "packages/standalone/releases"] {
        let mut scene = verified_with_files();
        scene.unlistable.push(at(part));

        let scan = scene.scan();

        only_reported_unknown(&scan);
        assert!(
            !scan.rows().iter().any(|row| row.starts_with("stale-")),
            "{part}: nothing is a release: {:#?}",
            scan.rows()
        );
    }
}

/// Catches a home with no store, such as an npm install's, passing for a verified one.
#[test]
fn home_without_a_store_is_unsupported() {
    let mut scene = Scene::new();
    scene
        .tree
        .file(&at(TUI_LOG), b"old\n")
        .file(&at(CATALOG), b"{}");

    let scan = scene.scan();

    only_reported_unknown(&scan);
    assert_eq!(scan.rule(TUI_LOG), "legacy-tui-log");
    assert_eq!(scan.value(TUI_LOG, "layout"), Some(FactValue::Unknown));
    assert!(
        scan.closed(TUI_LOG),
        "expected the home to stay closed with no store"
    );
}

/// Catches a relative `CODEX_HOME` falling back to the default home, or being reported as a
/// home with nothing in it.
#[test]
fn relative_codex_home_is_an_omission() {
    let mut scene = verified_with_files();
    scene.unresolved = Some(UnresolvedRoot::NotAbsolute {
        var: "CODEX_HOME",
        value: "relative/home".to_owned(),
    });

    let scan = scene.scan();

    assert_eq!(scan.rows(), Vec::<String>::new());
    assert_eq!(scan.owner(), OwnerStatus::Unsupported);
    assert!(
        scan.omissions()
            .iter()
            .any(|omission| matches!(omission, Omission::RootUnresolved { .. })),
        "expected the root to be reported as unknown, got {:#?}",
        scan.omissions()
    );
    assert!(
        scan.closed(TUI_LOG),
        "expected a home with no path to hold everything"
    );
}

/// Catches an unverified version being treated like the one before it. Below the list or
/// above it the home closes for everyone, `git` included, while a verified one lets both in.
#[test]
fn unverified_version_closes_the_home() {
    let worktree = "worktrees/abcd/repo";
    for version in [V164, "0.167.0"] {
        let mut scene = Scene::new();
        scene.store(STANDALONE, &[V165, version], version, V165);
        scene
            .tree
            .file(&at(TUI_LOG), b"old\n")
            .file(&at(CATALOG), b"{}")
            .dir(&at(worktree));

        let scan = scene.scan();

        only_reported_unknown(&scan);
        let active = release(STANDALONE, &name(version));
        assert_eq!(scan.rule(&active), "release-inventory", "{version}");
        assert_eq!(scan.rule(ROLLBACK), "release-inventory", "{version}");
        assert_eq!(
            scan.value(&active, "codex.version_supported"),
            Some(FactValue::Enum("unsupported")),
            "{version}"
        );
        assert_eq!(scan.value(&active, "layout"), Some(FactValue::Unknown));
        assert!(
            scan.closed(CATALOG),
            "expected {version} to close the catalog"
        );
        let owners = scan.owners_with(IntegrationId::GIT);
        assert!(
            scan.closes(worktree, IntegrationId::GIT, &owners),
            "expected {version} to close the worktrees"
        );
    }

    let mut control = verified_with_files();
    control.tree.dir(&at(worktree));
    let scan = control.scan();
    let owners = scan.owners_with(IntegrationId::GIT);
    assert!(
        !scan.closed(CATALOG),
        "expected a verified home to open the catalog"
    );
    assert!(
        !scan.closes(worktree, IntegrationId::GIT, &owners),
        "expected a verified home to open its worktrees to git"
    );
}

/// Catches one release of the pair deciding the home alone: a selected daemon of an
/// unverified version closes the home however good the standalone store is.
#[test]
fn every_selected_release_must_be_verified() {
    let mut scene = verified_with_files();
    scene.store(DAEMON, &[V165, "0.167.0"], "0.167.0", V165);

    let scan = scene.scan();

    only_reported_unknown(&scan);
    assert!(
        scan.closed(CATALOG),
        "expected the unverified daemon to close the catalog"
    );
}

/// Each way `selection-history` can be wrong, and the gap it leaves.
fn bad_histories(selected: &str, stamp: &str) -> Vec<(&'static str, Vec<u8>, GroupGap)> {
    let line = |previous: &str, written: &str| format!("{selected}\n{previous}\n{written}\n");
    let other = name(V164);
    let cases = [
        (
            "two lines",
            format!("{selected}\n\n"),
            GroupGap::HistoryMalformed,
        ),
        (
            "no last newline",
            format!("{selected}\n\n{stamp}"),
            GroupGap::HistoryMalformed,
        ),
        (
            "four lines",
            format!("{}x\n", line("", stamp)),
            GroupGap::HistoryMalformed,
        ),
        ("a slash", line("a/b", stamp), GroupGap::HistoryMalformed),
        ("a dot", line(".", stamp), GroupGap::HistoryMalformed),
        ("two dots", line("..", stamp), GroupGap::HistoryMalformed),
        (
            "a stale link",
            line("", "50 1 1793458575.030397325"),
            GroupGap::HistoryStale,
        ),
        (
            "not the selection",
            format!("{other}\n\n{stamp}\n"),
            GroupGap::HistoryStale,
        ),
    ];
    let bytes =
        |(case, text, gap): (&'static str, String, GroupGap)| (case, text.into_bytes(), gap);
    cases.into_iter().map(bytes).collect()
}

/// Catches a history that doesn't match what `current` says being used anyway, which would
/// name the wrong rollback, a gap reaching the other component, or a stale history closing
/// the layout: upstream rewrites it on any install, and the home is still what it read.
#[test]
fn stale_selection_history_blocks() {
    let mut scene = verified();
    scene.store(DAEMON, &[V165, V166], V166, V165);
    let built = scene.build();
    let history = built.home().join("packages/standalone/selection-history");
    let selected = name(V166);
    for (case, bytes, gap) in bad_histories(&selected, &built.stamp(STANDALONE)) {
        fs::write(&history, bytes).unwrap();

        let scan = built.scan();

        for path in [ACTIVE, ROLLBACK, OLD] {
            assert_eq!(scan.release(path).state, blocked(gap), "{case} at {path}");
        }
        assert_eq!(
            scan.release(ROLLBACK).role,
            ReleaseRole::Installed,
            "{case}: a bad history names no rollback"
        );
        let daemon = release(DAEMON, &name(V166));
        assert_eq!(scan.release(&daemon).state, ReleaseState::Sound, "{case}");
        assert_eq!(scan.value(ACTIVE, "layout"), Some(SUPPORTED), "{case}");
    }
}

/// Catches a store from before 0.164.0 being read as having no rollback, rather than as a
/// group nothing can be settled for.
#[test]
fn missing_selection_history_blocks() {
    let mut scene = Scene::new();
    scene
        .releases(STANDALONE, &[V165, V166])
        .select(STANDALONE, &name(V166));

    let scan = scene.scan();

    assert_eq!(
        scan.release(ROLLBACK).state,
        blocked(GroupGap::HistoryMissing)
    );
}

/// Catches `previous` being refused for holding a `.`, the mistake to make from "may not
/// contain `.`": every real release name does.
#[test]
fn real_release_names_pass_selection_history() {
    let prerelease = "0.166.0-alpha.5.1";
    for previous in [V165, prerelease, "0.166.0-beta.2"] {
        let mut scene = Scene::new();
        scene.store(STANDALONE, &[previous, V166], V166, previous);

        let scan = scene.scan();

        let path = release(STANDALONE, &name(previous));
        assert_eq!(scan.release(&path).state, ReleaseState::Sound, "{previous}");
        assert_eq!(
            scan.release(&path).role,
            ReleaseRole::Previous,
            "{previous}"
        );
    }
}

/// Catches an empty `previous`, which upstream writes after a first install, being refused.
#[test]
fn empty_previous_is_a_valid_history() {
    let mut scene = Scene::new();
    scene
        .releases(STANDALONE, &[V165, V166])
        .select(STANDALONE, &name(V166))
        .history(STANDALONE, &name(V166), "");

    let scan = scene.scan();

    assert_eq!(scan.release(ROLLBACK).state, ReleaseState::Sound);
    assert_eq!(scan.release(ROLLBACK).role, ReleaseRole::Installed);
}

/// A directory of the store under `name`, whose manifest is the valid one with `from`
/// replaced by `to`.
fn with_manifest(from: &str, to: &str) -> Scene {
    let mut scene = verified();
    let manifest = codex_v0_166_0::manifest(V165, MUSL, "bin/codex").replace(from, to);
    let dir = at(&release(STANDALONE, &name("0.163.0")));
    codex_v0_166_0::release(&mut scene.tree, &dir, "0.163.0", MUSL);
    scene
        .tree
        .file(&format!("{dir}/codex-package.json"), manifest.as_bytes());
    scene
}

/// The entry `0.163.0`, which is the one each case of [`signature_cases`] damages.
fn damaged() -> String {
    release(STANDALONE, &name("0.163.0"))
}

fn plain_release(mut build: impl FnMut(&mut Scene, &str)) -> Scene {
    let mut scene = verified();
    let dir = at(&damaged());
    codex_v0_166_0::release(&mut scene.tree, &dir, "0.163.0", MUSL);
    build(&mut scene, &dir);
    scene
}

/// A store with the entry [`damaged`] holding a manifest and `build`'s files, but not a
/// release's.
fn with_files(build: impl FnOnce(&mut Scene, &str)) -> Scene {
    let mut scene = verified();
    let dir = at(&damaged());
    let manifest = codex_v0_166_0::manifest("0.163.0", MUSL, "bin/codex");
    scene
        .tree
        .file(&format!("{dir}/codex-package.json"), manifest.as_bytes());
    build(&mut scene, &dir);
    scene
}

/// The entry `path` of a verified store, laid out as a release of `version` for `target`.
fn entry_named(path: &str, version: &str, target: &str) -> Scene {
    let mut scene = verified();
    codex_v0_166_0::release(&mut scene.tree, &at(path), version, target);
    scene
}

/// What a damaged release can be, past its manifest.
fn damaged_cases() -> Vec<(&'static str, Scene)> {
    let legacy = {
        let mut scene = verified();
        let dir = at(&damaged());
        scene
            .tree
            .file(&format!("{dir}/codex"), b"\x7fELF")
            .file(&format!("{dir}/codex-resources/rg"), b"\x7fELF");
        scene
    };
    let no_program = with_files(|scene, dir| {
        scene.tree.file(&format!("{dir}/bin/other"), b"x").link(
            &format!("{dir}/codex"),
            "bin/codex",
            Holds::AsWritten,
        );
    });
    let bad_alias = with_files(|scene, dir| {
        scene
            .tree
            .file(&format!("{dir}/bin/codex"), b"x")
            .file(&format!("{dir}/bin/other"), b"x")
            .link(&format!("{dir}/codex"), "bin/other", Holds::AsWritten);
    });
    let extra_link = plain_release(|scene, dir| {
        scene
            .tree
            .link(&format!("{dir}/bin/rg"), "codex", Holds::AsWritten);
    });
    let fifo = plain_release(|scene, dir| {
        let path = format!("{dir}/bin/pipe");
        scene.tree.file(&path, b"");
        scene.odd.push(path);
    });
    let mount = plain_release(|scene, dir| scene.mounts.push(format!("{dir}/codex-resources")));
    let mut linked = verified();
    linked
        .tree
        .link(&at(&damaged()), &at(ACTIVE), Holds::AbsoluteInTree);
    vec![
        ("legacy flat layout", legacy),
        ("a half-written install", no_program),
        ("an alias to another file", bad_alias),
        ("a link inside", extra_link),
        ("a FIFO inside", fifo),
        ("a mount inside", mount),
        ("itself a link", linked),
    ]
}

/// What a manifest can say that its name doesn't.
fn manifest_cases() -> Vec<(&'static str, Scene)> {
    vec![
        (
            "a manifest of another version",
            with_manifest("\"0.165.0\"", "\"0.162.0\""),
        ),
        (
            "a manifest of another target",
            with_manifest(MUSL, "aarch64-unknown-linux-musl"),
        ),
        ("invalid JSON", with_manifest("{", "")),
        (
            "another entrypoint",
            with_manifest("\"bin/codex\"", "\"bin/other\""),
        ),
        (
            "an unknown layout version",
            with_manifest("\"layoutVersion\": 1", "\"layoutVersion\": 2"),
        ),
    ]
}

/// Entries whose name alone keeps them.
fn name_cases() -> Vec<(&'static str, Scene, String)> {
    let local = release(STANDALONE, &format!("local-{}-{MUSL}", "ab".repeat(32)));
    let unnamed = release(STANDALONE, "backup-0.160");
    let riscv_target = "riscv64gc-unknown-linux-gnu";
    let riscv = release(STANDALONE, &format!("0.163.0-{riscv_target}"));
    vec![
        ("a local build", entry_named(&local, V166, MUSL), local),
        (
            "another name",
            entry_named(&unnamed, "0.160.0", MUSL),
            unnamed,
        ),
        (
            "an unknown target",
            entry_named(&riscv, "0.163.0", riscv_target),
            riscv,
        ),
    ]
}

fn signature_cases() -> Vec<(&'static str, Scene, String)> {
    let at_damaged = |(case, scene): (&'static str, Scene)| (case, scene, damaged());
    let damaged_entries = damaged_cases().into_iter().map(at_damaged);
    let manifests = manifest_cases().into_iter().map(at_damaged);
    damaged_entries
        .chain(manifests)
        .chain(name_cases())
        .collect()
}

/// Catches anything short of upstream's whole signature being offered as a release a rule
/// could later remove, and a kept entry being treated as damage to the releases beside it.
#[test]
fn signature_failures_are_kept() {
    for (case, scene, path) in signature_cases() {
        let scan = scene.scan();

        assert_eq!(scan.rule(&path), "release-inventory", "{case}");
        assert!(is_report(&scan, &path), "{case}: expected report only");
        assert_eq!(scan.release(OLD).state, ReleaseState::Sound, "{case}");
        assert_eq!(scan.owner(), OwnerStatus::Supported, "{case}");
    }
}

/// Catches `.retired.*` being read as an ordinary release: upstream decided it is unused and
/// may be half gone, so it never comes back. A process under it must still reach M4b's
/// check, which needs the files and the group.
#[test]
fn retired_is_reported_never_a_candidate() {
    let mut scene = verified();
    let retired = release(STANDALONE, ".retired.AbCdEfGh");
    let dir = at(&retired);
    codex_v0_165_0::release(&mut scene.tree, &format!("{dir}/release"), V164, MUSL);
    let empty = release(STANDALONE, ".retired.Zz");
    scene.tree.dir(&at(&empty));

    let scan = scene.scan();

    assert_eq!(scan.rule(&retired), "release-inventory");
    assert!(is_report(&scan, &retired), "expected report only");
    let facts = scan.release(&retired);
    assert_eq!(facts.role, ReleaseRole::Retired);
    assert_eq!(facts.version.0, V164);
    assert_eq!(facts.group.target, MUSL);
    assert_eq!(facts.files.len(), 6, "{:#?}", facts.files);
    let leftover = scan.release(&empty);
    assert_eq!(leftover.role, ReleaseRole::Retired);
    assert_eq!(leftover.group.component, STANDALONE);
    assert!(scan.closed(&retired), "expected the host to close it");
}

/// Catches an install in progress being read as a quiet store, or `.staging.*` being opened
/// by the area it sits in. Each marker upstream writes blocks every release of its component
/// and no other.
#[test]
fn staging_entries_are_closed_in_the_open_area() {
    let staging = release(
        STANDALONE,
        ".staging.0.167.0-x86_64-unknown-linux-musl.4242",
    );
    let tempdir = release(STANDALONE, ".staging.Ab12Cd");
    let in_component = |file: &str| format!("{}/{file}", component(STANDALONE));
    let cases: Vec<(String, bool)> = vec![
        (staging, true),
        (tempdir, true),
        (in_component("install.lock.d"), true),
        (in_component("install-lock-Ab12Cd"), true),
        (in_component(".migration-current"), false),
        (in_component(".current.4242"), false),
        (in_component("selection-history.tmp.4242"), false),
        (in_component(".tmpAb12Cd"), false),
    ];
    for (marker, directory) in cases {
        let mut scene = verified();
        scene.store(DAEMON, &[V165, V166], V166, V165);
        if directory {
            scene.tree.dir(&at(&marker));
        } else {
            scene.tree.file(&at(&marker), b"");
        }

        let scan = scene.scan();

        assert_eq!(scan.get(&marker).map(|f| &f.path), None, "{marker}");
        assert_eq!(
            scan.release(OLD).state,
            blocked(GroupGap::InstallerBusy),
            "{marker}"
        );
        assert_eq!(
            scan.value(OLD, "installer_activity"),
            Some(FactValue::Enum("busy")),
            "{marker}"
        );
        let daemon = release(DAEMON, &name(V166));
        assert_eq!(scan.release(&daemon).state, ReleaseState::Sound, "{marker}");
        assert!(scan.closed(&marker), "expected {marker} to stay closed");
        assert!(!scan.closed(OLD), "expected a release to stay open");
    }
}

/// Catches `.migration-current` being read as a marker only: the release it names is about
/// to be selected.
#[test]
fn migration_target_is_a_pending_install() {
    let mut scene = Scene::new();
    scene.store(DAEMON, &[V164, V165, V166], V166, V165);
    let pending = release(DAEMON, &name(V164));
    scene.tree.link(
        &at(&format!("{}/.migration-current", component(DAEMON))),
        &at(&pending),
        Holds::AbsoluteInTree,
    );

    let scan = scene.scan();

    assert_eq!(scan.release(&pending).role, ReleaseRole::PendingInstall);
    assert_eq!(
        scan.release(&pending).state,
        blocked(GroupGap::InstallerBusy)
    );
}

/// Catches an entry of the home being offered as anything, or read at all: credentials and
/// history stay shut even when the store around them is verified.
#[test]
fn auth_and_config_never_candidates() {
    let secret = "sk-very-secret-value";
    let mut scene = verified_with_files();
    let closed = [
        "auth.json",
        "config.toml",
        "managed_config.toml",
        "requirements.toml",
        "work.config.toml",
        "history.jsonl",
        "sessions/2026/rollout.jsonl.zst",
        "sessions/codex-login.log",
        "state_5.sqlite",
        "memories/note.md",
        "brand-new-entry",
    ];
    for path in closed {
        let body = format!("# {secret}\n");
        scene.tree.file(&at(path), body.as_bytes());
    }

    let scan = scene.scan();

    for path in closed {
        assert_eq!(scan.get(path).map(|f| &f.path), None, "{path}");
        assert!(scan.closed(path), "expected {path} to stay closed");
    }
    let opened = scan.opened();
    for path in ["auth.json", "history.jsonl", "state_5.sqlite"] {
        assert!(
            !opened.iter().any(|file| file == path),
            "{path} was opened: {opened:#?}"
        );
    }
    assert!(
        !format!("{:?}", scan.report).contains(secret),
        "nothing of a file may reach the report"
    );
}

/// Catches the daemon's logs and pid files being reported as clutter: deleting one can change
/// which store a daemon resolves to, so only the rotated `.previous` copies are shown, and
/// those are closed too.
#[test]
fn stderr_logs_and_pid_files_never_candidates() {
    let mut scene = verified();
    let never = [
        "app-server-daemon/daemon.stderr.log",
        "app-server-daemon/daemon-updater.stderr.log",
        "app-server-daemon/daemon.pid",
        "app-server-daemon/daemon.pid.lock",
        "app-server-daemon/daemon.lock",
        "app-server-daemon/settings.json",
    ];
    for path in never {
        scene.tree.file(&at(path), b"1\n");
    }
    let previous = "app-server-daemon/daemon.stderr.log.previous";
    scene.tree.file(&at(previous), b"old\n");

    let scan = scene.scan();

    for path in never {
        assert_eq!(scan.get(path).map(|f| &f.path), None, "{path}");
        assert!(scan.closed(path), "expected {path} to stay closed");
    }
    assert_eq!(scan.rule(previous), "store-inventory");
    assert!(is_report(&scan, previous), "expected report only");
    assert!(scan.closed(previous), "expected it to stay closed");
}

/// Each file of the home that can set `log_dir`, one at a time, with what it holds. On a
/// file system that ignores case, Codex opens `CONFIG.TOML` as `config.toml`.
fn config_layers() -> Vec<(&'static str, &'static [u8])> {
    let top: &[u8] = b"log_dir = \"logs\"\n";
    let table: &[u8] = b"[profiles.work]\nlog_dir = \"logs\"\n";
    vec![
        ("config.toml", top),
        ("config.toml", table),
        ("CONFIG.TOML", top),
        ("managed_config.toml", top),
        ("requirements.toml", top),
        ("work.config.toml", top),
        ("work.CONFIG.toml", top),
    ]
}

/// Catches the one log kipple may remove being eligible when something can still be writing
/// it: `log_dir` set in any layer turns the file back on, and a release before 0.166.0 may
/// append to it. Unset, it is Review, since old diagnostics don't come back.
#[test]
fn tui_log_eligible_only_with_log_dir_unset() {
    let unset = verified_with_files().scan();
    assert_eq!(unset.rule(TUI_LOG), "legacy-tui-log");
    assert_eq!(
        unset.value(TUI_LOG, "log_dir.configured"),
        Some(FactValue::Bool(false))
    );
    assert_eq!(
        unset.value(TUI_LOG, "tui_log.writer"),
        Some(FactValue::Enum("none"))
    );
    assert!(
        matches!(unset.verdict(TUI_LOG), Verdict::Review(_)),
        "expected Review, got {:?}",
        unset.verdict(TUI_LOG)
    );

    for (file, body) in config_layers() {
        let mut scene = verified_with_files();
        scene.tree.file(&at(file), body);
        scene.tree.file(&at("logs/codex-tui.log"), b"new\n");

        let scan = scene.scan();

        assert_eq!(
            scan.value(TUI_LOG, "log_dir.configured"),
            Some(FactValue::Bool(true)),
            "{file}"
        );
        assert_eq!(scan.holds(TUI_LOG), [Hold::FactUnmet], "{file}");
        assert_eq!(scan.rule("logs/codex-tui.log"), "store-inventory", "{file}");
    }

    let mut older = Scene::new();
    older.store(STANDALONE, &[V165, V166], V165, V166);
    older.tree.file(&at(TUI_LOG), b"old\n");
    let writer = older.scan();
    assert_eq!(
        writer.value(TUI_LOG, "tui_log.writer"),
        Some(FactValue::Enum("possible"))
    );
    assert_eq!(writer.holds(TUI_LOG), [Hold::FactUnmet]);
}

/// Catches a `log_dir` that can't be followed being dropped: logs kipple can't list are
/// reported as such, whether the directory is outside every root or simply not there yet.
#[test]
fn a_log_dir_out_of_reach_is_reported_unread() {
    let built = verified_with_files().build();
    let home = built.home();
    let outside = home.parent().unwrap().with_file_name("elsewhere");
    for (value, shown) in [
        (outside.to_str().unwrap(), "elsewhere"),
        ("../logs", "../logs"),
        ("logs-not-there-yet", "logs-not-there-yet"),
        ("~/logs", "~/logs"),
    ] {
        let config = format!("log_dir = '{value}'\n");
        fs::write(home.join("config.toml"), config).unwrap();

        let scan = built.scan();

        let rows = scan.rows();
        let reported = rows.iter().filter(|row| row.contains(shown)).count();
        assert_eq!(reported, 1, "{value}: {rows:#?}");
        assert_eq!(scan.holds(TUI_LOG), [Hold::FactUnmet], "{value}");
        assert_eq!(scan.owner(), OwnerStatus::Supported, "{value}");
    }
}

/// Catches one directory set in two layers being reported twice.
#[test]
fn a_log_dir_set_twice_reports_its_logs_once() {
    let mut scene = verified();
    scene
        .tree
        .file(&at("config.toml"), b"log_dir = \"logs\"\n")
        .file(&at("managed_config.toml"), b"log_dir = \"logs\"\n")
        .file(&at("logs/codex-tui.log"), b"new\n");

    let scan = scene.scan();

    let rows = scan.rows();
    let reported = rows
        .iter()
        .filter(|row| row.ends_with("logs/codex-tui.log"));
    assert_eq!(reported.count(), 1, "{rows:#?}");
}

/// Catches a config kipple couldn't read being taken for one that sets nothing, and anything
/// of it reaching the report.
#[test]
fn unparsable_config_keeps_tui_log() {
    let secret = "sk-very-secret-value";
    let huge = "#".repeat(1024 * 1024 + 1);
    let not_toml = format!("token = {secret}\nthis isn't [toml");
    let not_utf8 = [b'#', 0xff, b'\n'];
    let not_a_string = "log_dir = 7\n";
    let bodies: Vec<(&str, &[u8])> = vec![
        ("not TOML", not_toml.as_bytes()),
        ("not UTF-8", &not_utf8),
        ("not a string", not_a_string.as_bytes()),
        ("too large", huge.as_bytes()),
    ];
    for (case, body) in bodies {
        let mut scene = verified_with_files();
        scene.tree.file(&at("config.toml"), body);

        let scan = scene.scan();

        assert_eq!(
            scan.value(TUI_LOG, "log_dir.configured"),
            Some(FactValue::Unknown),
            "{case}"
        );
        assert_eq!(scan.holds(TUI_LOG), [Hold::FactUnknown], "{case}");
        assert!(
            !format!("{:?}", scan.report).contains(secret),
            "{case}: nothing of the file may reach the report"
        );
    }

    let mut denied = verified_with_files();
    denied.tree.file(&at("config.toml"), b"");
    denied.denied.push(at("config.toml"));
    let mut linked = verified_with_files();
    linked.tree.file("home/elsewhere.toml", b"");
    linked.tree.link(
        &at("config.toml"),
        "home/elsewhere.toml",
        Holds::AbsoluteInTree,
    );
    for (case, scene) in [("unreadable", denied), ("a link", linked)] {
        let scan = scene.scan();
        assert_eq!(
            scan.value(TUI_LOG, "log_dir.configured"),
            Some(FactValue::Unknown),
            "{case}"
        );
    }
}

/// Catches the login log being treated like the legacy one: nothing upstream says it may go.
#[test]
fn login_log_stays_report_only() {
    let login = "log/codex-login.log";
    let mut scene = verified();
    scene.tree.file(&at(login), b"login\n");

    let scan = scene.scan();

    assert_eq!(scan.rule(login), "store-inventory");
    assert!(is_report(&scan, login), "expected report only");
    assert_eq!(scan.facts(login).consequence, None);
    assert!(scan.closed(login), "expected the login log to stay closed");
}

/// Catches the catalog's pattern ignoring case, which upstream's names never need. Alone in
/// its directory, since a file system that ignores case makes it the same file as `CATALOG`.
#[test]
fn an_uppercase_catalog_name_is_only_reported() {
    let path = "cache/remote_plugin_catalog/0123456789ABCDEF.json";
    let mut scene = verified();
    scene.tree.file(&at(path), b"{}");

    let scan = scene.scan();

    assert_eq!(scan.rule(path), "store-inventory");
}

/// Catches a rule's reach growing past the names upstream writes.
#[test]
fn catalog_name_must_match() {
    let dir = "cache/remote_plugin_catalog";
    let mut scene = verified_with_files();
    let others = [
        "0123456789abcde.json",
        "0123456789abcdef0.json",
        "0123456789abcdef.tmp",
        "plugins.json",
    ];
    for file in others {
        scene.tree.file(&at(&format!("{dir}/{file}")), b"{}");
    }
    scene.tree.dir(&at(&format!("{dir}/fedcba9876543210.json")));
    scene
        .tree
        .file(&at("cache/codex_app_directory/a.json"), b"{}");

    let scan = scene.scan();

    assert_eq!(scan.rule(CATALOG), "plugin-catalog-cache");
    assert!(
        matches!(scan.verdict(CATALOG), Verdict::Safe | Verdict::Review(_)),
        "expected the catalog to be eligible, got {:?}",
        scan.verdict(CATALOG)
    );
    for file in others {
        let path = format!("{dir}/{file}");
        assert_eq!(scan.rule(&path), "store-inventory", "{file}");
    }
    let directory = format!("{dir}/fedcba9876543210.json");
    assert_eq!(scan.rule(&directory), "store-inventory");
    assert_eq!(
        scan.rule("cache/codex_app_directory"),
        "store-inventory",
        "any other cache is reported"
    );
}

/// Catches both rules shipping dead (an ordinary session holding them forever) or blind: a
/// running Codex holds them, and so does a process list that isn't whole.
#[test]
fn running_codex_holds_catalog_and_log() {
    let idle = verified_with_files().scan();
    for path in [TUI_LOG, CATALOG] {
        let holds = idle.holds(path);
        assert_eq!(holds, [], "an ordinary session holds nothing at {path}");
    }

    let mut running = verified_with_files();
    running.running.push(at(&format!("{ACTIVE}/bin/codex")));
    let in_store = running.scan();
    for path in [TUI_LOG, CATALOG] {
        assert_eq!(in_store.holds(path), [Hold::InUse], "{path}");
    }

    let mut outside = verified_with_files();
    outside
        .running
        .push("usr/lib/node_modules/bin/codex".to_owned());
    let elsewhere = outside.scan();
    for path in [TUI_LOG, CATALOG] {
        assert_eq!(
            elsewhere.holds(path),
            [Hold::InUse],
            "a Codex from elsewhere: {path}"
        );
    }

    let mut partial = verified_with_files();
    partial.inventory = Inventory::Partial {
        reason: "hidepid".to_owned(),
    };
    let unsure = partial.scan();
    for path in [TUI_LOG, CATALOG] {
        assert_eq!(unsure.holds(path), [Hold::UnknownActivity], "{path}");
    }
}

/// Catches a removing rule reaching another OS, where nothing here was verified on a real
/// store, or an OS's selected release being offered for review.
#[test]
fn macos_and_windows_are_report_only() {
    let mut mac = verified_with_files();
    mac.platform = Platform::Macos;
    let windows_target = "x86_64-pc-windows-msvc";
    let mut windows = Scene::new();
    let selected = format!("{V166}-{windows_target}");
    let rollback = format!("{V165}-{windows_target}");
    for (dir, version) in [(&selected, V166), (&rollback, V165)] {
        let dir = at(&release(STANDALONE, dir));
        codex_v0_166_0::windows_release(&mut windows.tree, &dir, version, windows_target);
    }
    windows
        .select(STANDALONE, &selected)
        .history(STANDALONE, &selected, &rollback)
        .tree
        .file(&at(TUI_LOG), b"old\n")
        .file(&at(CATALOG), b"{}");
    windows.platform = Platform::Windows;

    for (scene, active, inactive) in [
        (mac, ACTIVE.to_owned(), ROLLBACK.to_owned()),
        (
            windows,
            release(STANDALONE, &selected),
            release(STANDALONE, &rollback),
        ),
    ] {
        let scan = scene.scan();

        assert_eq!(scan.owner(), OwnerStatus::Supported);
        assert_eq!(
            scan.get(&active).map(|f| &f.path),
            None,
            "selected stays out"
        );
        assert_eq!(scan.rule(&inactive), "release-inventory");
        assert!(has_signature(&scan, &inactive), "expected a signed release");
        assert_eq!(scan.rule(TUI_LOG), "store-inventory");
        assert_eq!(scan.rule("cache/remote_plugin_catalog"), "store-inventory");
        for finding in &scan.report.findings {
            let proposal = &finding.facts.as_ref().unwrap().proposal;
            assert_eq!(*proposal, Proposal::Report, "{finding:?}");
        }
    }
}

/// Catches `current` being trusted without proof it selects a release of this store, or a
/// selection that can't be proved being read as no group at all: its releases stay findings,
/// each blocked, so nothing in the group can be settled apart.
#[test]
fn selection_must_land_on_a_signed_release() {
    let current = at("packages/standalone/current");
    let base = || {
        let mut scene = Scene::new();
        scene.releases(STANDALONE, &[V165, V166]);
        scene
    };
    let missing = base();
    let mut dangling = base();
    dangling.tree.bad_link(&current);
    let mut outside = base();
    outside.tree.dir("elsewhere");
    outside
        .tree
        .link(&current, "elsewhere", Holds::AbsoluteInTree);
    let mut directory = base();
    directory.tree.dir(&current);
    let mut file = base();
    file.tree.file(&current, b"");
    let mut unsigned = base();
    let kept = at(&release(STANDALONE, "backup-0.160"));
    unsigned.tree.dir(&kept);
    unsigned.tree.link(&current, &kept, Holds::AbsoluteInTree);
    let mut climbing = base();
    let selected = at(&release(STANDALONE, &name(V166)));
    climbing
        .tree
        .link(&current, &selected, Holds::AbsoluteInTree);
    let through = at(&format!("packages/standalone/x/../releases/{}", name(V166)));
    climbing.stored.push((current.clone(), through));

    for (case, scene) in [
        ("missing", missing),
        ("dangling", dangling),
        ("outside", outside),
        ("a directory", directory),
        ("a file", file),
        ("an unsigned entry", unsigned),
        ("climbing", climbing),
    ] {
        let scan = scene.scan();

        assert_eq!(scan.rule(ROLLBACK), "stale-releases", "{case}");
        assert_eq!(
            scan.release(ROLLBACK).state,
            blocked(GroupGap::Selection),
            "{case}"
        );
        assert_eq!(scan.value(ROLLBACK, "layout"), Some(FactValue::Unknown));
        assert_eq!(scan.owner(), OwnerStatus::Unsupported, "{case}");
    }
}

/// Catches a link judged by how it is written: only where it lands counts, and a relative
/// one that lands in `releases/` is what upstream's check accepts.
#[test]
fn relative_current_landing_in_releases_is_valid() {
    let mut scene = Scene::new();
    scene.releases(STANDALONE, &[V165, V166]).history(
        STANDALONE,
        "releases/0.166.0-x86_64-unknown-linux-musl",
        "",
    );
    let target = format!("releases/{}", name(V166));
    scene.tree.link(
        &at("packages/standalone/current"),
        &target,
        Holds::AsWritten,
    );

    let scan = scene.scan();

    assert_eq!(scan.release(ACTIVE).role, ReleaseRole::Active);
    assert_eq!(scan.value(ACTIVE, "layout"), Some(SUPPORTED));
}

/// Catches a read that failed passing for an empty listing or an absent file, which is how a
/// release with a running process could look unused. Each part leaves its group blocked and
/// nothing claims the inventory complete.
#[test]
fn an_unreadable_part_never_reads_as_empty() {
    let parts = [
        "packages/standalone/releases",
        "packages/standalone/current",
        "packages/standalone/selection-history",
        "packages/standalone/releases/0.164.0-x86_64-unknown-linux-musl/codex-package.json",
        "packages/standalone/releases/0.164.0-x86_64-unknown-linux-musl/bin",
        "packages/standalone/releases/0.164.0-x86_64-unknown-linux-musl/codex",
    ];
    for part in parts {
        let mut scene = verified();
        scene.denied.push(at(part));

        let scan = scene.scan();

        let row = scan.rows();
        assert!(
            !row.iter()
                .any(|row| row.contains("stale-") && row.contains(part)),
            "{part}: {row:#?}"
        );
        let held = row
            .iter()
            .filter(|row| row.starts_with("stale-"))
            .map(|row| row.trim_start_matches("stale-releases "));
        for release in held {
            assert_eq!(
                scan.release(release).state,
                blocked(GroupGap::Unreadable),
                "{part} at {release}"
            );
        }
    }
    let mut whole_component = verified();
    whole_component.denied.push(at("packages/standalone"));
    assert_eq!(whole_component.scan().owner(), OwnerStatus::Unsupported);
    let mut inventory = verified();
    inventory
        .denied
        .push(at("packages/standalone/releases/.retired.x"));
    inventory
        .tree
        .dir(&at("packages/standalone/releases/.retired.x"));
    let scan = inventory.scan();
    assert_eq!(scan.release(OLD).state, blocked(GroupGap::Unreadable));
    assert_eq!(scan.value(OLD, "inventory"), Some(FactValue::Unknown));
}

/// Catches an entry of `releases/` that couldn't be read leaving the home open: it may be a
/// layout nobody verified, so what rests on the layout can't stand.
#[test]
fn an_unread_entry_of_releases_closes_the_home() {
    let unread = [OLD, "packages/standalone/releases/.retired.x"];
    for part in unread {
        let mut scene = verified_with_files();
        scene
            .tree
            .dir(&at("packages/standalone/releases/.retired.x"));
        scene.denied.push(at(part));

        let scan = scene.scan();

        assert_eq!(scan.owner(), OwnerStatus::Unsupported, "{part}");
        assert_eq!(
            scan.value(TUI_LOG, "layout"),
            Some(FactValue::Unknown),
            "{part}"
        );
        assert_eq!(
            scan.value(CATALOG, "layout"),
            Some(FactValue::Unknown),
            "{part}"
        );
    }
}

/// Catches the home's own listing failing quietly, which would read as a home with nothing
/// in it and leave the owner supported.
#[test]
fn unreadable_home_is_an_omission() {
    let mut scene = verified_with_files();
    scene.denied.push(HOME.to_owned());

    let scan = scene.scan();

    assert_eq!(scan.rows(), Vec::<String>::new());
    assert_eq!(scan.owner(), OwnerStatus::Unsupported);
    assert_ne!(scan.omissions().len(), 0, "expected a recorded omission");
}

/// Catches a directory that can't be listed being silently skipped: what is unknown is shown.
#[test]
fn unreadable_cache_and_log_are_reported() {
    for dir in ["cache", "log", "cache/remote_plugin_catalog"] {
        let mut scene = verified_with_files();
        scene.denied.push(at(dir));

        let scan = scene.scan();

        assert_eq!(scan.rule(dir), "store-inventory", "{dir}");
        assert_eq!(scan.finding(dir).evidence, [Evidence::NoSignature], "{dir}");
    }
}

/// Catches advice that repeats what S5 found wrong: `max_bytes = 0` doesn't mean "keep
/// nothing", and the cap doesn't reach sessions.
#[test]
fn advice_caps_the_history_only_on_a_verified_home() {
    let scan = verified().scan();

    let [advice] = scan.advice() else {
        panic!("expected one piece of advice, got {:?}", scan.advice());
    };
    assert_eq!(advice.snippets, ["[history]\nmax_bytes = 104857600"]);
    assert!(
        advice.text.contains("Don't set it to 0"),
        "expected the warning about 0, got {}",
        advice.text
    );
    assert!(
        advice.text.contains("sessions/"),
        "expected what it doesn't cover, got {}",
        advice.text
    );
}

/// Catches `git` being kept out of a verified home's worktrees, and anyone else getting in.
#[test]
fn worktrees_open_to_git_alone() {
    let path = "worktrees/abcd/repo";
    let mut scene = verified();
    scene.tree.dir(&at(path));

    let scan = scene.scan();

    assert!(scan.closed(path), "expected Codex itself to be kept out");
    let owners = scan.owners_with(IntegrationId::GIT);
    assert!(
        !scan.closes(path, IntegrationId::GIT, &owners),
        "expected git to be let in"
    );
}

/// The demo's home is a verified store with one release to report, so what the docs show is
/// what a verified store looks like. Catches the fixture and the integration drifting apart.
#[test]
fn demo_home_is_a_verified_store() {
    let mut scene = Scene::new();
    codex_demo::demo(&mut scene.tree);
    let built = scene.build();
    let root = built.home();
    let root = root.parent().and_then(|home| home.parent()).unwrap();
    let stamp = |_: &std::path::Path| Ok(built.stamp(STANDALONE));
    codex_demo::selection_history(root, stamp).unwrap();

    let scan = built.scan();

    assert_eq!(scan.owner(), OwnerStatus::Supported);
    assert_eq!(scan.release(OLD).role, ReleaseRole::Installed);
    assert_eq!(scan.release(OLD).state, ReleaseState::Sound);
    assert_eq!(scan.rule(TUI_LOG), "legacy-tui-log");
    assert_eq!(scan.rule(CATALOG), "plugin-catalog-cache");
}

/// A sequence of numbers fixed by its seed, so a failure can be run again.
struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).unwrap()
    }

    /// `bytes` with up to three edits of the kind that turn a valid document into a near miss.
    fn mutate(&mut self, bytes: &[u8]) -> Vec<u8> {
        const NOISE: &[u8] = b"\n\n//..0123\"{}[],:\xff\x00 -";
        let mut out = bytes.to_vec();
        for _ in 0..=self.next() % 3 {
            let at = self.next() % (out.len() + 1);
            let byte = NOISE[self.next() % NOISE.len()];
            match self.next() % 4 {
                0 => out.insert(at, byte),
                1 if at < out.len() => {
                    out.remove(at);
                }
                2 if at < out.len() => out[at] = byte,
                _ => out.truncate(at),
            }
        }
        out
    }
}

/// What a history says, by S5's definition, when it is valid for `selected` and `stamp`.
fn valid_previous(bytes: &[u8], selected: &str, stamp: &str) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let [first, previous, last] = lines.as_slice() else {
        return None;
    };
    let previous = previous.strip_suffix('\n')?;
    let usable = !previous.contains('/') && previous != "." && previous != "..";
    (*first == format!("{selected}\n") && *last == format!("{stamp}\n") && usable)
        .then(|| previous.to_owned())
}

/// Catches a near-miss history passing, a valid one refused, or a parser that panics on what
/// a crash leaves: `Sound` must mean exactly what upstream's check means.
#[test]
fn selection_history_survives_generated_input() {
    let built = verified().build();
    let selected = name(V166);
    let stamp = built.stamp(STANDALONE);
    let valid = format!("{selected}\n{}\n{stamp}\n", name(V165)).into_bytes();
    let history = built.home().join("packages/standalone/selection-history");
    let mut seeded = Seeded(0x00C0_DE55);
    let (mut sound, mut gaps) = (0, 0);
    for _ in 0..300 {
        let bytes = seeded.mutate(&valid);
        fs::write(&history, &bytes).unwrap();

        let scan = built.scan();

        let facts = scan.release(ROLLBACK);
        if let Some(previous) = valid_previous(&bytes, &selected, &stamp) {
            sound += 1;
            assert_eq!(facts.state, ReleaseState::Sound, "{bytes:?}");
            let expected = if previous == name(V165) {
                ReleaseRole::Previous
            } else {
                ReleaseRole::Installed
            };
            assert_eq!(facts.role, expected, "{bytes:?}");
        } else {
            gaps += 1;
            assert!(
                matches!(
                    facts.state,
                    ReleaseState::Blocked(GroupGap::HistoryMalformed | GroupGap::HistoryStale)
                ),
                "expected a gap for {bytes:?}, got {:?}",
                facts.state
            );
        }
    }
    assert_ne!(gaps, 0, "the generator must reach invalid input");
    assert_ne!(sound, 0, "the generator must reach valid input");
}

/// Whether `bytes` is a manifest for `version`, by S5's signature.
fn valid_manifest(bytes: &[u8], version: &str) -> bool {
    let Ok(document) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    document["layoutVersion"].as_u64() == Some(1)
        && document["version"].as_str() == Some(version)
        && document["target"].as_str() == Some(MUSL)
        && document["entrypoint"].as_str() == Some("bin/codex")
}

/// Catches a near-miss manifest signing a release, or a parser that panics on a file a crash
/// cut short: only a document stating exactly what the name says makes it a release.
#[test]
fn manifest_survives_generated_input() {
    let built = verified().build();
    let valid = codex_v0_166_0::manifest(V164, MUSL, "bin/codex").into_bytes();
    let manifest = built.home().join(OLD).join("codex-package.json");
    let mut seeded = Seeded(0x0BAD_F00D);
    let (mut signed, mut kept) = (0, 0);
    for _ in 0..300 {
        let bytes = seeded.mutate(&valid);
        fs::write(&manifest, &bytes).unwrap();

        let scan = built.scan();

        if valid_manifest(&bytes, V164) {
            signed += 1;
            assert_eq!(scan.rule(OLD), "stale-releases", "{bytes:?}");
        } else {
            kept += 1;
            assert_eq!(scan.rule(OLD), "release-inventory", "{bytes:?}");
        }
        assert_eq!(scan.release(ACTIVE).state, ReleaseState::Sound, "{bytes:?}");
    }
    assert_ne!(signed, 0, "the generator must reach valid input");
    assert_ne!(kept, 0, "the generator must reach invalid input");
}
