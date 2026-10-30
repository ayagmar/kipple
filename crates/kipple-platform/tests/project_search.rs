//! The search for projects to suggest as roots (D-053, `docs/03-functional-spec.md` §6) on
//! disposable temp homes: its depth, hidden-directory, link, project, root and budget
//! limits, and that it says when it stopped early.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use kipple_core::{CancelToken, Clock, HeldPath};
use kipple_platform::{ProjectSearch, ProjectsFound, SearchLimits, SystemClock, search_projects};
use tempfile::TempDir;

/// A project at `rest` below `home`, marked by a `Cargo.toml`.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn project(home: &Path, rest: &str) -> PathBuf {
    let dir = home.join(rest);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Cargo.toml"), "").unwrap();
    dir
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn dir(home: &Path, rest: &str) -> PathBuf {
    let dir = home.join(rest);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(unix)]
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn symlink_dir(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn symlink_dir(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_dir(target, link).unwrap();
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn home() -> (TempDir, PathBuf) {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    (temp, home)
}

fn markers() -> Vec<OsString> {
    vec![OsString::from("Cargo.toml"), OsString::from("package.json")]
}

#[derive(Default)]
struct Options<'roots> {
    skip: &'roots [PathBuf],
    exclude: &'roots [HeldPath],
    limits: SearchLimits,
}

fn search_with(home: &Path, options: &Options<'_>, clock: &dyn Clock) -> ProjectsFound {
    let markers = markers();
    let search = ProjectSearch {
        home,
        markers: &markers,
        skip: options.skip,
        exclude: options.exclude,
        limits: options.limits,
    };
    search_projects(&search, clock, &CancelToken::new())
}

fn search(home: &Path) -> ProjectsFound {
    search_with(home, &Options::default(), &SystemClock)
}

/// The projects found, as paths below `home`.
fn names(home: &Path, found: &ProjectsFound) -> Vec<String> {
    found
        .projects
        .iter()
        .filter_map(|path| path.strip_prefix(home).ok())
        .map(|rest| rest.to_string_lossy().replace('\\', "/"))
        .collect()
}

// Catches the depth limit being off by one either way: a project 4 levels below home is
// found, one 5 levels below isn't.
#[test]
fn a_project_at_depth_four_is_found_and_at_depth_five_is_not() {
    let (_temp, home) = home();
    project(&home, "a/b/c/four");
    project(&home, "w/x/y/z/five");
    let found = search(&home);
    assert_eq!(names(&home, &found), ["a/b/c/four"]);
    assert!(found.complete);
}

// Catches `.git` alone, or a `.git` file as a worktree has, not marking a project.
#[test]
fn a_git_directory_or_file_marks_a_project() {
    let (_temp, home) = home();
    dir(&home, "code/repo/.git");
    let worktree = dir(&home, "code/worktree");
    fs::write(worktree.join(".git"), "gitdir: elsewhere").ok();
    assert_eq!(names(&home, &search(&home)), ["code/repo", "code/worktree"]);
}

// Catches the search entering hidden directories such as `~/.cache` or `~/.cargo`, whose
// checkouts would flood the suggestions.
#[test]
fn a_hidden_directory_is_not_entered() {
    let (_temp, home) = home();
    project(&home, ".cache/registry/crate");
    project(&home, "Documents/.backup/app");
    project(&home, "Documents/app");
    assert_eq!(names(&home, &search(&home)), ["Documents/app"]);
}

// Catches the search following a link into a project, which could lead it anywhere on
// the machine.
#[test]
fn a_link_to_a_project_is_not_followed() {
    let (temp, home) = home();
    let outside = project(temp.path(), "outside/app");
    dir(&home, "links");
    symlink_dir(&outside, &home.join("links/app"));
    symlink_dir(&temp.path().join("outside"), &home.join("elsewhere"));
    let found = search(&home);
    assert_eq!(names(&home, &found), Vec::<String>::new());
    assert!(found.complete);
}

// Catches a project's own nested projects (a workspace member, a vendored package) being
// reported beside it.
#[test]
fn a_project_inside_a_project_is_not_reported() {
    let (_temp, home) = home();
    let workspace = project(&home, "Documents/workspace");
    project(&workspace, "crates/member");
    project(&workspace, "node_modules/dep");
    assert_eq!(names(&home, &search(&home)), ["Documents/workspace"]);
}

// Catches home itself counting as a project (a dotfiles repository in `~/.git`), which
// would stop the search before it looked anywhere.
#[test]
fn home_is_never_a_project() {
    let (_temp, home) = home();
    dir(&home, ".git");
    project(&home, "Documents/app");
    assert_eq!(names(&home, &search(&home)), ["Documents/app"]);
}

// Catches the search walking an existing project root, whose projects are already
// scanned.
#[test]
fn an_existing_root_is_not_entered() {
    let (_temp, home) = home();
    project(&home, "projects/api");
    project(&home, "Documents/app");
    let skip = [home.join("projects")];
    let options = Options {
        skip: &skip,
        ..Options::default()
    };
    let found = search_with(&home, &options, &SystemClock);
    assert_eq!(names(&home, &found), ["Documents/app"]);
}

// Catches the search walking into a root it was given by its physical path when home is
// reached through a link (`/home` -> `/var/home`): compared as paths, `~/projects` and
// the root don't match, so its projects would be suggested again.
#[test]
fn an_existing_root_is_not_entered_when_home_is_reached_through_a_link() {
    let (temp, home) = home();
    project(&home, "projects/api");
    project(&home, "Documents/app");
    // The link is above home, as `/home` is; a link at home itself is resolved before.
    let linked = temp.path().join("linked");
    symlink_dir(temp.path(), &linked);
    let linked = linked.join("home");
    let skip = [home.join("projects")];
    let options = Options {
        skip: &skip,
        ..Options::default()
    };
    let found = search_with(&linked, &options, &SystemClock);
    assert_eq!(names(&linked, &found), ["Documents/app"]);
}

// Catches an excluded path being read by the search when the scan never reads it.
#[test]
fn an_excluded_path_is_not_entered() {
    let (_temp, home) = home();
    project(&home, "Private/app");
    project(&home, "Documents/app");
    let exclude = [HeldPath {
        forms: vec![home.join("Private")],
        ..HeldPath::default()
    }];
    let options = Options {
        exclude: &exclude,
        ..Options::default()
    };
    let found = search_with(&home, &options, &SystemClock);
    assert_eq!(names(&home, &found), ["Documents/app"]);
}

// Catches the directory budget being ignored, or being hit without saying the list may
// be incomplete.
#[test]
fn the_directory_budget_stops_early_and_says_so() {
    let (_temp, home) = home();
    for n in 0..5 {
        project(&home, &format!("d{n}/app"));
    }
    let options = Options {
        limits: SearchLimits {
            dirs: 3,
            ..SearchLimits::default()
        },
        ..Options::default()
    };
    let found = search_with(&home, &options, &SystemClock);
    assert!(!found.complete);
    assert!(found.projects.len() < 5, "{found:?}");
    let all = search(&home);
    assert!(all.complete);
    assert_eq!(all.projects.len(), 5);
}

/// A clock that moves a second on every reading.
#[derive(Debug)]
struct Ticking(Mutex<SystemTime>);

impl Clock for Ticking {
    fn now(&self) -> SystemTime {
        let mut now = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *now += Duration::from_secs(1);
        *now
    }
}

// Catches the time budget being ignored, or being hit without saying the list may be
// incomplete. The clock is injected, so the test never waits.
#[test]
fn the_time_budget_stops_early_and_says_so() {
    let (_temp, home) = home();
    for n in 0..5 {
        project(&home, &format!("d{n}/app"));
    }
    let clock = Ticking(Mutex::new(SystemTime::UNIX_EPOCH));
    let found = search_with(&home, &Options::default(), &clock);
    assert!(!found.complete);
    assert!(found.projects.len() < 5, "{found:?}");
}

// Catches a cancelled search reporting a complete list.
#[test]
fn a_cancelled_search_is_incomplete() {
    let (_temp, home) = home();
    project(&home, "Documents/app");
    let markers = markers();
    let search = ProjectSearch {
        home: &home,
        markers: &markers,
        skip: &[],
        exclude: &[],
        limits: SearchLimits::default(),
    };
    let cancel = CancelToken::new();
    cancel.cancel();
    let found = search_projects(&search, &SystemClock, &cancel);
    assert!(!found.complete);
    assert_eq!(found.projects, Vec::<PathBuf>::new());
}

// Catches an unreadable directory failing the whole search, or vanishing without being
// counted.
#[cfg(unix)]
#[test]
fn an_unreadable_directory_is_skipped_and_counted() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_temp, home) = home();
    project(&home, "Documents/app");
    let locked = dir(&home, "locked");
    project(&locked, "inside");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).ok();
    let readable = fs::read_dir(&locked).is_ok();
    let found = search(&home);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).ok();
    if readable {
        // Running as root: permissions don't stop the read, so there is nothing to test.
        return;
    }
    assert_eq!(names(&home, &found), ["Documents/app"]);
    assert_eq!(found.unreadable, 1);
    assert!(found.complete);
}

// Catches a home the search couldn't read at all being reported as searched in full, so
// "no projects found" reads as an answer when nothing was looked at (04 §3).
#[test]
fn a_home_that_is_missing_or_not_a_directory_is_incomplete() {
    let (temp, home) = home();
    let file = project(temp.path(), "other").join("Cargo.toml");
    for unread in [home.join("missing"), file] {
        let found = search(&unread);
        assert!(!found.complete, "{unread:?}: {found:?}");
        assert_eq!(found.unreadable, 1);
    }
}

// Catches a home whose listing fails being reported as searched in full.
#[cfg(unix)]
#[test]
fn an_unreadable_home_is_incomplete() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_temp, home) = home();
    project(&home, "Documents/app");
    fs::set_permissions(&home, fs::Permissions::from_mode(0o000)).ok();
    let readable = fs::read_dir(&home).is_ok();
    let found = search(&home);
    fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).ok();
    if readable {
        // Running as root: permissions don't stop the read, so there is nothing to test.
        return;
    }
    assert!(!found.complete, "{found:?}");
    assert_eq!(found.unreadable, 1);
    assert_eq!(found.projects, Vec::<PathBuf>::new());
}
