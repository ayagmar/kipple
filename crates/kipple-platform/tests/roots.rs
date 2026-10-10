//! Root resolution from an injected environment, upstream conventions first
//! (`docs/05-architecture.md` §1). Windows resolves Known Folders through the OS, so its
//! roots are covered by the native profile instead.
#![cfg(unix)]

use std::ffi::OsString;
use std::path::PathBuf;

use kipple_core::{RootSource, Roots, SymbolicRoot, UnresolvedRoot};
use kipple_platform::{Environment, kipple_dirs, resolve_roots};

fn roots(vars: &[(&str, &str)]) -> Roots {
    resolve_roots(&Environment::from_vars(
        vars.iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    ))
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
