//! `FixtureTree`'s links and directory times, which a fixture relies on being real: an
//! integration that reads a link or a directory's time would pass over a tree that only
//! looks right (`docs/06-adapters-and-rules.md` §7).

#[path = "fixtures/tree.rs"]
mod tree;

use std::fs;
use std::io;
use std::time::{Duration, SystemTime};

use tree::FixtureTree;

/// Catches a link followed and copied, which reads the same but isn't a link, and on
/// Windows a link of the wrong kind, which can't be listed through. The links come first
/// to catch one made before its target.
#[test]
fn a_link_is_a_real_link_to_its_target() {
    let mut tree = FixtureTree::new();
    tree.link("links/dir", "real/dir")
        .link("links/file", "real/file.txt")
        .file("real/dir/inner.txt", b"inner")
        .file("real/file.txt", b"file");
    let temp = tempfile::tempdir().unwrap();
    tree.build_in(temp.path()).unwrap();

    let dir = temp.path().join("links/dir");
    let file = temp.path().join("links/file");
    for (link, target) in [(&dir, "real/dir"), (&file, "real/file.txt")] {
        let kind = fs::symlink_metadata(link).unwrap().file_type();
        assert!(kind.is_symlink(), "{} should be a link", link.display());
        let held = fs::read_link(link).unwrap();
        assert!(
            held.is_absolute() && held.ends_with(target),
            "{} should hold the absolute path of {target}, not {}",
            link.display(),
            held.display()
        );
    }
    let listed: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(listed, ["inner.txt"]);
    assert_eq!(fs::read(&file).unwrap(), b"file");
}

/// Catches a dangling link made as an empty file, which an integration would read as a
/// sound one.
#[test]
fn a_bad_link_is_a_link_to_nothing() {
    let mut tree = FixtureTree::new();
    tree.bad_link("store/current");
    let temp = tempfile::tempdir().unwrap();
    tree.build_in(temp.path()).unwrap();

    let link = temp.path().join("store/current");
    let kind = fs::symlink_metadata(&link).unwrap().file_type();
    assert!(kind.is_symlink(), "a bad link should be a link");
    assert_eq!(
        fs::metadata(&link).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

/// Catches a directory time set before a later child is written, which overwrites it.
/// The time is asked for first, and each kind of child comes after it.
#[test]
fn a_directory_time_survives_its_children_being_written() {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    let mut tree = FixtureTree::new();
    tree.modified_dir("store", at)
        .modified_dir("store/nested", at)
        .file("store/nested/file", b"")
        .sized("store/sized", 1)
        .dir("store/empty")
        .link("store/link", "store/nested")
        .bad_link("store/bad")
        .modified("store", at + Duration::from_secs(60));
    let temp = tempfile::tempdir().unwrap();
    tree.build_in(temp.path()).unwrap();

    for dir in ["store", "store/nested"] {
        let meta = fs::metadata(temp.path().join(dir)).unwrap();
        assert_eq!(meta.modified().unwrap(), at, "{dir}");
    }
}

/// Catches a stamp read through the link, which describes its target and would never
/// match what the link's own `lstat` reports.
#[test]
#[cfg(unix)]
fn a_link_stamp_is_the_links_own_lstat() {
    use std::os::unix::fs::MetadataExt as _;

    let mut tree = FixtureTree::new();
    tree.file("releases/1.0.0/bin", b"")
        .link("current", "releases/1.0.0");
    let temp = tempfile::tempdir().unwrap();
    tree.build_in(temp.path()).unwrap();

    let link = temp.path().join("current");
    let stamp = FixtureTree::link_stamp(&link).unwrap();
    let meta = fs::symlink_metadata(&link).unwrap();
    let nanos = format!("{:09}", meta.ctime_nsec());
    let expected = format!("{} {} {}.{nanos}", meta.dev(), meta.ino(), meta.ctime());
    assert_eq!(stamp, expected);
    let target = fs::metadata(&link).unwrap();
    assert_ne!(meta.ino(), target.ino());
    let error = FixtureTree::link_stamp(&temp.path().join("releases")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}
