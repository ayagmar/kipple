//! Native-backend tests: the real process table and the real account's roots. They run
//! only in the `native` nextest profile, on a disposable host
//! (`docs/08-engineering-standards.md` §5).

use std::ffi::OsString;

use kipple_core::{
    EntryKind, FsProbe, ProcessOutcome, ProcessOwner, ProcessProbe, ProcessRecord, SymbolicRoot,
};
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

/// This process's record in a fresh snapshot.
fn me() -> Option<ProcessRecord> {
    OsProcesses
        .snapshot()
        .ok()?
        .processes
        .into_iter()
        .find(|process| process.pid == std::process::id())
}

#[test]
fn this_process_is_owned_by_the_current_user() {
    assert_eq!(
        me().expect("this process is listed").owner,
        ProcessOwner::CurrentUser
    );
}

#[cfg(unix)]
#[test]
fn this_process_shows_its_working_directory_arguments_environment_and_mapped_executable() {
    use std::path::Path;

    let identity = |path: &Path| NoFollowFs.metadata(path).unwrap().identity.unwrap();
    let me = me().expect("this process is listed");

    // Compared by identity: the OS may report the directory by another form of its path.
    let cwd = me.cwd.expect("the working directory is readable");
    assert_eq!(identity(&cwd), identity(&std::env::current_dir().unwrap()));

    let passed: Vec<OsString> = std::env::args_os().collect();
    let argv = me.argv.expect("the arguments are readable");
    assert_eq!(argv.first(), passed.first());
    assert!(argv.iter().all(|arg| passed.contains(arg)), "{argv:?}");

    me.env_paths.expect("the environment is readable");

    let exe = identity(&std::env::current_exe().unwrap());
    let mapped = me.mapped_files.expect("the mapped files are readable");
    assert!(
        mapped
            .iter()
            .any(|file| NoFollowFs.metadata(file).ok().and_then(|m| m.identity) == Some(exe)),
        "{mapped:?}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn this_process_shows_its_mount_namespace_root_and_capabilities() {
    let me = me().expect("this process is listed");

    me.mount_namespace.expect("the mount namespace is readable");
    let root = NoFollowFs.metadata(std::path::Path::new("/")).unwrap();
    assert_eq!(me.root_dir.ok(), root.identity);
    me.capabilities.expect("the capabilities are readable");
}

/// The runner is a whole VM, so its process table holds the kernel's threads, whose
/// `exe` a user who isn't root is refused.
#[cfg(target_os = "linux")]
#[test]
fn kernel_threads_run_no_program_file() {
    let snapshot = OsProcesses.snapshot().unwrap();

    let kthreads = snapshot.processes.iter();
    let kthreads = kthreads.filter(|p| matches!(p.outcome, ProcessOutcome::NoExecutable));
    assert!(kthreads.count() > 0);
}

#[test]
fn the_home_root_resolves_to_an_existing_directory() {
    let vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();

    let roots = resolve_roots(&Environment::from_vars(vars), &[]);

    let Some(Ok(home)) = roots.get(SymbolicRoot::Home) else {
        panic!("home did not resolve: {:?}", roots.get(SymbolicRoot::Home));
    };
    assert_eq!(
        NoFollowFs.metadata(&home.path).unwrap().kind,
        EntryKind::Dir
    );
}
