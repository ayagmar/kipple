//! The git tracked query (D-066) against the host's git, on repositories built in temp
//! directories with the tests' own config (`docs/08-engineering-standards.md` §5): a
//! tracked file inside an apparent artifact (04 §10) is named, and every answer git can't
//! give in full blocks the item.

#[path = "git/fixture.rs"]
mod fixture;

use std::ffi::OsString;
use std::fs;

use fixture::{Repo, freeze, host_env, paths, query, remove, rename};
use kipple_core::{CancelToken, GitGap, TrackedQuery as _};
use kipple_platform::{Environment, GitQuery};

/// Catches a tracked file inside an artifact left in the plan, and an untracked one left
/// out, with more tracked paths than the runner's old 64 KiB output bound holds.
#[test]
fn every_tracked_path_in_an_item_is_named_and_nothing_else() {
    let repo = Repo::new();
    let many: Vec<String> = (0..1500)
        .map(|n| format!("target/generated/a-long-enough-file-name-{n:05}.rs"))
        .collect();
    for path in &many {
        repo.write(path);
    }
    repo.git(&["add", "target/generated"]);
    repo.track("target/keep.rs");
    repo.track("src/main.rs");
    repo.write("target/debug/build-output");
    repo.write("target/.rustc_info.json");

    assert_eq!(repo.count("target"), Ok(1501));
    let tracked = repo.tracked("target").unwrap();

    assert_eq!(tracked.len(), 1501);
    assert!(
        tracked.contains(&"keep.rs".to_owned()),
        "{:?}",
        &tracked[..3]
    );
    assert!(
        !tracked.iter().any(|path| path.starts_with("debug")),
        "nothing untracked is named"
    );
    assert_eq!(repo.count("src"), Ok(1), "a sibling is asked about alone");
}

/// Catches an item with nothing tracked read as an error, which would hold every artifact
/// in a repository.
#[test]
fn an_item_git_tracks_nothing_in_names_nothing() {
    let repo = Repo::new();
    repo.track("Cargo.toml");
    repo.write("target/debug/x");

    assert_eq!(repo.count("target"), Ok(0));
    assert_eq!(repo.tracked("target"), Ok(Vec::new()));
}

/// Catches the scan's answer reused at apply: a file added to the index after the scan
/// is still left out.
#[test]
fn the_repository_is_asked_again_and_a_file_tracked_since_is_left_out() {
    let repo = Repo::new();
    repo.write("target/late.rs");
    assert_eq!(repo.count("target"), Ok(0));

    repo.git(&["add", "target/late.rs"]);

    assert_eq!(repo.tracked("target"), Ok(vec!["late.rs".to_owned()]));
}

/// Catches a skip-worktree entry, absent on disk, taken for an entry kipple must place:
/// it holds nothing of the item, so it places nothing and blocks nothing.
#[test]
fn a_skip_worktree_entry_absent_on_disk_places_nothing() {
    let repo = Repo::new();
    repo.track("target/gone/away.rs");
    repo.track("target/here.rs");
    repo.track("target/sparse/left.rs");
    repo.git(&[
        "update-index",
        "--skip-worktree",
        "target/gone/away.rs",
        "target/sparse/left.rs",
    ]);
    remove(&repo.at("target/gone/away.rs"));
    remove(&repo.at("target/sparse/left.rs"));
    fs::create_dir(repo.at("target/sparse")).unwrap();

    assert_eq!(repo.count("target"), Ok(3));
    assert_eq!(repo.tracked("target"), Ok(vec!["here.rs".to_owned()]));
}

/// Catches a submodule's gitlink, a directory git tracks as one entry, being taken for
/// nothing tracked: its checkout would be removed.
#[test]
fn a_gitlink_inside_the_item_blocks_it() {
    let repo = Repo::new();
    fs::create_dir_all(repo.at("target/vendored")).unwrap();
    repo.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        "160000,0123456789abcdef0123456789abcdef01234567,target/vendored",
    ]);

    assert_eq!(repo.count("target"), Err(GitGap::Unplaced));
    let anchor = repo.anchor("target");
    let tree = freeze(&anchor);
    let entries = query().entries(&repo.root, &anchor, &tree, &CancelToken::new());
    assert_eq!(entries, Err(GitGap::Unplaced));
}

/// Catches a tracked file replaced after the freeze: the file on disk is no entry kipple
/// froze, so its place in the plan is unknown.
#[test]
fn a_tracked_path_that_matches_no_frozen_entry_blocks_the_item() {
    let repo = Repo::new();
    repo.track("target/keep.rs");
    let anchor = repo.anchor("target");
    let tree = freeze(&anchor);
    remove(&repo.at("target/keep.rs"));
    repo.write("target/keep.rs");

    let entries = query().entries(&repo.root, &anchor, &tree, &CancelToken::new());

    assert_eq!(entries, Err(GitGap::Unplaced));
}

/// Catches only the nearest repository asked: one made inside a directory another
/// already tracks files in leaves them in the outer index, and they would be removed.
/// Each repository answers for its own files, and an outer one holding the inner as a
/// submodule tracks nothing inside it.
#[test]
fn every_repository_above_an_item_answers_for_its_files() {
    let outer = Repo::new();
    outer.track("README");
    outer.track("inner/target/outer.rs");
    let inner = outer.at("inner");
    outer.git(&["-C", "inner", "init", "-q"]);
    outer.write("inner/target/inner.rs");
    outer.git(&["-C", "inner", "add", "target/inner.rs"]);
    outer.write("inner/target/untracked.rs");
    let query = query();
    let cancel = CancelToken::new();
    let anchor = outer.anchor("inner/target");
    let tree = freeze(&anchor);

    assert_eq!(query.count(&inner, &anchor.path(), &cancel), Ok(2));
    let ids = query.entries(&inner, &anchor, &tree, &cancel).unwrap();
    assert_eq!(paths(&tree, &ids), ["inner.rs", "outer.rs"]);

    outer.git(&["rm", "-q", "--cached", "inner/target/outer.rs"]);
    outer.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        "160000,0123456789abcdef0123456789abcdef01234567,inner",
    ]);
    assert_eq!(query.count(&inner, &anchor.path(), &cancel), Ok(1));
}

/// Catches a `.git` file (a linked worktree or submodule) not followed to its git
/// directory outside, and a work tree git names differently from the walk's.
#[test]
fn a_git_file_pointing_outside_is_followed_and_another_work_tree_blocks() {
    let repo = Repo::new();
    let separate = repo.temp.path().join("separate.git");
    let outside = repo.temp.path().join("checkout");
    fs::create_dir(&outside).unwrap();
    repo.git(&[
        "-C",
        outside.to_str().unwrap(),
        "init",
        "-q",
        "--separate-git-dir",
        separate.to_str().unwrap(),
    ]);
    assert!(outside.join(".git").is_file(), "a gitdir file");
    fs::write(outside.join("tracked.rs"), b"x").unwrap();
    repo.git(&["-C", outside.to_str().unwrap(), "add", "tracked.rs"]);
    let query = query();
    let cancel = CancelToken::new();

    assert_eq!(
        query.count(&outside, &outside.join("tracked.rs"), &cancel),
        Ok(1)
    );

    // Its config now names another directory as its work tree.
    repo.git(&[
        "-C",
        outside.to_str().unwrap(),
        "config",
        "core.worktree",
        repo.root.to_str().unwrap(),
    ]);
    assert_eq!(
        query.count(&outside, &outside.join("tracked.rs"), &cancel),
        Err(GitGap::OtherWorkTree)
    );
}

/// Catches kipple's own environment reaching git: `GIT_DIR`, `GIT_INDEX_FILE` or
/// `GIT_CONFIG_PARAMETERS` would make it answer about another repository or index.
#[test]
fn gits_variables_in_kipples_environment_never_reach_it() {
    let repo = Repo::new();
    repo.track("target/keep.rs");
    let other = Repo::new();
    let other_git = other.at(".git");
    let other_index = other.at(".git/no-such-index");
    let env = host_env(&[
        ["GIT_DIR", other_git.to_str().unwrap()],
        ["GIT_WORK_TREE", other.root.to_str().unwrap()],
        ["GIT_INDEX_FILE", other_index.to_str().unwrap()],
        ["GIT_CONFIG_PARAMETERS", "'core.worktree'='/'"],
        ["GIT_LITERAL_PATHSPECS", "0"],
    ]);
    let query = GitQuery::new(&env);

    let count = query.count(&repo.root, &repo.at("target"), &CancelToken::new());

    assert_eq!(count, Ok(1));
}

/// Catches git missing or too old to isolate being taken for nothing tracked.
#[test]
fn git_missing_blocks_every_item_in_a_repository() {
    let repo = Repo::new();
    repo.track("target/keep.rs");
    let empty = repo.temp.path().join("home");
    for env in [
        Environment::from_vars([(OsString::from("PATH"), empty.into())]),
        Environment::default(),
    ] {
        let query = GitQuery::new(&env);
        let count = query.count(&repo.root, &repo.at("target"), &CancelToken::new());
        assert_eq!(count, Err(GitGap::Missing));
    }
}

/// Catches a cancelled scan or apply waiting on git, or reading its silence as an answer.
#[test]
fn a_cancelled_query_fails() {
    let repo = Repo::new();
    repo.track("target/keep.rs");
    let cancel = CancelToken::new();
    cancel.cancel();

    let count = query().count(&repo.root, &repo.at("target"), &cancel);

    assert_eq!(count, Err(GitGap::Failed));
}

/// Catches pathspec magic read from a directory's name: as a glob `target[1]` names
/// `target1` and `*` names everything, and `:(exclude)` turns the question around.
#[cfg(unix)]
#[test]
fn magic_in_a_name_is_read_literally() {
    let repo = Repo::new();
    repo.track("target1/sibling.rs");
    repo.track("t/z.rs");
    repo.track("x/other.rs");
    for name in ["target[1]", "*", ":x", ":(attr:zz)t", ":(exclude)t", "?"] {
        repo.write(format!("{name}/untracked.o"));
        assert_eq!(repo.count(name), Ok(0), "{name}");
        assert_eq!(repo.tracked(name), Ok(Vec::new()), "{name}");
    }
    repo.track(":(attr:zz)t/mine.rs");
    assert_eq!(repo.tracked(":(attr:zz)t"), Ok(vec!["mine.rs".to_owned()]));
}

/// Catches a name that isn't UTF-8 passed to git lossily, which would ask about another
/// path and find nothing tracked. macOS file systems refuse such names.
#[cfg(target_os = "linux")]
#[test]
fn an_item_whose_name_isnt_utf8_is_asked_about_exactly() {
    use std::os::unix::ffi::OsStrExt as _;

    let repo = Repo::new();
    let name = std::ffi::OsStr::from_bytes(b"build-\xff");
    let item = std::path::Path::new(name);
    repo.track(item.join("keep.rs"));
    repo.write(item.join("out.o"));

    assert_eq!(repo.count(item), Ok(1));
    assert_eq!(repo.tracked(item), Ok(vec!["keep.rs".to_owned()]));
}

/// Catches a Windows name that can't reach git whole (ill-formed UTF-16) being asked
/// about by a lossy copy.
#[cfg(windows)]
#[test]
fn an_item_whose_name_cant_reach_git_blocks() {
    use std::os::windows::ffi::OsStringExt as _;

    let repo = Repo::new();
    repo.track("keep.rs");
    let name = OsString::from_wide(&[0x62, 0xD800]);
    fs::create_dir(repo.at(&name)).unwrap();

    assert_eq!(repo.count(&name), Err(GitGap::Unpassable));
}

/// Catches a case-only rename on a volume that folds case: git keeps the old case in its
/// index and matches pathspecs by case, so asking by the new name alone finds nothing
/// and the tracked file is removed. Where the volume tells the cases apart, the file at
/// the new name isn't the one git tracks.
#[test]
fn a_directory_renamed_only_in_case_is_still_asked_about() {
    let repo = Repo::new();
    repo.track("p/Target/x.rs");
    repo.track("p/Target/Debug/y.rs");
    rename(&repo.at("p/Target"), &repo.at("p/target"));
    let folds = repo.at("p/TARGET").exists();

    let tracked = repo.tracked("p/target");

    if folds {
        assert_eq!(repo.count("p/target"), Ok(2));
        assert_eq!(
            tracked,
            Ok(vec!["Debug/y.rs".to_owned(), "x.rs".to_owned()])
        );
    } else {
        assert_eq!(repo.count("p/target"), Ok(0));
        assert_eq!(tracked, Ok(Vec::new()));
    }
}

/// Catches a case-only rename past ASCII on a volume that folds Unicode case (APFS,
/// NTFS): git folds only ASCII case, so asking by the new name finds nothing and the
/// tracked file is removed. git is asked about the directory above, and the paths it
/// holds elsewhere there are neither counted nor blocking. Where the volume tells the
/// cases apart, the file at the new name isn't the one git tracks.
#[test]
fn a_directory_renamed_only_in_case_past_ascii_is_still_asked_about() {
    let repo = Repo::new();
    repo.track("p/\u{fc}ber/target/keep.rs");
    repo.track("p/other/target/elsewhere.rs");
    repo.track("p/\u{fc}berall/target/beside.rs");
    repo.track("README");
    rename(&repo.at("p/\u{fc}ber"), &repo.at("p/\u{dc}ber"));
    let folds = repo.at("p/\u{fc}ber").exists();

    let tracked = repo.tracked("p/\u{dc}ber/target");

    if folds {
        assert_eq!(repo.count("p/\u{dc}ber/target"), Ok(1));
        assert_eq!(tracked, Ok(vec!["keep.rs".to_owned()]));
    } else {
        assert_eq!(repo.count("p/\u{dc}ber/target"), Ok(0));
        assert_eq!(tracked, Ok(Vec::new()));
    }
}

/// Catches an item reached by its 8.3 short name (`--root C:\\code\\MY-LON~1`): git
/// matches only the long name the index holds, so asking by the short one finds nothing
/// and the tracked file is removed. A volume that makes no short names has nothing to
/// test.
#[cfg(windows)]
#[test]
fn an_item_reached_by_its_short_name_is_still_asked_about() {
    let repo = Repo::new();
    repo.track("my-long-project/target/keep.rs");
    let Some(short) = fixture::short_name(&repo.at("my-long-project")) else {
        return;
    };

    let item = repo.root.join(short).join("target");
    let count = query().count(&repo.root, &item, &CancelToken::new());

    assert_eq!(count, Ok(1));
}

/// Catches a directory whose name is stored in another Unicode normalization than the
/// index holds (macOS): the file system finds one by the other, so its tracked files are
/// still named. Where the volume tells them apart, the renamed one isn't tracked.
#[test]
fn a_directory_renamed_to_another_normalization_is_still_asked_about() {
    let repo = Repo::new();
    // As `git init` sets it on macOS, where file names come back decomposed.
    repo.git(&["config", "core.precomposeunicode", "true"]);
    // "été", with each "é" one code point, then a letter and a combining accent.
    let composed = "\u{e9}t\u{e9}";
    let decomposed = "e\u{301}te\u{301}";
    repo.track(format!("{composed}/keep.rs"));
    rename(&repo.at(composed), &repo.at(decomposed));
    let normalizes = repo.at(composed).exists();

    let tracked = repo.tracked(decomposed);

    if normalizes {
        assert_eq!(tracked, Ok(vec!["keep.rs".to_owned()]));
    } else {
        assert_eq!(tracked, Ok(Vec::new()));
    }
}

/// Catches a root reached through a link: git prints the physical work tree, which only
/// file identity matches to the path kipple walked.
#[cfg(unix)]
#[test]
fn a_work_tree_reached_through_a_link_is_the_same_work_tree() {
    let repo = Repo::new();
    repo.track("target/keep.rs");
    let link = repo.temp.path().join("linked");
    std::os::unix::fs::symlink(&repo.root, &link).unwrap();

    let count = query().count(&link, &link.join("target"), &CancelToken::new());

    assert_eq!(count, Ok(1));
}

/// Catches the repository's own config running something while kipple asks: an
/// fsmonitor, a hook, a pager, or any of them through an included file.
#[cfg(unix)]
#[test]
fn a_hostile_repository_config_runs_nothing() {
    use std::os::unix::fs::PermissionsExt as _;

    let repo = Repo::new();
    repo.track("target/keep.rs");
    let marker = repo.temp.path().join("ran");
    let script = repo.temp.path().join("hostile");
    fs::write(
        &script,
        format!("#!/bin/sh\necho \"$0 $*\" >> '{}'\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let hooks = repo.temp.path().join("hostile-hooks");
    fs::create_dir(&hooks).unwrap();
    for hook in ["post-index-change", "reference-transaction", "pre-commit"] {
        fs::copy(&script, hooks.join(hook)).unwrap();
    }
    let included = repo.temp.path().join("included");
    let script = script.display();
    fs::write(
        &included,
        format!("[core]\n\tfsmonitor = {script}\n\tpager = {script}\n"),
    )
    .unwrap();
    let config = repo.at(".git/config");
    let text = format!(
        "{}[core]\n\tfsmonitor = {script}\n\thooksPath = {}\n\tpager = {script}\n\
         [include]\n\tpath = {}\n[pager]\n\tls-files = {script}\n",
        fs::read_to_string(&config).unwrap(),
        hooks.display(),
        included.display(),
    );
    fs::write(&config, text).unwrap();

    assert_eq!(repo.count("target"), Ok(1));
    assert_eq!(repo.tracked("target"), Ok(vec!["keep.rs".to_owned()]));
    assert!(
        !marker.exists(),
        "nothing the repository configures ran: {:?}",
        fs::read_to_string(&marker)
    );
}

/// Catches a git too old to leave the user's global config out being run anyway, and one
/// whose version can't be read taken for a new one.
#[cfg(unix)]
#[test]
fn a_git_older_than_2_32_or_of_unknown_version_blocks() {
    let repo = Repo::new();
    repo.write("src/main.rs");
    let listing = repo.temp.path().join("listing");
    fs::write(&listing, b"").unwrap();
    for (version, gap) in [("2.31.9", GitGap::TooOld), ("unknown", GitGap::Failed)] {
        let query = GitQuery::new(&fixture::fake_git(&repo, version, &listing));
        let count = query.count(&repo.root, &repo.at("src"), &CancelToken::new());
        assert_eq!(count, Err(gap), "{version}");
    }
    let query = GitQuery::new(&fixture::fake_git(&repo, "2.32.0", &listing));
    let count = query.count(&repo.root, &repo.at("src"), &CancelToken::new());
    assert_eq!(count, Ok(0), "2.32 is new enough");
}

/// Catches an answer cut at the output bound, or missing its last NUL, read as complete,
/// and an answer exactly at the bound refused.
#[cfg(unix)]
#[test]
fn an_answer_is_whole_up_to_64_mib_and_cut_past_it() {
    const BOUND: usize = 64 * 1024 * 1024;
    let repo = Repo::new();
    repo.write("target/keep.o");
    // Records of 64 bytes, each a path absent on disk.
    let record = |n: usize| format!("target/{n:056}\0");
    let at_bound: String = (0..BOUND / 64).map(record).collect();
    assert_eq!(at_bound.len(), BOUND);
    let listing = repo.temp.path().join("listing");
    let ask = |output: &[u8]| {
        fs::write(&listing, output).unwrap();
        let query = GitQuery::new(&fixture::fake_git(&repo, "2.56.0", &listing));
        query.count(&repo.root, &repo.at("target"), &CancelToken::new())
    };

    assert_eq!(ask(at_bound.as_bytes()), Ok(1_048_576));
    let past_bound = format!("{at_bound}target/x\0");
    assert_eq!(ask(past_bound.as_bytes()), Err(GitGap::Truncated));
    assert_eq!(ask(b"target/a\0target/keep.o"), Err(GitGap::Truncated));
    assert_eq!(ask(b"target/a\0\0"), Err(GitGap::Truncated));
}
