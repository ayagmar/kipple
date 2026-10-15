//! The walker and the no-follow probes on disposable temp trees (`docs/04-safety-model.md`
//! §3.4 and §8, D-036).

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use kipple_core::{Apparent, CancelToken, Completeness, EntryKind, FsProbe, Sizer};
use kipple_platform::{NoFollowFs, Walker};
use tempfile::TempDir;

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn write(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![7_u8; bytes]).unwrap();
}

#[cfg(unix)]
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn symlink(target: &Path, link: &Path) {
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link).unwrap();
    } else {
        std::os::windows::fs::symlink_file(target, link).unwrap();
    }
}

/// `item/` with files at three depths, and links to a large file and a directory that
/// both live outside it.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn tree_with_links_out() -> (TempDir, u64) {
    let temp = TempDir::new().unwrap();
    let item = temp.path().join("item");
    write(&item.join("a"), 100);
    write(&item.join("sub/b"), 200);
    write(&item.join("sub/deeper/c"), 300);
    write(&temp.path().join("outside/big"), 1 << 20);
    symlink(&temp.path().join("outside/big"), &item.join("big-link"));
    symlink(&temp.path().join("outside"), &item.join("dir-link"));
    let links: u64 = ["big-link", "dir-link"]
        .iter()
        .map(|name| fs::symlink_metadata(item.join(name)).unwrap().len())
        .sum();
    (temp, 600 + links)
}

#[test]
fn an_item_is_sized_completely_and_links_out_of_it_are_never_followed() {
    let (temp, expected) = tree_with_links_out();

    let measured = Walker::new()
        .unwrap()
        .size(&temp.path().join("item"), &CancelToken::new())
        .unwrap();

    assert_eq!(measured.size.apparent, Apparent(expected));
    assert_eq!(measured.size.completeness, Completeness::Complete);
    assert!(!measured.skipped_mount);
}

#[test]
fn an_item_last_changed_when_anything_inside_it_did_a_file_deep_down_or_a_directory() {
    let temp = TempDir::new().unwrap();
    let item = temp.path().join("item");
    let file = item.join("pkg/lib/index.js");
    write(&file, 10);
    let set = |at: SystemTime| {
        let handle = fs::File::options().write(true).open(&file).unwrap();
        handle.set_modified(at).unwrap();
    };
    let newest_dir = ["item", "item/pkg", "item/pkg/lib"]
        .iter()
        .map(|dir| {
            fs::metadata(temp.path().join(dir))
                .unwrap()
                .modified()
                .unwrap()
        })
        .max()
        .unwrap();
    let modified = || {
        let walker = Walker::new().unwrap();
        walker.size(&item, &CancelToken::new()).unwrap().modified
    };

    // A package manager may give every file an old fixed time: the directories it
    // created still date the install.
    set(SystemTime::UNIX_EPOCH);
    assert_eq!(modified(), Some(newest_dir));

    let later = newest_dir + Duration::from_secs(3600);
    set(later);
    assert_eq!(modified(), Some(later));
}

#[test]
fn a_git_checkout_anywhere_inside_an_item_is_reported_whether_dot_git_is_a_dir_or_a_file() {
    let temp = TempDir::new().unwrap();
    let repository = |item: &Path| {
        let walker = Walker::new().unwrap();
        walker.size(item, &CancelToken::new()).unwrap().repository
    };
    // `pip install -e git+…` clones into the venv; a submodule or linked worktree has a
    // `.git` file instead of a directory.
    let venv = temp.path().join(".venv");
    write(&venv.join("src/pkg/.git/HEAD"), 10);
    write(&venv.join("lib/site.py"), 10);
    let modules = temp.path().join("node_modules");
    write(&modules.join("dep/.git"), 10);
    let plain = temp.path().join("target");
    write(&plain.join("debug/app"), 10);

    assert_eq!(repository(&venv), Some(venv.join("src/pkg")));
    assert_eq!(repository(&modules), Some(modules.join("dep")));
    assert_eq!(repository(&plain), None);
}

#[test]
fn a_cancelled_walk_is_never_reported_as_complete() {
    let (temp, _) = tree_with_links_out();
    let cancel = CancelToken::new();
    cancel.cancel();

    let size = Walker::new()
        .unwrap()
        .size(&temp.path().join("item"), &cancel)
        .unwrap()
        .size;

    assert_eq!(size.completeness, Completeness::Cancelled);
}

#[test]
fn probes_report_a_link_as_a_link_and_never_what_it_points_to() {
    let (temp, _) = tree_with_links_out();
    let item = temp.path().join("item");

    let meta = NoFollowFs.metadata(&item.join("dir-link")).unwrap();
    let through_link = NoFollowFs.read_dir(&item.join("dir-link"));
    // A trailing separator or `.` makes the OS resolve the link.
    let spellings = [
        item.join("dir-link").join(""),
        item.join("dir-link").join("."),
    ];
    let resolved: Vec<_> = spellings
        .iter()
        .map(|path| {
            (
                NoFollowFs.metadata(path).unwrap().kind,
                NoFollowFs.read_dir(path),
            )
        })
        .collect();
    let mut listing: Vec<_> = NoFollowFs
        .read_dir(&item)
        .unwrap()
        .into_iter()
        .map(|entry| (entry.name.into_string().unwrap(), entry.kind))
        .collect();
    listing.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(meta.kind, EntryKind::Symlink);
    for (kind, through) in &resolved {
        assert_eq!(*kind, EntryKind::Symlink);
        assert!(through.is_err(), "listed a link's target: {through:?}");
    }
    assert!(
        through_link.is_err(),
        "listed a link's target: {through_link:?}"
    );
    assert_eq!(
        listing,
        [
            ("a".to_owned(), EntryKind::File),
            ("big-link".to_owned(), EntryKind::Symlink),
            ("dir-link".to_owned(), EntryKind::Symlink),
            ("sub".to_owned(), EntryKind::Dir),
        ]
    );
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    use kipple_core::Allocated;

    use super::*;

    #[expect(
        clippy::unwrap_used,
        reason = "a fixture that fails to build fails the test"
    )]
    fn allocated(path: &Path) -> u64 {
        fs::symlink_metadata(path).unwrap().blocks() * 512
    }

    #[test]
    fn a_file_hard_linked_from_outside_is_counted_once_and_not_as_reclaimable() {
        let temp = TempDir::new().unwrap();
        let item = temp.path().join("item");
        write(&item.join("shared"), 64 << 10);
        write(&item.join("own"), 64 << 10);
        fs::hard_link(item.join("shared"), temp.path().join("kept-elsewhere")).unwrap();
        fs::hard_link(item.join("own"), item.join("own-again")).unwrap();

        let size = Walker::new()
            .unwrap()
            .size(&item, &CancelToken::new())
            .unwrap()
            .size;

        // `own` has two names inside the item: one file, freed with the item.
        assert_eq!(size.apparent, Apparent(128 << 10));
        let allocated_total = size.allocated.unwrap().0;
        let shared = allocated(&item.join("shared"));
        assert_eq!(
            size.unique_reclaim,
            Some(Allocated(allocated_total - shared))
        );
        assert!(shared > 0);
    }

    #[test]
    fn a_sparse_file_counts_its_length_as_apparent_but_only_its_blocks_as_allocated() {
        let temp = TempDir::new().unwrap();
        let sparse = fs::File::create(temp.path().join("sparse")).unwrap();
        sparse.set_len(256 << 20).unwrap();

        let size = Walker::new()
            .unwrap()
            .size(temp.path(), &CancelToken::new())
            .unwrap()
            .size;

        assert_eq!(size.apparent, Apparent(256 << 20));
        assert!(size.allocated.unwrap().0 < 1 << 20);
    }

    #[test]
    fn an_unreadable_directory_makes_the_estimate_incomplete() {
        let temp = TempDir::new().unwrap();
        write(&temp.path().join("locked/secret"), 10);
        let locked = temp.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let size = Walker::new()
            .unwrap()
            .size(temp.path(), &CancelToken::new());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();

        let completeness = size.unwrap().size.completeness;
        assert_eq!(completeness, Completeness::Incomplete { unreadable: 1 });
    }
}
