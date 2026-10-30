//! Suggested project roots (D-053, `docs/03-functional-spec.md` §6): a project's parent,
//! or the project itself when its parent is home, nested suggestions folded, and never
//! home, an existing root, an excluded path or anything inside either.

use std::path::{Path, PathBuf};

use kipple_core::{HeldPath, Suggestion, suggest};

const HOME: &str = "/home/u";

fn home(rest: &str) -> PathBuf {
    Path::new(HOME).join(rest)
}

fn paths(rests: &[&str]) -> Vec<PathBuf> {
    rests.iter().map(|rest| home(rest)).collect()
}

fn excluded(rest: &str) -> HeldPath {
    HeldPath {
        forms: vec![home(rest)],
        ..HeldPath::default()
    }
}

fn suggested(projects: &[&str], roots: &[&str], exclude: &[HeldPath]) -> Vec<Suggestion> {
    suggest(Path::new(HOME), &paths(projects), &paths(roots), exclude)
}

fn suggestion(rest: &str, projects: &[&str]) -> Suggestion {
    Suggestion {
        path: home(rest),
        projects: paths(projects),
    }
}

// Catches the parent rule suggesting home itself, which would make the next scan walk
// every directory of the home folder.
#[test]
fn a_project_directly_in_home_is_suggested_as_itself_never_home() {
    let found = suggested(&["notes", "Documents/uni/thesis"], &[], &[]);
    assert_eq!(
        found,
        vec![
            suggestion("Documents/uni", &["Documents/uni/thesis"]),
            suggestion("notes", &["notes"]),
        ]
    );
    assert!(
        found.iter().all(|s| s.path != Path::new(HOME)),
        "home itself is never suggested"
    );
}

// Catches each project of one folder being offered as a root of its own.
#[test]
fn two_projects_in_one_folder_are_one_suggestion_with_both() {
    let found = suggested(&["Documents/uni/b", "Documents/uni/a"], &[], &[]);
    assert_eq!(
        found,
        vec![suggestion(
            "Documents/uni",
            &["Documents/uni/a", "Documents/uni/b"]
        )]
    );
}

// Catches nested suggestions being offered side by side, so adding both makes two roots
// that cover the same projects.
#[test]
fn a_suggestion_inside_another_folds_into_the_outer_one() {
    let found = suggested(&["Documents/a", "Documents/uni/b"], &[], &[]);
    assert_eq!(
        found,
        vec![suggestion("Documents", &["Documents/a", "Documents/uni/b"])]
    );
}

// Catches folding merging two directories that differ only in case, which a
// case-sensitive file system keeps apart: adding the one suggestion would scan only one
// of them, so its project count couldn't come true.
#[test]
fn folding_keeps_directories_that_differ_in_case_apart() {
    let found = suggested(&["Documents/a", "documents/uni/b"], &[], &[]);
    assert_eq!(
        found,
        vec![
            suggestion("Documents", &["Documents/a"]),
            suggestion("documents/uni", &["documents/uni/b"]),
        ]
    );
}

// Catches an existing root, or a folder inside one, being offered again.
#[test]
fn never_an_existing_root_or_a_path_inside_one() {
    let found = suggested(
        &["Documents/uni/a", "Documents/uni/deep/b", "work/c"],
        &["Documents/uni", "Work"],
        &[],
    );
    assert_eq!(found, Vec::new());
}

// Catches an excluded path, or a folder inside it, being offered as something to scan.
#[test]
fn never_an_excluded_path_or_a_path_inside_one() {
    let found = suggested(
        &["Private/a", "Private/old/b", "Code/c"],
        &[],
        &[excluded("Private")],
    );
    assert_eq!(found, vec![suggestion("Code", &["Code/c"])]);
}

// Catches a project outside home, or home itself, producing a suggestion above home.
#[test]
fn nothing_outside_home_or_above_it_is_suggested() {
    let found = suggest(
        Path::new(HOME),
        &[
            PathBuf::from("/srv/app"),
            PathBuf::from(HOME),
            PathBuf::from("/home"),
        ],
        &[],
        &[],
    );
    assert_eq!(found, Vec::new());
}

// Catches a home at the file system root letting `/` itself be suggested.
#[test]
fn never_the_file_system_root() {
    let found = suggest(Path::new("/"), &[PathBuf::from("/app")], &[], &[]);
    assert_eq!(
        found,
        vec![Suggestion {
            path: PathBuf::from("/app"),
            projects: vec![PathBuf::from("/app")],
        }]
    );
}
