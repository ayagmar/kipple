//! The confined executor and its freezer on the real file system (spike S1): the 04 §10
//! failure scenarios they own, on disposable trees. Each test names what it catches.

#[path = "executor/confinement.rs"]
mod confinement;
#[path = "executor/mounts.rs"]
mod mounts;
#[path = "executor/support.rs"]
pub(crate) mod support;

use std::fs;

use kipple_core::{
    CancelToken, DeleteOp, EntryKind, ErrorCode, FileSystemClass, Freed, Freezer as _, ItemOutcome,
    LeftReason, Marks, RelPath, Scope, Status,
};
use kipple_platform::ConfinedFs;
use support::{
    Report, Site, delete, freeze, is_link, left, link, move_to, present, removed, run, run_through,
    write,
};

/// `target/` holding files at three depths.
fn nested(site: &Site) {
    write(&site.at("target/a"), b"a");
    write(&site.at("target/sub/b"), b"bb");
    write(&site.at("target/sub/deeper/c"), b"ccc");
    write(&site.at("target/sub/deeper/d"), b"dddd");
}

/// Catches a delete that leaves part of an untouched tree, or removes its root.
#[test]
fn an_unchanged_tree_is_removed_whole_and_nothing_else() {
    let site = Site::new();
    nested(&site);
    write(&site.at("sibling"), b"next to the target");
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);
    assert_eq!(frozen.tree.entry_count(), 7, "the target and six entries");

    let outcome = delete(&anchor, frozen.tree);

    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    assert_eq!(removed(&outcome).names, 7);
    assert!(
        !present(&site.at("target")),
        "the target is gone: {outcome:?}"
    );
    assert!(
        present(&site.at("sibling")),
        "its sibling stays: {outcome:?}"
    );
    assert!(present(&site.root), "the root stays: {outcome:?}");
    site.assert_outside_intact();
}

/// Catches the root of a root's contents being removed (04 §3.2).
#[test]
fn a_roots_contents_go_and_the_root_stays() {
    let site = Site::new();
    write(&site.at("a"), b"a");
    write(&site.at("sub/b"), b"b");
    let anchor = site.anchor("");
    let frozen = freeze(&anchor);
    assert_eq!(frozen.tree.scope(), Scope::Contents);
    assert_eq!(frozen.parent, None);

    let outcome = delete(&anchor, frozen.tree);

    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    assert!(present(&site.root), "the root itself stays: {outcome:?}");
    assert_eq!(fs::read_dir(&site.root).unwrap().count(), 0);
}

/// Catches a run that trusts the tree's scope over its anchor: a root's contents frozen,
/// then handed over with a target below the root, would empty the whole root.
#[test]
fn a_tree_of_the_roots_contents_never_runs_for_a_target_below_it() {
    let site = Site::new();
    nested(&site);
    write(&site.at("other/x"), b"beside the target");
    let tree = freeze(&site.anchor("")).tree;

    let outcome = delete(&site.anchor("target"), tree);

    assert!(
        matches!(&outcome, ItemOutcome::Failed { error, .. } if error.code == ErrorCode::TargetChanged),
        "{outcome:?}"
    );
    assert!(present(&site.at("other/x")), "other/x stays: {outcome:?}");
    assert!(present(&site.at("target/a")), "target/a stays: {outcome:?}");
}

/// Catches a link inside a target being followed: what it leads to, outside the root,
/// must survive, and the link itself goes as a name (04 §3.4).
#[test]
fn a_link_inside_the_target_goes_and_what_it_leads_to_stays() {
    let site = Site::new();
    nested(&site);
    link(&site.outside.join("dir"), &site.at("target/dir-link"));
    link(&site.outside.join("keep"), &site.at("target/file-link"));
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);
    let dir_link = frozen
        .tree
        .find(&RelPath::of("dir-link".as_ref()).unwrap())
        .unwrap();
    assert_eq!(
        frozen.tree.entry(dir_link).unwrap().kind(),
        EntryKind::Symlink,
        "a link or junction is frozen as a link, not entered"
    );

    let outcome = delete(&anchor, frozen.tree);

    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    assert!(!present(&site.at("target")), "target is gone: {outcome:?}");
    site.assert_outside_intact();
}

/// Catches a directory swapped for a link while the run is going being entered, which
/// would remove what the link leads to (04 §3.4, §10 "symlink, junction ... escapes").
/// The swap comes after the target was opened and checked, so only the walk's own check
/// of `sub` stands between the run and the outside.
#[test]
fn a_directory_replaced_by_a_link_during_the_run_is_never_entered() {
    let site = Site::new();
    nested(&site);
    write(&site.outside.join("dir/b"), b"looks like the frozen b");
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);
    let marks = Marks::of(&frozen.tree);
    let op = DeleteOp::for_platform_tests(anchor, frozen.tree, marks);

    let outcome = run_through(&op, &CancelToken::new(), |report, _| {
        if matches!(report, Report::Removed(..)) && !is_link(&site.at("target/sub")) {
            site.replace(&site.at("target/sub"), |path| {
                link(&site.outside.join("dir"), path);
            });
        }
    });

    assert_eq!(outcome.status(), Status::Partial, "{outcome:?}");
    assert_eq!(
        left(&outcome, LeftReason::Replaced),
        5,
        "sub and all below it: {outcome:?}"
    );
    assert!(
        !present(&site.at("target/a")),
        "target/a is gone: {outcome:?}"
    );
    assert!(is_link(&site.at("target/sub")), "the link stays");
    assert!(
        present(&site.outside.join("dir/b")),
        "outside/dir/b stays: {outcome:?}"
    );
    site.assert_outside_intact();
}

/// Catches a directory changed between freeze and apply being emptied: once a name in
/// it was swapped, nothing below it is touched (amendment B).
#[test]
fn a_directory_changed_after_the_freeze_is_left_whole() {
    let site = Site::new();
    nested(&site);
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);
    site.replace(&site.at("target/sub"), |path| {
        link(&site.outside.join("dir"), path);
    });

    let outcome = delete(&anchor, frozen.tree);

    assert_eq!(outcome.status(), Status::Partial, "{outcome:?}");
    assert_eq!(left(&outcome, LeftReason::Changed), 7, "{outcome:?}");
    assert!(present(&site.at("target/a")), "target/a stays: {outcome:?}");
    site.assert_outside_intact();
}

/// Catches a component on the way to the target swapped for a link: the walk must stop
/// before reaching anything through it.
#[test]
fn a_link_on_the_way_to_the_target_stops_the_run() {
    let site = Site::new();
    write(&site.at("way/target/a"), b"a");
    write(&site.outside.join("target/a"), b"outside, same shape");
    let anchor = site.anchor("way/target");
    let frozen = freeze(&anchor);
    site.replace(&site.at("way"), |path| link(&site.outside, path));

    let outcome = delete(&anchor, frozen.tree);

    assert!(
        matches!(&outcome, ItemOutcome::Failed { error, .. } if error.code == ErrorCode::ThroughLink),
        "{outcome:?}"
    );
    assert_eq!(removed(&outcome).names, 0);
    assert!(
        present(&site.outside.join("target/a")),
        "outside/target/a stays: {outcome:?}"
    );
    site.assert_outside_intact();
}

/// Catches a root replaced by another directory being trusted (04 §3.4).
#[test]
fn a_replaced_root_stops_the_run_before_anything_is_removed() {
    let site = Site::new();
    nested(&site);
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);
    let elsewhere = site.outside.join("old-root");
    move_to(&site.root, &elsewhere);
    nested(&site);

    let outcome = delete(&anchor, frozen.tree);

    assert!(
        matches!(&outcome, ItemOutcome::Failed { error, .. } if error.code == ErrorCode::RootChanged),
        "{outcome:?}"
    );
    assert!(
        present(&site.at("target/sub/deeper/c")),
        "the new root's tree stays: {outcome:?}"
    );
    assert!(
        present(&elsewhere.join("target/sub/deeper/c")),
        "the old one too"
    );
}

/// Catches a freeze of a target whose directory isn't the one planned.
#[test]
fn a_freeze_refuses_a_target_whose_directory_was_replaced() {
    let site = Site::new();
    write(&site.at("way/target/a"), b"a");
    let anchor = site.anchor("way/target");
    site.replace(&site.at("way"), |path| write(&path.join("target/a"), b"a"));

    let refused = ConfinedFs.freeze(&anchor, &CancelToken::new()).unwrap_err();

    assert_eq!(refused.code, ErrorCode::ParentChanged);
}

/// Catches an item replaced while the run is going being removed (04 §10 "an item
/// replaced between scan and apply"): `z` is swapped after the target was opened and
/// checked, so only the check of `z` itself can catch it.
#[test]
fn a_file_replaced_during_the_run_stays() {
    let site = Site::new();
    nested(&site);
    write(&site.at("target/z"), b"z");
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);
    let marks = Marks::of(&frozen.tree);
    let op = DeleteOp::for_platform_tests(anchor, frozen.tree, marks);
    let mut swapped = false;

    let outcome = run_through(&op, &CancelToken::new(), |report, _| {
        if matches!(report, Report::Removed(..)) && !swapped {
            swapped = true;
            site.replace(&site.at("target/z"), |path| write(path, b"z"));
        }
    });

    assert_eq!(outcome.status(), Status::Partial, "{outcome:?}");
    assert_eq!(left(&outcome, LeftReason::Replaced), 1, "{outcome:?}");
    assert_eq!(
        left(&outcome, LeftReason::NotEmpty),
        1,
        "the target: {outcome:?}"
    );
    assert_eq!(fs::read(site.at("target/z")).unwrap(), b"z");
    assert!(
        !present(&site.at("target/sub")),
        "target/sub is gone: {outcome:?}"
    );
}

/// Catches a file edited in place after the freeze being removed: its fingerprint, not
/// only its identity, must match (spec pinned 3, amendment I).
#[test]
fn a_file_edited_in_place_after_the_freeze_stays() {
    let site = Site::new();
    nested(&site);
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);
    fs::write(site.at("target/sub/b"), b"edited, and longer").unwrap();

    let outcome = delete(&anchor, frozen.tree);

    assert_eq!(left(&outcome, LeftReason::Changed), 1, "{outcome:?}");
    assert_eq!(
        fs::read(site.at("target/sub/b")).unwrap(),
        b"edited, and longer"
    );
    assert!(
        !present(&site.at("target/sub/deeper")),
        "target/sub/deeper is gone: {outcome:?}"
    );
}

/// Catches a child created after the freeze being removed because its directory was
/// selected (04 §3.5): the directory changed, so it is left with everything in it
/// (amendment B), and the rest of the tree still goes.
#[test]
fn a_child_created_after_the_freeze_and_its_directory_stay() {
    let site = Site::new();
    nested(&site);
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);
    write(&site.at("target/sub/deeper/new"), b"appeared after review");

    let outcome = delete(&anchor, frozen.tree);

    assert_eq!(outcome.status(), Status::Partial, "{outcome:?}");
    assert_eq!(
        left(&outcome, LeftReason::Changed),
        3,
        "deeper and its two frozen files"
    );
    assert!(
        present(&site.at("target/sub/deeper/new")),
        "target/sub/deeper/new stays: {outcome:?}"
    );
    assert!(
        present(&site.at("target/sub/deeper/c")),
        "target/sub/deeper/c stays: {outcome:?}"
    );
    assert!(
        !present(&site.at("target/a")),
        "target/a is gone: {outcome:?}"
    );
}

/// Catches an entry the plan left out being removed, or its directories: they stay as
/// not-empty, and the item is still done (docs/04 §7).
#[test]
fn an_excluded_entry_and_its_directories_stay() {
    let site = Site::new();
    nested(&site);
    let anchor = site.anchor("target");
    let mut tree = freeze(&anchor).tree;
    let kept = tree
        .find(&RelPath::of("sub/deeper/c".as_ref()).unwrap())
        .unwrap();
    tree.exclude(kept);

    let outcome = delete(&anchor, tree);

    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    assert_eq!(
        left(&outcome, LeftReason::NotEmpty),
        3,
        "deeper, sub, target"
    );
    assert!(
        present(&site.at("target/sub/deeper/c")),
        "target/sub/deeper/c stays: {outcome:?}"
    );
    assert!(
        !present(&site.at("target/sub/deeper/d")),
        "target/sub/deeper/d is gone: {outcome:?}"
    );
    assert!(
        !present(&site.at("target/sub/b")),
        "target/sub/b is gone: {outcome:?}"
    );
}

/// Catches a non-UTF-8 name (Linux) or an ill-formed UTF-16 one (Windows) being
/// removed by a lossy copy of its name, which would miss it or hit another file.
#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_name_that_isnt_unicode_is_removed_by_its_own_name() {
    let site = Site::new();
    let odd = support::ill_formed("odd");
    write(&site.at("target").join(&odd).join("inner"), b"x");
    write(&site.at("target/odd--"), b"what a lossy copy might name");
    let anchor = site.anchor("target");
    let mut tree = freeze(&anchor).tree;
    let lookalike = tree.find(&RelPath::of("odd--".as_ref()).unwrap()).unwrap();
    tree.exclude(lookalike);

    let outcome = delete(&anchor, tree);

    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    assert!(
        !present(&site.at("target").join(&odd)),
        "the name that isn't Unicode is gone: {outcome:?}"
    );
    assert!(
        present(&site.at("target/odd--")),
        "target/odd-- stays: {outcome:?}"
    );
}

/// Catches a hard-linked file counted as freed while another name keeps it, and two
/// frozen names of one inode refusing each other after the first is removed (spec
/// pinned 7, amendments A and I).
#[test]
fn hard_links_are_removed_as_names_and_free_only_the_last() {
    let site = Site::new();
    write(&site.at("target/release/app"), &[7; 4096]);
    fs::create_dir(site.at("target/deps")).unwrap();
    fs::hard_link(
        site.at("target/release/app"),
        site.at("target/deps/app-1a2b"),
    )
    .unwrap();
    write(&site.at("target/shared"), b"another name stays outside");
    fs::hard_link(site.at("target/shared"), site.outside.join("shared")).unwrap();
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);

    let mut reports = Vec::new();
    let marks = Marks::of(&frozen.tree);
    let op = DeleteOp::for_platform_tests(anchor, frozen.tree, marks);
    let outcome = run_through(&op, &CancelToken::new(), |report, _| reports.push(report));

    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    let shared = reports
        .iter()
        .filter(|report| matches!(report, Report::Removed(_, Freed::Shared)))
        .count();
    assert_eq!(shared, 2, "the first app name and shared: {reports:?}");
    assert_eq!(
        fs::read(site.outside.join("shared")).unwrap(),
        b"another name stays outside"
    );
}

/// Catches an edit to a hard-linked file going unnoticed, since its change time alone
/// isn't compared (amendment I).
#[test]
fn an_edit_to_a_hard_linked_file_after_the_freeze_is_still_refused() {
    let site = Site::new();
    write(&site.at("target/a"), b"one inode");
    fs::hard_link(site.at("target/a"), site.at("target/b")).unwrap();
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);
    fs::write(site.at("target/a"), b"one inode, edited").unwrap();

    let outcome = delete(&anchor, frozen.tree);

    assert_eq!(left(&outcome, LeftReason::Changed), 2, "{outcome:?}");
    assert!(
        present(&site.at("target/a")) && present(&site.at("target/b")),
        "both names stay"
    );
}

/// Catches a sparse file counted at its apparent size: removed bytes are what it
/// allocated (04 §8).
#[cfg(unix)]
#[test]
fn a_sparse_file_frees_what_it_allocated_not_its_length() {
    let site = Site::new();
    fs::create_dir(site.at("target")).unwrap();
    let file = fs::File::create(site.at("target/sparse")).unwrap();
    file.set_len(1 << 30).unwrap();
    drop(file);
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);

    let outcome = delete(&anchor, frozen.tree);

    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    let estimated = removed(&outcome).estimated.0;
    assert!(
        estimated < 1 << 20,
        "a 1 GiB hole allocates almost nothing, not {estimated}"
    );
}

/// Catches one refused entry stopping the run or being reported removed (04 §10
/// "partial permission failure").
#[cfg(unix)]
#[test]
fn an_entry_the_os_refuses_is_left_and_the_rest_goes() {
    let site = Site::new();
    nested(&site);
    support::chmod(&site.at("target/sub/deeper"), 0o555);
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);

    let outcome = delete(&anchor, frozen.tree);
    support::chmod(&site.at("target/sub/deeper"), 0o755);

    assert_eq!(outcome.status(), Status::Partial, "{outcome:?}");
    assert_eq!(left(&outcome, LeftReason::Denied), 2, "c and d");
    assert_eq!(
        left(&outcome, LeftReason::NotEmpty),
        3,
        "deeper, sub, target"
    );
    assert!(
        !present(&site.at("target/a")),
        "target/a is gone: {outcome:?}"
    );
    assert!(
        present(&site.at("target/sub/deeper/c")),
        "target/sub/deeper/c stays: {outcome:?}"
    );
}

/// Catches a cancel that doesn't stop the run at the next entry (04 §10 "cancellation
/// after the first success").
#[test]
fn a_cancel_after_the_first_removal_stops_at_the_next_entry() {
    let site = Site::new();
    nested(&site);
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);
    let marks = Marks::of(&frozen.tree);
    let op = DeleteOp::for_platform_tests(anchor, frozen.tree, marks);

    let outcome = run_through(&op, &CancelToken::new(), |report, cancel| {
        if matches!(report, Report::Removed(..)) {
            cancel.cancel();
        }
    });

    assert_eq!(outcome.status(), Status::Cancelled, "{outcome:?}");
    assert_eq!(removed(&outcome).names, 1);
    assert_eq!(left(&outcome, LeftReason::NotReached), 6);
    let still = [
        "target/a",
        "target/sub/b",
        "target/sub/deeper/c",
        "target/sub/deeper/d",
    ]
    .iter()
    .filter(|path| present(&site.at(path)))
    .count();
    assert_eq!(still, 3, "exactly one file went");
}

/// Catches a freeze that goes on after a cancel.
#[test]
fn a_cancelled_freeze_stops() {
    let site = Site::new();
    nested(&site);
    let cancel = CancelToken::new();
    cancel.cancel();

    let refused = ConfinedFs
        .freeze(&site.anchor("target"), &cancel)
        .unwrap_err();

    assert_eq!(refused.code, ErrorCode::Cancelled);
}

/// Catches a file another program holds open without sharing delete being reported
/// removed, or stopping the run (04 §10 "locked files on Windows").
#[cfg(windows)]
#[test]
fn a_locked_file_is_left_busy_and_the_rest_goes() {
    use std::os::windows::fs::OpenOptionsExt as _;

    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

    let site = Site::new();
    nested(&site);
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(site.at("target/sub/b"))
        .unwrap();

    let outcome = delete(&anchor, frozen.tree);
    drop(held);

    assert_eq!(left(&outcome, LeftReason::Busy), 1, "{outcome:?}");
    assert!(
        present(&site.at("target/sub/b")),
        "target/sub/b stays: {outcome:?}"
    );
    assert!(
        !present(&site.at("target/sub/deeper")),
        "target/sub/deeper is gone: {outcome:?}"
    );
}

/// Catches a read-only file stopping a delete, or its attribute being cleared as a
/// separate step that would survive a failed delete (spike S1's Windows bar).
#[cfg(windows)]
#[test]
fn a_read_only_file_is_removed() {
    let site = Site::new();
    nested(&site);
    let mut readonly = fs::metadata(site.at("target/a")).unwrap().permissions();
    readonly.set_readonly(true);
    fs::set_permissions(site.at("target/a"), readonly).unwrap();
    let anchor = site.anchor("target");
    let frozen = freeze(&anchor);

    let outcome = delete(&anchor, frozen.tree);

    assert_eq!(outcome.status(), Status::Done, "{outcome:?}");
    assert!(!present(&site.at("target")), "target is gone: {outcome:?}");
}

/// Catches a cloud placeholder being frozen as a plain file, so a delete would remove a
/// file whose contents were never on disk (spec pinned 11). Windows marks one offline.
#[cfg(windows)]
#[test]
#[expect(
    unsafe_code,
    reason = "windows-sys exposes SetFileAttributesW as a raw call"
)]
fn a_placeholder_fails_the_freeze() {
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_OFFLINE, SetFileAttributesW};

    let site = Site::new();
    nested(&site);
    let path: Vec<u16> = site
        .at("target/sub/b")
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect();
    // SAFETY: `path` is NUL-terminated.
    assert_ne!(
        unsafe { SetFileAttributesW(path.as_ptr(), FILE_ATTRIBUTE_OFFLINE) },
        0
    );

    let refused = ConfinedFs
        .freeze(&site.anchor("target"), &CancelToken::new())
        .unwrap_err();

    assert_eq!(refused.code, ErrorCode::UnknownEntry);
    assert_eq!(refused.at, Some(RelPath::of("sub/b".as_ref()).unwrap()));
}

/// Catches the file system going unclassified on the system's own volume.
#[test]
fn the_temp_volume_is_a_supported_local_file_system() {
    let site = Site::new();
    nested(&site);

    let frozen = freeze(&site.anchor("target"));

    assert!(
        matches!(frozen.file_system, FileSystemClass::Supported(_)),
        "{:?}",
        frozen.file_system
    );
}

/// Catches an executor that touches an entry the operation doesn't authorize: the tally
/// would fail the item, and the entry would be gone.
#[test]
fn only_entries_still_authorized_are_touched() {
    let site = Site::new();
    nested(&site);
    let anchor = site.anchor("target");
    let tree = freeze(&anchor).tree;
    let mut marks = Marks::of(&tree);
    let sub = tree.find(&RelPath::of("sub".as_ref()).unwrap()).unwrap();
    marks.drop_subtree(&tree, sub, LeftReason::Changed);
    let op = DeleteOp::for_platform_tests(anchor, tree, marks);

    let outcome = run(&op);

    assert_eq!(outcome.status(), Status::Partial, "{outcome:?}");
    assert_eq!(left(&outcome, LeftReason::Changed), 5);
    assert!(
        present(&site.at("target/sub/deeper/d")),
        "target/sub/deeper/d stays: {outcome:?}"
    );
    assert!(
        !present(&site.at("target/a")),
        "target/a is gone: {outcome:?}"
    );
}
