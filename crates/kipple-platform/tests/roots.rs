//! Root resolution from an injected environment, upstream conventions first
//! (`docs/05-architecture.md` §1). Windows resolves Known Folders through the OS, so its
//! roots are covered by the native profile instead. A root whose own path is a link is
//! resolved once (D-041), here in a temp dir.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "a temp dir or path that can't be made fails the test"
)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use kipple_core::{
    FsProbe as _, HeldPath, IntegrationDescriptor, IntegrationId, Lens, Lineage, LinkProblem,
    Owners, Platform, Protections, RootSource, Roots, StateDeclaration, SymbolicRoot, Unresolved,
    UnresolvedRoot,
};
use kipple_platform::{Environment, NoFollowFs, kipple_dirs, resolve_roots};

fn roots(vars: &[(&str, &str)]) -> Roots {
    resolve_roots(&env(vars), &[])
}

/// `vars`, with the system's fixed paths somewhere that doesn't exist, so no test reads
/// this machine's `/etc` or `/var`.
fn env(vars: &[(&str, &str)]) -> Environment {
    Environment::from_vars(
        vars.iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    )
    .with_system_root(PathBuf::from("/nonexistent/kipple-test-system"))
}

fn path(roots: &Roots, root: SymbolicRoot) -> Result<(PathBuf, RootSource), String> {
    match roots.get(root) {
        Some(Ok(resolved)) => Ok((resolved.path.clone(), resolved.source)),
        Some(Err(unresolved)) => Err(unresolved.to_string()),
        None => Err("not on this platform".to_owned()),
    }
}

#[test]
fn a_set_xdg_variable_wins_over_the_default_and_says_so() {
    let roots = roots(&[("HOME", "/home/u"), ("XDG_CACHE_HOME", "/fast/cache")]);

    assert_eq!(
        path(&roots, SymbolicRoot::XdgCache),
        Ok((
            PathBuf::from("/fast/cache"),
            RootSource::Env("XDG_CACHE_HOME")
        ))
    );
    assert_eq!(
        path(&roots, SymbolicRoot::XdgData),
        Ok((
            PathBuf::from("/home/u/.local/share"),
            RootSource::Default {
                base: SymbolicRoot::Home,
                relative: ".local/share",
            }
        ))
    );
    let dirs = kipple_dirs(&roots);
    assert_eq!(
        dirs.cache.path.unwrap(),
        PathBuf::from("/fast/cache/kipple")
    );
    assert_eq!(
        dirs.config.path.unwrap(),
        PathBuf::from("/home/u/.config/kipple")
    );
}

#[test]
fn an_empty_variable_counts_as_unset() {
    let roots = roots(&[("HOME", "/home/u"), ("XDG_STATE_HOME", "")]);

    let (state, _) = path(&roots, SymbolicRoot::XdgState).unwrap();
    assert_eq!(state, PathBuf::from("/home/u/.local/state"));
}

#[test]
fn a_relative_override_leaves_the_root_unknown_instead_of_using_the_default() {
    let roots = roots(&[("HOME", "/home/u"), ("XDG_CACHE_HOME", "cache")]);

    assert!(
        matches!(
            roots.get(SymbolicRoot::XdgCache),
            Some(Err(UnresolvedRoot::NotAbsolute {
                var: "XDG_CACHE_HOME",
                ..
            }))
        ),
        "a relative override leaves the cache root unresolved"
    );
    assert!(
        kipple_dirs(&roots).cache.path.is_err(),
        "a relative cache override leaves kipple's cache directory unknown"
    );
}

#[test]
fn without_home_only_explicitly_set_roots_resolve() {
    let roots = roots(&[("XDG_DATA_HOME", "/data")]);

    assert!(
        matches!(
            roots.get(SymbolicRoot::Home),
            Some(Err(UnresolvedRoot::NotSet { var: "HOME" }))
        ),
        "without HOME the home root is unset"
    );
    assert!(
        matches!(
            roots.get(SymbolicRoot::XdgCache),
            Some(Err(UnresolvedRoot::BaseUnresolved {
                base: SymbolicRoot::Home
            }))
        ),
        "without HOME the cache root's base is unresolved"
    );
    let (data, _) = path(&roots, SymbolicRoot::XdgData).unwrap();
    assert_eq!(data, PathBuf::from("/data"));
}

#[cfg(target_os = "macos")]
#[test]
fn macos_caches_live_under_home() {
    let roots = roots(&[("HOME", "/Users/u")]);

    let (caches, _) = path(&roots, SymbolicRoot::MacosCaches).unwrap();
    assert_eq!(caches, PathBuf::from("/Users/u/Library/Caches"));
}

/// A home directory in a temp dir, with `.cache` as whatever `make_cache` puts there.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn home_with_cache(make_cache: impl FnOnce(&Path, &Path)) -> (tempfile::TempDir, Roots) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    make_cache(temp.path(), &home.join(".cache"));
    let roots = roots(&[("HOME", home.to_str().unwrap())]);
    (temp, roots)
}

#[test]
fn a_root_that_is_a_link_is_read_where_the_link_leads_and_keeps_its_configured_path() {
    let (temp, roots) = home_with_cache(|temp, cache| {
        fs::create_dir_all(temp.join("ssd/cache")).unwrap();
        // A chain, with a relative link at the end, as a move to another disk leaves it.
        symlink("ssd/cache", temp.join("moved")).unwrap();
        symlink("../moved", cache).unwrap();
    });
    let target = fs::canonicalize(temp.path().join("ssd/cache")).unwrap();

    let Some(Ok(cache)) = roots.get(SymbolicRoot::XdgCache) else {
        panic!("{:?}", roots.get(SymbolicRoot::XdgCache));
    };

    assert_eq!(cache.path, target);
    assert_eq!(cache.configured(), &temp.path().join("home/.cache"));
    let identity = NoFollowFs.metadata(&target).unwrap().identity;
    assert_eq!(cache.link.as_ref().map(|link| link.identity), identity);
    assert_eq!(
        kipple_dirs(&roots).cache.path.unwrap(),
        target.join("kipple")
    );
}

#[test]
fn a_root_link_that_leads_nowhere_or_to_a_file_leaves_the_root_unknown() {
    let (_dangling_dir, dangling) = home_with_cache(|temp, cache| {
        symlink(temp.join("unmounted/cache"), cache).unwrap();
    });
    let (_file_dir, to_file) = home_with_cache(|temp, cache| {
        fs::write(temp.join("file"), "").unwrap();
        symlink(temp.join("file"), cache).unwrap();
    });

    assert!(
        matches!(
            dangling.get(SymbolicRoot::XdgCache),
            Some(Err(UnresolvedRoot::Link {
                problem: LinkProblem::Unresolvable(_),
                ..
            }))
        ),
        "a dangling link leaves the cache root unresolved"
    );
    assert!(
        matches!(
            to_file.get(SymbolicRoot::XdgCache),
            Some(Err(UnresolvedRoot::Link {
                problem: LinkProblem::NotDirectory,
                ..
            }))
        ),
        "a link to a file leaves the cache root unresolved"
    );
}

#[test]
fn a_root_that_is_not_a_link_is_left_as_configured() {
    let (temp, roots) = home_with_cache(|_, cache| fs::create_dir(cache).unwrap());

    let Some(Ok(cache)) = roots.get(SymbolicRoot::XdgCache) else {
        panic!("{:?}", roots.get(SymbolicRoot::XdgCache));
    };
    assert_eq!(cache.path, temp.path().join("home/.cache"));
    assert_eq!(cache.link, None);
}

fn project_roots(roots: &Roots) -> Vec<Result<(PathBuf, RootSource), String>> {
    roots
        .all(SymbolicRoot::ProjectRoots)
        .iter()
        .map(|r| match r {
            Ok(resolved) => Ok((resolved.path.clone(), resolved.source)),
            Err(unresolved) => Err(unresolved.to_string()),
        })
        .collect()
}

#[test]
fn the_default_project_roots_are_those_that_exist_and_root_replaces_them() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    fs::create_dir_all(home.join("code")).unwrap();
    fs::create_dir_all(home.join("source/repos")).unwrap();
    // A file is not a project root.
    fs::write(home.join("projects"), "").unwrap();
    let env = env(&[("HOME", home.to_str().unwrap())]);

    let default = |relative| RootSource::Default {
        base: SymbolicRoot::Home,
        relative,
    };
    assert_eq!(
        project_roots(&resolve_roots(&env, &[])),
        [
            Ok((home.join("code"), default("code"))),
            Ok((home.join("source").join("repos"), default("source/repos"))),
        ]
    );

    // The same directory given twice, once through `..`, is one root.
    let given = [
        home.join("code"),
        home.join("code/../code"),
        PathBuf::from("relative"),
        home.join("gone/repo"),
        // A root the user names that doesn't exist is never silently clean.
        home.join("cod"),
    ];
    let replaced = project_roots(&resolve_roots(&env, &given));
    assert_eq!(
        replaced[0],
        Ok((home.join("code"), RootSource::CommandLine))
    );
    assert!(
        replaced[1].as_ref().is_err_and(|e| e.contains("--root")),
        "a root in a missing directory is refused naming --root"
    );
    assert!(
        replaced[2]
            .as_ref()
            .is_err_and(|e| e.contains("can't be resolved")),
        "a root that can't be resolved is refused"
    );
    assert!(
        replaced[3]
            .as_ref()
            .is_err_and(|e| e.contains("cod can't be resolved")),
        "a mistyped root is refused by its name"
    );
    assert_eq!(replaced.len(), 4);
}

#[test]
fn a_default_project_root_that_cant_be_checked_or_is_a_broken_link_is_kept_for_the_scan_to_report()
{
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    fs::create_dir_all(home.join("source/repos")).unwrap();
    symlink(home.join("unmounted"), home.join("work")).unwrap();
    // As root the directory stays searchable, and it is kept just the same.
    fs::set_permissions(home.join("source"), fs::Permissions::from_mode(0o000)).unwrap();

    let roots = resolve_roots(&env(&[("HOME", home.to_str().unwrap())]), &[]);
    fs::set_permissions(home.join("source"), fs::Permissions::from_mode(0o755)).unwrap();

    let found = project_roots(&roots);
    let source = RootSource::Default {
        base: SymbolicRoot::Home,
        relative: "source/repos",
    };
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found[0].as_ref().is_err_and(|e| e.contains("work")),
        "{found:?}"
    );
    assert_eq!(found[1], Ok((home.join("source/repos"), source)));
}

#[test]
fn a_root_given_with_parent_components_is_the_directory_it_names() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    fs::create_dir_all(base.join("a/b")).unwrap();
    let given = [base.join("a/b/.."), base.join("gone/..")];

    let found = project_roots(&resolve_roots(&env(&[]), &given));

    assert_eq!(found[0], Ok((base.join("a"), RootSource::CommandLine)));
    assert!(
        found[1]
            .as_ref()
            .is_err_and(|e| e.contains("can't be resolved")),
        "a root through a missing parent is refused"
    );
}

#[test]
fn a_root_link_given_through_parent_components_keeps_its_link_and_identity() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    fs::create_dir_all(base.join("hop")).unwrap();
    fs::create_dir_all(base.join("ssd/projects")).unwrap();
    symlink(base.join("ssd/projects"), base.join("linked")).unwrap();

    let roots = resolve_roots(&env(&[]), &[base.join("hop/../linked")]);

    let [Ok(root)] = roots.all(SymbolicRoot::ProjectRoots) else {
        panic!("{:?}", roots.all(SymbolicRoot::ProjectRoots));
    };
    assert_eq!(root.path, base.join("ssd/projects"));
    // The engine re-checks this identity before reading the root (D-041).
    let link = root.link.as_ref().unwrap();
    assert_eq!(link.configured, base.join("linked"));
    let identity = NoFollowFs.metadata(&root.path).unwrap().identity;
    assert_eq!(Some(link.identity), identity);
}

#[test]
fn the_same_directory_given_directly_and_through_a_link_keeps_the_links_identity_check() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    fs::create_dir_all(base.join("ssd/projects")).unwrap();
    symlink(base.join("ssd/projects"), base.join("linked")).unwrap();
    let direct = base.join("ssd/projects");
    let linked = base.join("linked");

    for given in [[&direct, &linked], [&linked, &direct]] {
        let given = given.map(Clone::clone);
        let roots = resolve_roots(&env(&[]), &given);

        let [Ok(root)] = roots.all(SymbolicRoot::ProjectRoots) else {
            panic!("{:?}", roots.all(SymbolicRoot::ProjectRoots));
        };
        assert_eq!(root.path, direct);
        // Without it, the root given first would decide whether the scan re-checks it.
        let link = root.link.as_ref().unwrap();
        assert_eq!(link.configured, linked);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn a_pacman_cache_dir_link_spelled_with_parent_components_keeps_its_identity_check() {
    let temp = tempfile::tempdir().unwrap();
    let system = temp.path().canonicalize().unwrap();
    fs::create_dir_all(system.join("etc")).unwrap();
    fs::create_dir_all(system.join("hop")).unwrap();
    fs::create_dir_all(system.join("srv/pkg")).unwrap();
    symlink(system.join("srv/pkg"), system.join("linked")).unwrap();
    fs::write(
        system.join("etc/pacman.conf"),
        "[options]\nCacheDir = /hop/../linked\n",
    )
    .unwrap();
    let env = env(&[("HOME", "/home/u")]).with_system_root(system.clone());

    let roots = resolve_roots(&env, &[]);

    let [Ok(root)] = roots.all(SymbolicRoot::PacmanCache) else {
        panic!("{:?}", roots.all(SymbolicRoot::PacmanCache));
    };
    assert_eq!(root.path, system.join("srv/pkg"));
    let link = root.link.as_ref().unwrap();
    assert_eq!(link.configured, system.join("linked"));
}

#[test]
#[cfg(target_os = "linux")]
fn pacman_and_journal_roots_come_from_the_system_root_never_this_machines_own() {
    let temp = tempfile::tempdir().unwrap();
    let system = temp.path().canonicalize().unwrap();
    fs::create_dir_all(system.join("etc")).unwrap();
    fs::create_dir_all(system.join("srv/pkg")).unwrap();
    fs::create_dir_all(system.join("srv/old")).unwrap();
    fs::write(
        system.join("etc/pacman.conf"),
        "[options]\nCacheDir = /srv/pkg/ /srv/old/../pkg\n",
    )
    .unwrap();
    let env = env(&[("HOME", "/home/u")]).with_system_root(system.clone());

    let roots = resolve_roots(&env, &[]);

    let setting = RootSource::Setting {
        file: "/etc/pacman.conf",
        key: "CacheDir",
    };
    assert_eq!(
        path(&roots, SymbolicRoot::PacmanCache),
        Ok((system.join("srv/pkg"), setting))
    );
    // The second `CacheDir` names the same directory through `..`.
    assert_eq!(roots.all(SymbolicRoot::PacmanCache).len(), 1);
    assert_eq!(
        path(&roots, SymbolicRoot::Journal),
        Ok((system.join("var/log/journal"), RootSource::Fixed))
    );
}

#[test]
fn claude_code_config_follows_claude_config_dir_and_defaults_under_home() {
    let set = roots(&[("HOME", "/home/u"), ("CLAUDE_CONFIG_DIR", "/cfg/claude")]);
    assert_eq!(
        path(&set, SymbolicRoot::ClaudeConfig),
        Ok((
            PathBuf::from("/cfg/claude"),
            RootSource::Env("CLAUDE_CONFIG_DIR")
        ))
    );
    let unset = roots(&[("HOME", "/home/u")]);
    let (default, _) = path(&unset, SymbolicRoot::ClaudeConfig).unwrap();
    assert_eq!(default, PathBuf::from("/home/u/.claude"));
}

#[test]
#[cfg(target_os = "linux")]
fn the_linux_trash_is_where_the_freedesktop_home_trash_keeps_the_trashed_files() {
    let roots = roots(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "/data")]);

    assert_eq!(
        path(&roots, SymbolicRoot::Trash),
        Ok((
            PathBuf::from("/data/Trash/files"),
            RootSource::Default {
                base: SymbolicRoot::XdgData,
                relative: "Trash/files",
            }
        ))
    );
}

/// A real home directory in a temp dir, as `HOME`.
fn home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn locations(roots: &Roots) -> Vec<PathBuf> {
    let all = roots.all(SymbolicRoot::GitAgentLocations);
    all.iter()
        .map(|location| location.as_ref().unwrap().path.clone())
        .collect()
}

/// Catches an absent location opened as a root, which reads as a scanned one, and a present
/// one left out, so its worktrees are never seen.
#[test]
fn agent_locations_are_opened_only_when_present() {
    let dir = home();
    let home = dir.path();
    let vars = [("HOME", text(home))];
    assert_eq!(locations(&roots(&vars)), Vec::<PathBuf>::new());

    fs::create_dir_all(home.join(".cursor/worktrees")).unwrap();
    fs::create_dir_all(home.join(".codex/worktrees")).unwrap();
    fs::create_dir_all(home.join("conductor")).unwrap();
    fs::write(home.join(".herdr"), b"a file, not a directory").unwrap();

    assert_eq!(
        locations(&roots(&vars)),
        [
            home.join(".codex/worktrees"),
            home.join(".cursor/worktrees")
        ]
    );
}

/// Catches a location that can't be placed reading as an empty one: with `$CODEX_HOME`
/// unusable, the worktrees below it must show as a gap.
#[test]
fn an_agent_location_with_no_base_is_unknown() {
    let dir = home();
    let vars = [("HOME", text(dir.path())), ("CODEX_HOME", "relative/home")];

    let roots = roots(&vars);

    let all = roots.all(SymbolicRoot::GitAgentLocations);
    assert!(
        matches!(all, [Err(UnresolvedRoot::BaseUnresolved { .. })]),
        "expected one unknown location, found {all:?}"
    );
}

/// Catches a setup Pi accepts holding every finding of the scan: `pi.agent` is `HoldAll`, so
/// a tilde read as a relative path, or an unmade directory read as an error, would close
/// everything.
#[test]
fn pi_dir_expands_tilde_and_an_absent_one_holds_nothing() {
    let dir = home();
    let home = dir.path();
    for (value, expected) in [
        ("~", home.to_owned()),
        ("~/pi/agent", home.join("pi/agent")),
        ("", home.join(".pi/agent")),
    ] {
        let roots = roots(&[("HOME", text(home)), ("PI_CODING_AGENT_DIR", value)]);
        let (path, _) = path(&roots, SymbolicRoot::PiAgent).unwrap();
        assert_eq!(path, expected, "{value:?}");
    }
    let absent = home.join("not/made/yet");
    let vars = [("HOME", text(home)), ("PI_CODING_AGENT_DIR", text(&absent))];
    let roots = roots(&vars);
    assert_eq!(path(&roots, SymbolicRoot::PiAgent).unwrap().0, absent);

    let declared = IntegrationDescriptor {
        id: IntegrationId("pi"),
        lens: Lens::Agents,
        platforms: Platform::ALL,
        roots: &[],
        selectors: &[],
        artifacts: &[],
        owner_files: &[],
        verified: &[],
        consequences: &[],
        facts: &[],
        state: &[StateDeclaration {
            root: SymbolicRoot::PiAgent,
            protected: &[],
            open: &[],
            delegates: &[],
            unresolved: Unresolved::HoldAll,
        }],
        evidences: &[],
        revision: 1,
    };
    let held = |path: &Path| HeldPath {
        forms: vec![path.to_owned()],
        lineage: lineage(path),
        note: None,
    };
    let protections = Protections::of_host(&roots, Some(&home.join("data")), &[declared], &held);
    let finding = home.join("projects/app/node_modules");
    fs::create_dir_all(&finding).unwrap();

    let answer = protections.check(
        &finding,
        &lineage(&finding),
        IntegrationId("node"),
        &Owners::default(),
    );

    assert_eq!(answer, None);
}

fn lineage(path: &Path) -> Lineage {
    let identity = |at: &Path| NoFollowFs.metadata(at).ok().and_then(|m| m.identity);
    Lineage(path.ancestors().map(identity).collect())
}

/// Catches a value upstream refuses being treated as a place, or as unset: either would
/// let a finding through a store kipple can't locate.
#[test]
fn an_unusable_agent_home_is_unresolved_and_never_the_default() {
    let dir = home();
    let home = dir.path();
    let file = home.join("a-file");
    fs::write(&file, b"").unwrap();
    let missing = home.join("missing");
    let unusable = [
        ("CODEX_HOME", SymbolicRoot::CodexHome, "relative/home"),
        ("CODEX_HOME", SymbolicRoot::CodexHome, "   "),
        ("CODEX_HOME", SymbolicRoot::CodexHome, text(&missing)),
        ("CODEX_HOME", SymbolicRoot::CodexHome, text(&file)),
        (
            "PI_CODING_AGENT_DIR",
            SymbolicRoot::PiAgent,
            "relative/agent",
        ),
        ("PI_CODING_AGENT_DIR", SymbolicRoot::PiAgent, text(&file)),
    ];
    for (var, root, value) in unusable {
        let roots = roots(&[("HOME", text(home)), (var, value)]);
        assert!(
            matches!(roots.get(root), Some(Err(_))),
            "{var}={value:?} gave {:?}",
            roots.get(root)
        );
    }
}

/// Catches the release store derived through a link it can't re-check: `$CODEX_HOME` is
/// canonicalized, and what is below it hangs off the directory it leads to.
#[test]
fn codex_home_is_canonicalized_and_its_store_is_below_it() {
    let dir = home();
    let home = dir.path().canonicalize().unwrap();
    let real = home.join("real-codex");
    fs::create_dir(&real).unwrap();
    symlink(&real, home.join("linked")).unwrap();
    let linked = home.join("linked");

    let roots = roots(&[("HOME", text(&home)), ("CODEX_HOME", text(&linked))]);

    assert_eq!(path(&roots, SymbolicRoot::CodexHome).unwrap().0, real);
    let (store, _) = path(&roots, SymbolicRoot::CodexReleaseStore).unwrap();
    assert_eq!(store, real.join("packages"));
    let (default, _) = path(&roots, SymbolicRoot::CodexDefaultHome).unwrap();
    assert_eq!(default, home.join(".codex"));
}

/// Catches a store a config file moves staying open: `codex.sqlite-home` and
/// `pi.session-dir` exist only because of these two keys.
#[test]
fn settings_that_move_a_store_are_read() {
    let dir = home();
    let home = dir.path();
    let codex = home.join(".codex");
    let agent = home.join(".pi/agent");
    fs::create_dir_all(&codex).unwrap();
    fs::create_dir_all(&agent).unwrap();
    fs::write(
        codex.join("config.toml"),
        "model = \"x\"\nsqlite_home = \"/data/codex\" # moved\n\n[history]\nmax_bytes = 1\n",
    )
    .unwrap();
    fs::write(
        agent.join("settings.json"),
        "{\n  \"theme\": \"sessionDir\",\n  \"sessionDir\": \"~/pi-sessions\"\n}\n",
    )
    .unwrap();

    let roots = roots(&[("HOME", text(home))]);

    let (sqlite, source) = path(&roots, SymbolicRoot::CodexSqliteHome).unwrap();
    assert_eq!(sqlite, Path::new("/data/codex"));
    assert_eq!(
        source,
        RootSource::Setting {
            file: "config.toml",
            key: "sqlite_home",
        }
    );
    let (sessions, _) = path(&roots, SymbolicRoot::PiSessionDir).unwrap();
    assert_eq!(sessions, home.join("pi-sessions"));
}

/// Catches a store with no setting being invented, which would show a path nothing uses.
#[test]
fn a_store_no_setting_moves_is_absent() {
    let dir = home();

    let roots = roots(&[("HOME", text(dir.path()))]);

    assert!(
        roots.all(SymbolicRoot::CodexSqliteHome).is_empty(),
        "no setting moves the Codex store"
    );
    assert!(
        roots.all(SymbolicRoot::PiSessionDir).is_empty(),
        "no setting moves the Pi sessions"
    );
}

/// Catches a config kipple can't read being treated as one that sets nothing, and the value
/// of another key reaching the message: these files can hold tokens.
#[test]
fn an_unreadable_setting_is_unknown_and_shows_no_other_value() {
    let dir = home();
    let home = dir.path();
    let codex = home.join(".codex");
    fs::create_dir_all(&codex).unwrap();
    fs::write(
        codex.join("config.toml"),
        "token = \"s3cret\"\nsqlite_home = 12\n",
    )
    .unwrap();

    let roots = roots(&[("HOME", text(home))]);

    let found = roots.all(SymbolicRoot::CodexSqliteHome);
    assert_eq!(found.len(), 1, "one root, found {found:?}");
    let shown = found[0].as_ref().unwrap_err().to_string();
    assert!(shown.contains("sqlite_home"), "says which key: {shown}");
    assert!(!shown.contains("s3cret"), "shows no other value: {shown}");
}

const CODEX_CONFIG: &str = ".codex/config.toml";
const PI_SETTINGS: &str = ".pi/agent/settings.json";

/// What `root` resolves to with `document` at `file` under a new home, sorted.
fn set_by(file: &str, root: SymbolicRoot, document: &str) -> Vec<Result<PathBuf, String>> {
    let dir = home();
    let at = dir.path().join(file);
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    fs::write(&at, document).unwrap();
    let roots = roots(&[("HOME", text(dir.path()))]);
    let mut found: Vec<_> = roots
        .all(root)
        .iter()
        .map(|resolved| {
            let path = resolved.as_ref().map(|r| r.path.clone());
            path.map_err(ToString::to_string)
        })
        .collect();
    found.sort();
    found
}

fn paths(expected: &[&str]) -> Vec<Result<PathBuf, String>> {
    expected.iter().map(|p| Ok(PathBuf::from(p))).collect()
}

/// Catches a TOML form decoded wrongly or missed: that names the wrong directory and leaves
/// the real store open.
#[test]
fn every_toml_form_of_a_setting_decodes_to_its_path() {
    let rows: [(&str, &[&str]); 12] = [
        (
            r#"sqlite_home = "/data/co\u0064ex\ttab""#,
            &["/data/codex\ttab"],
        ),
        (r"sqlite_home = '/data/co\dex'", &[r"/data/co\dex"]),
        (r#""sqlite_home" = "/data/quoted""#, &["/data/quoted"]),
        (
            r#"'sqlite_home' = "/data/literal-key""#,
            &["/data/literal-key"],
        ),
        (r#"profile.sqlite_home = "/data/dotted""#, &["/data/dotted"]),
        (
            r#"profile = { sqlite_home = "/data/inline" }"#,
            &["/data/inline"],
        ),
        (
            "sqlite_home = \"\"\"\n/data/multi\\\n   -line\"\"\"",
            &["/data/multi-line"],
        ),
        ("sqlite_home = '''\n/data/raw\\n'''", &[r"/data/raw\n"]),
        (
            "[[profile]]\nsqlite_home = \"/data/a\"\n[[profile]]\nsqlite_home = \"/data/b\"\n",
            &["/data/a", "/data/b"],
        ),
        (
            "sqlite_home = \"/data/top\"\n[profile.deep]\nsqlite_home = \"/data/table\"\n",
            &["/data/table", "/data/top"],
        ),
        ("# sqlite_home = \"/data/comment\"\n", &[]),
        (r#"note = "sqlite_home = '/data/string'""#, &[]),
    ];

    for (document, expected) in rows {
        let found = set_by(CODEX_CONFIG, SymbolicRoot::CodexSqliteHome, document);
        assert_eq!(found, paths(expected), "for {document:?}");
    }
}

/// Catches a JSON form decoded wrongly or missed, or a key's name taken for the key where it
/// is only text.
#[test]
fn every_json_form_of_a_setting_decodes_to_its_path() {
    let rows: [(&str, &[&str]); 7] = [
        (r#"{"sessionDir": "/data/pi\u0073"}"#, &["/data/pis"]),
        (r#"{"sessionDir": "/data/\ud83d\ude00"}"#, &["/data/😀"]),
        (
            r#"{"session\u0044ir": "/data/escaped-key"}"#,
            &["/data/escaped-key"],
        ),
        (
            r#"{"a": {"sessionDir": "/data/a"}, "b": [{"sessionDir": "/data/b"}]}"#,
            &["/data/a", "/data/b"],
        ),
        (
            r#"{"sessionDir": "/data/first", "sessionDir": "/data/last"}"#,
            &["/data/last"],
        ),
        (r#"{"theme": "sessionDir"}"#, &[]),
        (r#"{"note": "\"sessionDir\": \"/data/string\""}"#, &[]),
    ];

    for (document, expected) in rows {
        let found = set_by(PI_SETTINGS, SymbolicRoot::PiSessionDir, document);
        assert_eq!(found, paths(expected), "for {document:?}");
    }
}

/// Catches a file kipple can't parse counting as one that sets nothing, a non-string value
/// being skipped, and the parser's own message reaching the output: toml's quotes the line,
/// and these files can hold tokens. A file nested past any use must not crash the scan.
#[test]
fn a_setting_file_kipple_cant_parse_is_unknown_and_quotes_nothing() {
    let deep = "[".repeat(100_000);
    let toml_rows = [
        "token = \"s3cret\"\nsqlite_home = \"/data/unclosed\n".to_owned(),
        "token = \"s3cret\"\ntoken = \"s3cret\"\n".to_owned(),
        "[profile]\nsqlite_home = [\"/data/s3cret\"]\n".to_owned(),
        "[profile]\nsqlite_home = { token = \"s3cret\" }\n".to_owned(),
        format!("token = {deep}"),
    ];
    let json_rows = [
        "{\"token\": \"s3cret\", \"sessionDir\": }".to_owned(),
        "{\"token\": \"s3cret\", \"sessionDir\": null}".to_owned(),
        "{\"a\": [{\"sessionDir\": 12}], \"token\": \"s3cret\"}".to_owned(),
        deep,
    ];
    let toml_cases = toml_rows
        .iter()
        .map(|row| set_by(CODEX_CONFIG, SymbolicRoot::CodexSqliteHome, row));
    let json_cases = json_rows
        .iter()
        .map(|row| set_by(PI_SETTINGS, SymbolicRoot::PiSessionDir, row));

    for found in toml_cases.chain(json_cases) {
        assert_eq!(found.len(), 1, "one unknown root, found {found:?}");
        let message = found[0].as_ref().unwrap_err();
        assert!(!message.contains("s3cret"), "shows no value: {message}");
    }
}

/// Catches the parsers' input reaching a panic through what a person left in a file: any
/// input must end in a value, an unknown or nothing.
#[test]
fn the_setting_readers_survive_generated_input() {
    const PIECES: [&str; 14] = [
        "sqlite_home",
        "\"sessionDir\"",
        " = ",
        ": ",
        "\"",
        "'",
        "\\",
        "\\\\",
        "\n",
        "é",
        "/a/b",
        "~/x",
        "[t]",
        "{",
    ];
    let dir = home();
    let home = dir.path();
    let codex = home.join(".codex");
    let agent = home.join(".pi/agent");
    fs::create_dir_all(&codex).unwrap();
    fs::create_dir_all(&agent).unwrap();
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    for _ in 0..300 {
        let mut document = String::new();
        for _ in 0..12 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let at = usize::try_from(state >> 33).unwrap() % PIECES.len();
            document.push_str(PIECES[at]);
        }
        fs::write(codex.join("config.toml"), &document).unwrap();
        fs::write(agent.join("settings.json"), &document).unwrap();

        let roots = roots(&[("HOME", text(home))]);

        assert!(
            roots.get(SymbolicRoot::Home).is_some(),
            "resolved for {document:?}"
        );
    }
}
