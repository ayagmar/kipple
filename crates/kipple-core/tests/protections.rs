//! Which state roots a finding gets through (`docs/04-safety-model.md` §3.9): closed by
//! default, and every declaration that reaches a path must consent.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Path, PathBuf};

use kipple_core::{
    Area, FileIdentity, HeldPath, IntegrationDescriptor, IntegrationId, Lens, Lineage, OpenArea,
    OwnerStatus, Owners, Platform, ProtectedEntry, Protection, Protections, ResolvedRoot,
    RootSource, Roots, StateDeclaration, SymbolicRoot, Unresolved, UnresolvedRoot,
};

const CODEX: IntegrationId = IntegrationId("codex");
const CURSOR: IntegrationId = IntegrationId("cursor");
const GIT: IntegrationId = IntegrationId("git");
const NPM: IntegrationId = IntegrationId("npm");
const CARGO: IntegrationId = IntegrationId("cargo");

fn abs(path: &str) -> PathBuf {
    if Path::new("/").is_absolute() {
        PathBuf::from(path)
    } else {
        PathBuf::from(format!("C:{path}"))
    }
}

fn identity(path: &Path) -> FileIdentity {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    FileIdentity {
        volume: 1,
        file: u128::from(hasher.finish()),
    }
}

/// The identities of `path` and each directory above it, as a host with no links has.
fn lineage(path: &Path) -> Lineage {
    Lineage(path.ancestors().map(identity).map(Some).collect())
}

fn held(path: &Path) -> HeldPath {
    HeldPath {
        forms: vec![path.to_owned()],
        lineage: lineage(path),
        note: None,
    }
}

const fn descriptor(
    id: IntegrationId,
    state: &'static [StateDeclaration],
) -> IntegrationDescriptor {
    IntegrationDescriptor {
        id,
        lens: Lens::Dev,
        platforms: Platform::ALL,
        roots: &[],
        selectors: &[],
        artifacts: &[],
        owner_files: &[],
        verified: &[],
        consequences: &[],
        facts: &[],
        state,
        evidences: &[],
        revision: 1,
    }
}

/// A root that is closed whole, as an unlisted part of any state root is.
const fn closed(root: SymbolicRoot, unresolved: Unresolved) -> StateDeclaration {
    StateDeclaration {
        root,
        protected: &[],
        open: &[],
        delegates: &[],
        unresolved,
    }
}

/// A root open to git in `worktrees/`, as Codex's is.
const CODEX_HOME: StateDeclaration = StateDeclaration {
    open: &[OpenArea {
        at: Area::Inside("worktrees"),
        integrations: &[GIT, IntegrationId::GENERIC],
    }],
    protected: &[
        ProtectedEntry::Exact("worktrees/keep"),
        ProtectedEntry::Exact("worktrees/deep/inside/keep"),
        ProtectedEntry::Prefix("worktrees/.staging."),
    ],
    ..closed(SymbolicRoot::CodexHome, Unresolved::HoldAll)
};

const NPM_CACHE: StateDeclaration = StateDeclaration {
    open: &[OpenArea {
        at: Area::Inside("_cacache"),
        integrations: &[NPM],
    }],
    ..closed(SymbolicRoot::NpmCache, Unresolved::HoldOwn)
};

/// A host with these roots, every path as `held` reads it.
fn host(places: &[(SymbolicRoot, &str)], descriptors: &[IntegrationDescriptor]) -> Protections {
    let mut roots = Roots::default();
    let home = abs("/home/u");
    roots.insert(
        SymbolicRoot::Home,
        Ok(ResolvedRoot::new(
            SymbolicRoot::Home,
            home,
            RootSource::Fixed,
        )),
    );
    for (root, path) in places {
        let resolved = ResolvedRoot::new(*root, abs(path), RootSource::Fixed);
        roots.insert(*root, Ok(resolved));
    }
    let data = abs("/home/u/.local/share/kipple");
    Protections::of_host(&roots, Some(&data), descriptors, &held)
}

fn supported(integrations: &[IntegrationId]) -> Owners {
    let status = |id: &IntegrationId| (*id, OwnerStatus::Supported);
    Owners(integrations.iter().map(status).collect())
}

/// What protects a finding of `integration` at `path` on a host with no links.
fn check(
    protections: &Protections,
    path: &str,
    integration: IntegrationId,
    owners: &Owners,
) -> Option<Protection> {
    let path = abs(path);
    protections.check(&path, &lineage(&path), integration, owners)
}

const SUBRESOURCE: Option<Protection> = Some(Protection::Subresource);

fn codex_host() -> Protections {
    host(
        &[(SymbolicRoot::CodexHome, "/home/u/.codex")],
        &[descriptor(CODEX, &[CODEX_HOME])],
    )
}

/// Catches a third-party rule getting into a tool's state because a declaration listed
/// the owner of third-party rules, and a first-party one the area doesn't list.
#[test]
fn third_party_finding_never_enters_an_open_area() {
    let protections = codex_host();
    let owners = supported(&[CODEX, GIT]);
    let inside = "/home/u/.codex/worktrees/a1";

    assert_eq!(check(&protections, inside, GIT, &owners), None);
    assert_eq!(
        check(&protections, inside, IntegrationId::GENERIC, &owners),
        SUBRESOURCE
    );
    assert_eq!(check(&protections, inside, CARGO, &owners), SUBRESOURCE);
}

/// Catches an open area beating what the tool protects inside it, at the entry, inside it,
/// above it, under another case, or by a prefix of its name.
#[test]
fn protected_entry_inside_an_open_area_is_refused() {
    let protections = codex_host();
    let owners = supported(&[CODEX, GIT]);
    let refused = [
        "/home/u/.codex/worktrees/keep",
        "/home/u/.codex/worktrees/keep/x",
        "/home/u/.codex/worktrees/Keep",
        "/home/u/.codex/worktrees/deep",
        "/home/u/.codex/worktrees/deep/inside",
        "/home/u/.codex/worktrees/.staging.8f3",
        "/home/u/.codex/worktrees/.staging.8f3/x",
    ];

    for path in refused {
        assert_eq!(
            check(&protections, path, GIT, &owners),
            SUBRESOURCE,
            "{path}"
        );
    }
    assert_eq!(
        check(
            &protections,
            "/home/u/.codex/worktrees/deep/other",
            GIT,
            &owners
        ),
        None
    );
}

/// Catches an area matching by a different case, which can be another directory.
#[test]
fn an_open_area_is_matched_exactly() {
    let protections = codex_host();
    let owners = supported(&[CODEX, GIT]);

    assert_eq!(
        check(&protections, "/home/u/.codex/Worktrees/a1", GIT, &owners),
        SUBRESOURCE
    );
}

/// Catches `Inside` opening the directory itself, which holds everything in it.
#[test]
fn the_directory_of_an_inside_area_stays_closed() {
    let protections = codex_host();
    let owners = supported(&[CODEX, GIT]);

    assert_eq!(
        check(&protections, "/home/u/.codex/worktrees", GIT, &owners),
        SUBRESOURCE
    );
}

/// Catches a root that holds only while its integration ran.
#[test]
fn subresource_held_when_integration_absent() {
    let protections = codex_host();
    let no_scan_of_it = Owners::default();

    assert_eq!(
        check(
            &protections,
            "/home/u/.codex/worktrees/a1",
            GIT,
            &no_scan_of_it
        ),
        SUBRESOURCE
    );
    assert_eq!(
        check(
            &protections,
            "/home/u/.codex/auth.json",
            GIT,
            &no_scan_of_it
        ),
        SUBRESOURCE
    );
}

/// Catches an unsupported tool's root staying open to generic Git, which is the case 04
/// §3.9 exists for.
#[test]
fn unsupported_version_keeps_state_root_closed() {
    let protections = codex_host();
    let path = "/home/u/.codex/worktrees/a1";
    let unsupported = Owners(BTreeMap::from([
        (CODEX, OwnerStatus::Unsupported),
        (GIT, OwnerStatus::Supported),
    ]));

    assert_eq!(check(&protections, path, GIT, &unsupported), SUBRESOURCE);
    assert_eq!(
        check(&protections, path, GIT, &supported(&[CODEX, GIT])),
        None
    );
}

/// Catches one owner's consent standing for another's: `npm.cache` set inside Cursor's
/// application data stays Cursor's to close.
#[test]
fn overlapping_owner_keeps_its_root_closed() {
    const CURSOR_DATA: StateDeclaration = closed(SymbolicRoot::CursorAppData, Unresolved::HoldOwn);
    let protections = host(
        &[
            (SymbolicRoot::CursorAppData, "/home/u/.config/Cursor"),
            (SymbolicRoot::NpmCache, "/home/u/.config/Cursor/npm"),
        ],
        &[
            descriptor(CURSOR, &[CURSOR_DATA]),
            descriptor(NPM, &[NPM_CACHE]),
        ],
    );
    let owners = supported(&[CURSOR, NPM]);

    assert_eq!(
        check(
            &protections,
            "/home/u/.config/Cursor/npm/_cacache/x",
            NPM,
            &owners
        ),
        SUBRESOURCE
    );
}

/// Catches a root reached through a link getting past its owner: the path says one thing,
/// and the identity says it is the other root too.
#[test]
fn linked_root_alias_needs_every_owner() {
    const SHARED: StateDeclaration = StateDeclaration {
        open: &[OpenArea {
            at: Area::Inside("worktrees"),
            integrations: &[GIT],
        }],
        ..closed(SymbolicRoot::CursorConfig, Unresolved::HoldOwn)
    };
    let protections = host(
        &[
            (SymbolicRoot::CodexHome, "/home/u/.codex"),
            (SymbolicRoot::CursorConfig, "/mnt/shared"),
        ],
        &[
            descriptor(CODEX, &[CODEX_HOME]),
            descriptor(CURSOR, &[SHARED]),
        ],
    );
    // `/home/u/.codex` is a link to `/mnt/shared`: what the scan read below it has the
    // shared directory's identities.
    let path = abs("/home/u/.codex/worktrees/a1");
    let real = abs("/mnt/shared/worktrees/a1");
    let through_the_link = lineage(&real);

    let with = |owners: &[IntegrationId]| {
        protections.check(&path, &through_the_link, GIT, &supported(owners))
    };

    assert_eq!(with(&[CODEX, GIT]), SUBRESOURCE);
    assert_eq!(with(&[CODEX, CURSOR, GIT]), None);
}

const PI: IntegrationId = IntegrationId("pi");
const PI_AGENT: StateDeclaration = StateDeclaration {
    open: &[OpenArea {
        at: Area::Inside("sessions"),
        integrations: &[PI],
    }],
    ..closed(SymbolicRoot::PiAgent, Unresolved::HoldAll)
};
const PI_HOME: StateDeclaration = StateDeclaration {
    delegates: &[SymbolicRoot::PiAgent],
    ..closed(SymbolicRoot::PiHome, Unresolved::HoldOwn)
};

/// Catches a parent standing aside for another integration's root, or a delegation
/// opening what the child keeps closed.
#[test]
fn delegation_needs_the_same_owner_and_the_child_to_consent() {
    let places = [
        (SymbolicRoot::PiHome, "/home/u/.pi"),
        (SymbolicRoot::PiAgent, "/home/u/.pi/agent"),
    ];
    let same_owner = host(&places, &[descriptor(PI, &[PI_HOME, PI_AGENT])]);
    let other_owner = host(
        &places,
        &[descriptor(PI, &[PI_HOME]), descriptor(CURSOR, &[PI_AGENT])],
    );
    let owners = supported(&[PI, CURSOR]);
    let session = "/home/u/.pi/agent/sessions/a.jsonl";

    assert_eq!(check(&same_owner, session, PI, &owners), None);
    assert_eq!(
        check(&same_owner, "/home/u/.pi/agent/auth.json", PI, &owners),
        SUBRESOURCE
    );
    assert_eq!(
        check(&same_owner, "/home/u/.pi/other", PI, &owners),
        SUBRESOURCE
    );
    assert_eq!(check(&other_owner, session, PI, &owners), SUBRESOURCE);
}

/// Catches a slip in a descriptor opening a store of credentials to everyone: roots that
/// stand aside for each other, or for themselves, leave nobody to say no.
#[test]
fn a_delegation_cycle_opens_nothing() {
    const FIRST: StateDeclaration = StateDeclaration {
        delegates: &[SymbolicRoot::PiAgent],
        ..closed(SymbolicRoot::PiHome, Unresolved::HoldAll)
    };
    const SECOND: StateDeclaration = StateDeclaration {
        delegates: &[SymbolicRoot::PiHome],
        ..closed(SymbolicRoot::PiAgent, Unresolved::HoldAll)
    };
    const ITSELF: StateDeclaration = StateDeclaration {
        delegates: &[SymbolicRoot::PiHome],
        ..closed(SymbolicRoot::PiHome, Unresolved::HoldAll)
    };
    let both = [
        (SymbolicRoot::PiHome, "/home/u/.pi"),
        (SymbolicRoot::PiAgent, "/home/u/.pi/agent"),
    ];
    let cycle = host(&both, &[descriptor(PI, &[FIRST, SECOND])]);
    let alone = host(&both[..1], &[descriptor(PI, &[ITSELF])]);
    let owners = supported(&[PI]);
    let secret = "/home/u/.pi/agent/auth.json";

    for integration in [PI, IntegrationId::GENERIC] {
        assert_eq!(check(&cycle, secret, integration, &owners), SUBRESOURCE);
        assert_eq!(check(&alone, secret, integration, &owners), SUBRESOURCE);
    }
}

/// Catches a directory above a state root being removed, root and all, because it isn't
/// inside anything closed.
#[test]
fn parent_selection_above_a_state_root_is_refused() {
    const EVERYTHING: StateDeclaration = StateDeclaration {
        open: &[OpenArea {
            at: Area::Inside(""),
            integrations: &[GIT],
        }],
        ..closed(SymbolicRoot::CodexHome, Unresolved::HoldAll)
    };
    let protections = host(
        &[(SymbolicRoot::CodexHome, "/home/u/tools/codex")],
        &[descriptor(CODEX, &[EVERYTHING])],
    );
    let owners = supported(&[CODEX, GIT]);

    assert_eq!(
        check(&protections, "/home/u/tools", GIT, &owners),
        SUBRESOURCE
    );
    assert_eq!(
        check(&protections, "/home/u/tools/codex", GIT, &owners),
        SUBRESOURCE,
        "the root itself isn't strictly inside it"
    );
    assert_eq!(
        check(&protections, "/home/u/tools/codex/x", GIT, &owners),
        None
    );
}

/// Catches a root with no path holding nothing, or holding everything when only its owner
/// was meant to stop.
#[test]
fn unresolved_npm_root_blocks_only_npm() {
    let mut roots = Roots::default();
    let home = ResolvedRoot::new(SymbolicRoot::Home, abs("/home/u"), RootSource::Fixed);
    roots.insert(SymbolicRoot::Home, Ok(home));
    let unset = UnresolvedRoot::NotSet {
        var: "npm_config_cache",
    };
    roots.insert(SymbolicRoot::NpmCache, Err(unset));
    let data = abs("/home/u/.local/share/kipple");
    let protections =
        Protections::of_host(&roots, Some(&data), &[descriptor(NPM, &[NPM_CACHE])], &held);
    let owners = supported(&[NPM, CARGO]);

    assert_eq!(check(&protections, "/home/u/x", NPM, &owners), SUBRESOURCE);
    assert_eq!(check(&protections, "/home/u/x", CARGO, &owners), None);
}

/// Catches an override leaving the default store open, where the tool's credentials or
/// history may still be: the child moved, so the parent answers.
#[test]
fn default_store_stays_closed_under_an_override() {
    const CLAUDE: IntegrationId = IntegrationId("claude");
    const OPEN: &[OpenArea] = &[OpenArea {
        at: Area::Inside("open"),
        integrations: &[CLAUDE, CODEX, NPM, PI],
    }];
    const fn pair(default: SymbolicRoot, moved: &'static [SymbolicRoot]) -> StateDeclaration {
        StateDeclaration {
            delegates: moved,
            ..closed(default, Unresolved::HoldOwn)
        }
    }
    const fn moved(root: SymbolicRoot) -> StateDeclaration {
        StateDeclaration {
            open: OPEN,
            ..closed(root, Unresolved::HoldAll)
        }
    }
    const CLAUDE_STATE: [StateDeclaration; 2] = [
        pair(SymbolicRoot::ClaudeHome, &[SymbolicRoot::ClaudeConfig]),
        moved(SymbolicRoot::ClaudeConfig),
    ];
    const CODEX_STATE: [StateDeclaration; 2] = [
        pair(SymbolicRoot::CodexDefaultHome, &[SymbolicRoot::CodexHome]),
        moved(SymbolicRoot::CodexHome),
    ];
    const NPM_STATE: [StateDeclaration; 2] = [
        pair(SymbolicRoot::NpmDefaultCache, &[SymbolicRoot::NpmCache]),
        moved(SymbolicRoot::NpmCache),
    ];
    const PI_STATE: [StateDeclaration; 2] = [
        pair(SymbolicRoot::PiHome, &[SymbolicRoot::PiAgent]),
        moved(SymbolicRoot::PiAgent),
    ];
    let cases = [
        (CLAUDE, &CLAUDE_STATE),
        (CODEX, &CODEX_STATE),
        (NPM, &NPM_STATE),
        (PI, &PI_STATE),
    ];
    for (owner, state) in cases {
        let [default, overridden] = state;
        let protections = host(
            &[
                (default.root, "/home/u/.default"),
                (overridden.root, "/srv/moved"),
            ],
            &[descriptor(owner, state)],
        );
        let owners = supported(&[owner]);

        assert_eq!(
            check(&protections, "/home/u/.default/open/x", owner, &owners),
            SUBRESOURCE,
            "{owner:?}"
        );
        assert_eq!(
            check(&protections, "/srv/moved/open/x", owner, &owners),
            None
        );

        let unmoved = host(
            &[
                (default.root, "/home/u/.default"),
                (overridden.root, "/home/u/.default"),
            ],
            &[descriptor(owner, state)],
        );
        assert_eq!(
            check(&unmoved, "/home/u/.default/open/x", owner, &owners),
            None
        );
    }
}

/// Catches a finding whose ancestry couldn't be read getting through: it might be anywhere
/// in the root.
#[test]
fn a_finding_with_an_unknown_ancestry_gets_no_consent() {
    let protections = codex_host();
    let owners = supported(&[CODEX, GIT]);
    let path = abs("/home/u/.codex/worktrees/a1");
    let mut unknown = lineage(&path);
    unknown.0[1] = None;

    assert_eq!(
        protections.check(&path, &unknown, GIT, &owners),
        SUBRESOURCE
    );
}
