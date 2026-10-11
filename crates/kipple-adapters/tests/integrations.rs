//! The built-in integrations against upstream layouts built with `FixtureTree`
//! (`docs/06-adapters-and-rules.md` §5 to §7): each supported layout is found with its
//! evidence, nothing is looked for inside an artifact, and an unknown layout says so.

#[path = "fixtures/tree.rs"]
mod tree;

#[path = "fixtures/node/bun_v1_4_2.rs"]
mod bun_v1_4_2;
#[path = "fixtures/cargo/v1_99_0.rs"]
mod cargo_v1_99_0;
#[path = "fixtures/claude-code/settings.rs"]
mod claude_settings;
#[path = "fixtures/demo_home.rs"]
mod demo_home;
#[path = "fixtures/journald/v262.rs"]
mod journald_v262;
#[path = "fixtures/node/npm_v11_19_0.rs"]
mod npm_v11_19_0;
#[path = "fixtures/pacman/v7_1_0.rs"]
mod pacman_v7_1_0;
#[path = "fixtures/node/pnpm_v12_9_1.rs"]
mod pnpm_v12_9_1;
#[path = "fixtures/trash/freedesktop_v1_0.rs"]
mod trash_v1_0;
#[path = "fixtures/python/uv_v0_12_23.rs"]
mod uv_v0_12_23;
#[path = "fixtures/python/venv_v3_14_8.rs"]
mod venv_v3_14_8;

use std::fs;
use std::io::{self, Read as _};
use std::path::Path;

use kipple_adapters::builtin;
use kipple_core::{
    Advice, Apparent, CancelToken, Completeness, DirEntryMeta, Engine, EntryKind, EntryMeta,
    Evidence, FsProbe, ProbeError, ResolvedRoot, RootSource, Roots, ScanReport, ScanRequest, Sizer,
    SpaceEstimate, SymbolicRoot, event_channel,
};
use tree::FixtureTree;

/// The real file system, read without following links: enough to run the integrations
/// over a fixture in a temp dir.
#[derive(Debug)]
struct TempFs;

fn kind(file_type: fs::FileType) -> EntryKind {
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

impl FsProbe for TempFs {
    fn metadata(&self, path: &Path) -> Result<EntryMeta, ProbeError> {
        let meta = fs::symlink_metadata(path).map_err(|e| ProbeError::io(path, e))?;
        Ok(EntryMeta {
            kind: kind(meta.file_type()),
            identity: None,
        })
    }

    fn read_dir(&self, dir: &Path) -> Result<Vec<DirEntryMeta>, ProbeError> {
        let read = || -> io::Result<Vec<DirEntryMeta>> {
            fs::read_dir(dir)?
                .map(|entry| {
                    let entry = entry?;
                    Ok(DirEntryMeta {
                        name: entry.file_name(),
                        kind: kind(entry.file_type()?),
                    })
                })
                .collect()
        };
        read().map_err(|e| ProbeError::io(dir, e))
    }

    fn open_dir(&self, dir: &Path) -> Result<(), ProbeError> {
        fs::read_dir(dir)
            .map(drop)
            .map_err(|e| ProbeError::io(dir, e))
    }

    fn read_file(&self, path: &Path, limit: u64) -> Result<Vec<u8>, ProbeError> {
        let mut bytes = Vec::new();
        fs::File::open(path)
            .and_then(|file| file.take(limit).read_to_end(&mut bytes))
            .map_err(|e| ProbeError::io(path, e))?;
        Ok(bytes)
    }
}

/// Sizes nothing: these tests are about what is found, not how big it is.
#[derive(Debug)]
struct NoSizer;

impl Sizer for NoSizer {
    fn size(&self, _: &Path, _: &CancelToken) -> Result<SpaceEstimate, ProbeError> {
        Ok(SpaceEstimate {
            apparent: Apparent(0),
            allocated: None,
            unique_reclaim: None,
            completeness: Completeness::Complete,
        })
    }
}

/// Builds `tree` in a temp dir and scans it with every built-in integration and rule,
/// each root of `demo_home::ROOTS` placed inside it.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn scan(tree: &FixtureTree) -> (tempfile::TempDir, ScanReport) {
    let temp = tempfile::tempdir().unwrap();
    tree.build_in(temp.path()).unwrap();
    let mut roots = Roots::default();
    for (name, relative) in demo_home::ROOTS {
        let root = SymbolicRoot::from_name(name).unwrap();
        let resolved = ResolvedRoot::new(root, temp.path().join(relative), RootSource::Fixed);
        roots.insert(root, Ok(resolved));
    }
    let packs = builtin::packs(&semver::Version::new(0, 0, 0)).unwrap();
    let rules = packs.into_iter().flat_map(|pack| pack.rules).collect();
    let engine = Engine::new(
        Box::new(TempFs),
        Box::new(NoSizer),
        builtin::integrations(),
        rules,
    )
    .unwrap();
    let cancel = CancelToken::new();
    let (events, rx) = event_channel(4096, &cancel);
    let report = engine.scan(&ScanRequest { roots, lens: None }, &events, &cancel);
    drop(rx);
    (temp, report)
}

/// Each finding as its rule, its path relative to `base` with `/` separators, and its
/// evidence.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn found(base: &Path, report: &ScanReport) -> Vec<(String, String, Vec<Evidence>)> {
    report
        .findings
        .iter()
        .map(|f| {
            let relative = f.path.strip_prefix(base).unwrap();
            let parts: Vec<_> = relative.iter().map(|p| p.to_string_lossy()).collect();
            (f.rule.0.clone(), parts.join("/"), f.evidence.clone())
        })
        .collect()
}

fn advice<'r>(report: &'r ScanReport, integration: &str) -> &'r [Advice] {
    report
        .integrations
        .iter()
        .find(|i| i.id.0 == integration)
        .map_or(&[], |i| i.advice.as_slice())
}

fn marker(name: &str) -> Evidence {
    Evidence::Marker(name.to_owned())
}

fn signature(name: &str) -> Evidence {
    Evidence::Signature(name.to_owned())
}

#[test]
fn every_supported_layout_in_the_demo_home_is_found_with_its_evidence_and_nothing_inside_one() {
    let (temp, report) = scan(&demo_home::tree());

    let mut found = found(temp.path(), &report);
    found.sort();
    let root = |name| vec![Evidence::Root(SymbolicRoot::from_name(name).unwrap())];
    let project = |file: &str, sign: Evidence| vec![marker(file), sign];
    let mut expected = vec![
        (
            "builtin.cargo.target",
            "home/projects/clients/acme/api/target",
            project("Cargo.toml", signature("CACHEDIR.TAG")),
        ),
        (
            "builtin.cargo.target",
            "home/projects/rust-app/target",
            project("Cargo.toml", signature("CACHEDIR.TAG")),
        ),
        (
            "builtin.journald.journal",
            "var/log/journal",
            root("journald.journal"),
        ),
        (
            "builtin.node.node-modules",
            "home/projects/bunapp/node_modules",
            project("package.json", Evidence::NoSignature),
        ),
        (
            "builtin.node.node-modules",
            "home/projects/dash/node_modules",
            project("package.json", signature(".modules.yaml")),
        ),
        (
            "builtin.node.node-modules",
            "home/projects/web/node_modules",
            project("package.json", signature(".package-lock.json")),
        ),
        (
            "builtin.pacman.cache",
            "var/cache/pacman/pkg",
            root("pacman.cache"),
        ),
        (
            "builtin.python.bytecode",
            "home/projects/tool/pkg/__pycache__",
            vec![signature("*.pyc")],
        ),
        (
            "builtin.python.venv",
            "home/projects/tool/.venv",
            project("pyproject.toml", signature("pyvenv.cfg")),
        ),
        (
            "builtin.python.venv",
            "home/projects/uvproj/.venv",
            project("pyproject.toml", signature("pyvenv.cfg")),
        ),
        (
            "builtin.trash.contents",
            "home/.local/share/Trash",
            root("trash"),
        ),
    ]
    .into_iter()
    .map(|(rule, path, evidence)| (rule.to_owned(), path.to_owned(), evidence))
    .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(found, expected);
    assert!(report.integrations.iter().all(|i| i.omissions.is_empty()));
}

#[test]
fn a_target_without_cargos_tag_or_with_a_forged_one_has_an_unknown_layout() {
    let mut tree = FixtureTree::new();
    tree.file("home/projects/a/Cargo.toml", b"")
        .dir("home/projects/a/target/debug")
        .file("home/projects/b/Cargo.toml", b"")
        .file(
            "home/projects/b/target/CACHEDIR.TAG",
            b"Signature: not the one",
        );

    let (temp, report) = scan(&tree);

    let no_layout = vec![marker("Cargo.toml"), Evidence::NoSignature];
    assert_eq!(
        found(temp.path(), &report),
        [
            (
                "builtin.cargo.target".to_owned(),
                "home/projects/a/target".to_owned(),
                no_layout.clone()
            ),
            (
                "builtin.cargo.target".to_owned(),
                "home/projects/b/target".to_owned(),
                no_layout
            ),
        ]
    );
}

#[test]
fn a_pycache_with_anything_but_bytecode_or_inside_any_virtual_environment_is_not_proposed() {
    let mut tree = FixtureTree::new();
    tree.file("home/projects/p/pkg/__pycache__/mod.cpython-314.pyc", b"")
        .file("home/projects/p/pkg/__pycache__/notes.txt", b"mine")
        // An environment with no project beside it is still never looked into.
        .file("home/projects/loose/.venv/pkg/__pycache__/x.cpython-314.pyc", b"");

    let (temp, report) = scan(&tree);

    assert_eq!(found(temp.path(), &report), []);
}

#[test]
fn system_locations_are_advised_on_when_present_and_reported_only_when_not_empty() {
    let mut tree = FixtureTree::new();
    tree.dir("var/cache/pacman/pkg")
        .dir("var/log/journal")
        .dir("home/.local/share/Trash");

    let (temp, report) = scan(&tree);

    assert_eq!(found(temp.path(), &report), []);
    let pacman = advice(&report, "pacman");
    assert!(
        pacman
            .iter()
            .any(|a| a.snippets.contains(&"sudo paccache -rk3".to_owned()))
    );
    assert_eq!(advice(&report, "journald").len(), 1);

    let (_, absent) = scan(&FixtureTree::new());
    assert_eq!(advice(&absent, "pacman"), []);
    assert_eq!(advice(&absent, "claude-code"), []);
}

#[test]
fn claude_code_advice_shows_the_retention_its_settings_resolve_to() {
    let text = |tree: &FixtureTree| {
        let (_, report) = scan(tree);
        let [advice] = advice(&report, "claude-code") else {
            panic!("expected one piece of advice");
        };
        advice.text.clone()
    };
    let mut set = FixtureTree::new();
    claude_settings::config(&mut set, "home/.claude", 14);
    let mut unset = FixtureTree::new();
    unset.file("home/.claude/settings.json", b"{\"model\": \"x\"}");
    let mut empty = FixtureTree::new();
    empty.dir("home/.claude");
    let mut invalid = FixtureTree::new();
    invalid.file("home/.claude/settings.json", b"{\"cleanupPeriodDays\": 0}");

    assert!(text(&set).contains(": 14 days, from "), "{}", text(&set));
    assert!(text(&unset).contains(": 30 days, the default"));
    assert!(text(&empty).contains(": 30 days, the default"));
    assert!(text(&invalid).contains(": unknown: "));
}
