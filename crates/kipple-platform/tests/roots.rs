//! Root resolution from an injected environment, upstream conventions first
//! (`docs/05-architecture.md` §1). Windows resolves Known Folders through the OS, so its
//! roots are covered by the native profile instead. A root whose own path is a link is
//! resolved once (D-041), here in a temp dir.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use kipple_core::{FsProbe as _, LinkProblem, RootSource, Roots, SymbolicRoot, UnresolvedRoot};
use kipple_platform::{Environment, NoFollowFs, kipple_dirs, resolve_roots};

fn roots(vars: &[(&str, &str)]) -> Roots {
    resolve_roots(&env(vars), &[])
}

fn env(vars: &[(&str, &str)]) -> Environment {
    Environment::from_vars(
        vars.iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    )
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

    let given = [PathBuf::from("/elsewhere"), PathBuf::from("relative")];
    let replaced = project_roots(&resolve_roots(&env, &given));
    assert_eq!(
        replaced[0],
        Ok((PathBuf::from("/elsewhere"), RootSource::CommandLine))
    );
    assert!(replaced[1].as_ref().is_err_and(|e| e.contains("--root")));
    assert_eq!(replaced.len(), 2);
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
fn the_linux_trash_is_the_freedesktop_home_trash_under_xdg_data_home() {
    let roots = roots(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "/data")]);

    assert_eq!(
        path(&roots, SymbolicRoot::Trash),
        Ok((
            PathBuf::from("/data/Trash"),
            RootSource::Default {
                base: SymbolicRoot::XdgData,
                relative: "Trash",
            }
        ))
    );
}
