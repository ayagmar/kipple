//! Native-backend tests: the real process table and the real account's roots. They run
//! only in the `native` nextest profile, on a disposable host
//! (`docs/08-engineering-standards.md` §5).

use std::ffi::OsString;

use kipple_core::{EntryKind, FsProbe, ProcessOutcome, ProcessProbe, SymbolicRoot};
use kipple_platform::{Environment, NoFollowFs, OsProcesses, resolve_roots};

#[test]
fn this_process_is_identified_by_the_file_it_runs() {
    let exe = std::env::current_exe().unwrap();
    let expected = NoFollowFs.metadata(&exe).unwrap().identity.unwrap();

    let snapshot = OsProcesses.snapshot().unwrap();

    let me = snapshot
        .processes
        .iter()
        .find(|process| process.pid == std::process::id())
        .expect("this process is listed");
    assert!(
        matches!(&me.outcome, ProcessOutcome::Identified { identity, deleted: false, .. } if *identity == expected),
        "{:?}",
        me.outcome
    );
}

#[test]
fn the_home_root_resolves_to_an_existing_directory() {
    let vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();

    let roots = resolve_roots(&Environment::from_vars(vars));

    let Some(Ok(home)) = roots.get(SymbolicRoot::Home) else {
        panic!("home did not resolve: {:?}", roots.get(SymbolicRoot::Home));
    };
    assert_eq!(
        NoFollowFs.metadata(&home.path).unwrap().kind,
        EntryKind::Dir
    );
}
