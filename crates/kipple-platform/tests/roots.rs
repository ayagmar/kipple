//! Root resolution from an injected environment, upstream conventions first
//! (`docs/05-architecture.md` §1). Windows resolves Known Folders through the OS, so its
//! roots are covered by the native profile instead. A root whose own path is a link is
//! resolved once (D-041), here in a temp dir.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use kipple_core::{FsProbe as _, LinkProblem, RootSource, Roots, SymbolicRoot, UnresolvedRoot};
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

    assert!(matches!(
        roots.get(SymbolicRoot::XdgCache),
        Some(Err(UnresolvedRoot::NotAbsolute {
            var: "XDG_CACHE_HOME",
            ..
        }))
    ));
    assert!(kipple_dirs(&roots).cache.path.is_err());
}

#[test]
fn without_home_only_explicitly_set_roots_resolve() {
    let roots = roots(&[("XDG_DATA_HOME", "/data")]);

    assert!(matches!(
        roots.get(SymbolicRoot::Home),
        Some(Err(UnresolvedRoot::NotSet { var: "HOME" }))
    ));
    assert!(matches!(
        roots.get(SymbolicRoot::XdgCache),
        Some(Err(UnresolvedRoot::BaseUnresolved {
            base: SymbolicRoot::Home
        }))
    ));
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

    assert!(matches!(
        dangling.get(SymbolicRoot::XdgCache),
        Some(Err(UnresolvedRoot::Link {
            problem: LinkProblem::Unresolvable(_),
            ..
        }))
    ));
    assert!(matches!(
        to_file.get(SymbolicRoot::XdgCache),
        Some(Err(UnresolvedRoot::Link {
            problem: LinkProblem::NotDirectory,
            ..
        }))
    ));
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
    ];
    let replaced = project_roots(&resolve_roots(&env, &given));
    assert_eq!(
        replaced[0],
        Ok((home.join("code"), RootSource::CommandLine))
    );
    assert!(replaced[1].as_ref().is_err_and(|e| e.contains("--root")));
    assert!(
        replaced[2]
            .as_ref()
            .is_err_and(|e| e.contains("can't be resolved"))
    );
    assert_eq!(replaced.len(), 3);
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
            .is_err_and(|e| e.contains("can't be resolved"))
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
