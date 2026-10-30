//! Mount boundaries and unsupported file systems, on real mounts the CI job prepares
//! before the tests run (spec pinned 13): another volume mounted inside a directory, a
//! bind mount of the same device (Linux), and a FAT volume. Each mount is named by a
//! variable; with `KIPPLE_TEST_MOUNTS_REQUIRED=1`, as in CI, a missing one fails.
//!
//! - `KIPPLE_TEST_OTHER_MOUNT`: a directory whose entry `other` is another volume's mount
//!   point. On Windows `KIPPLE_TEST_OTHER_VOLUME` names that volume's root (a drive
//!   letter), since a Windows root is opened as itself, never through a mount point.
//! - `KIPPLE_TEST_BIND_MOUNT` (Linux): a directory whose entry `inside` is a bind mount of
//!   a directory on the same device.
//! - `KIPPLE_TEST_FAT`: a writable directory on FAT.
//! - `KIPPLE_TEST_REFS` (Windows, optional): a writable directory on `ReFS`.

use std::path::PathBuf;

use kipple_core::{
    CancelToken, DeleteOp, ErrorCode, FileSystemClass, Freezer as _, FsProbe as _, ItemOutcome,
    Marks, Unsupported,
};
use kipple_platform::{ConfinedFs, NoFollowFs};
use tempfile::TempDir;

use super::support::{anchor_at, delete, freeze, present, run, write};

/// The directory variable `name` names, or `None` to skip where CI doesn't require it.
fn prepared(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os(name).map(PathBuf::from);
    assert!(
        path.is_some() || std::env::var_os("KIPPLE_TEST_MOUNTS_REQUIRED").is_none(),
        "{name} must name a prepared mount"
    );
    path
}

/// Catches a freeze that walks into another volume mounted inside the target.
#[test]
fn a_freeze_stops_at_another_volume_mounted_inside() {
    let Some(holder) = prepared("KIPPLE_TEST_OTHER_MOUNT") else {
        return;
    };

    let refused = ConfinedFs
        .freeze(&anchor_at(&holder, ""), &CancelToken::new())
        .unwrap_err();

    assert_eq!(refused.code, ErrorCode::MountBoundary, "{refused:?}");
}

/// Catches an executor that reaches a target through another mount: the target was
/// frozen on that mount, then named through the directory it is mounted on.
#[test]
fn a_target_reached_through_another_mount_is_refused() {
    let Some(holder) = prepared("KIPPLE_TEST_OTHER_MOUNT") else {
        return;
    };
    let volume = std::env::var_os("KIPPLE_TEST_OTHER_VOLUME")
        .map_or_else(|| holder.join("other"), PathBuf::from);
    refused_through(&volume, &holder, "other");
}

/// Catches mount detection by device alone: a bind mount of the same device is another
/// mount (spec pinned 5).
#[cfg(target_os = "linux")]
#[test]
fn a_freeze_stops_at_a_bind_mount_of_the_same_device() {
    let Some(holder) = prepared("KIPPLE_TEST_BIND_MOUNT") else {
        return;
    };
    let (above, inside) = (
        NoFollowFs.metadata(&holder).unwrap().identity.unwrap(),
        NoFollowFs
            .metadata(&holder.join("inside"))
            .unwrap()
            .identity
            .unwrap(),
    );
    assert_eq!(
        above.volume, inside.volume,
        "the bind mount is on the same device"
    );

    let refused = ConfinedFs
        .freeze(&anchor_at(&holder, ""), &CancelToken::new())
        .unwrap_err();

    assert_eq!(refused.code, ErrorCode::MountBoundary, "{refused:?}");
}

/// Catches an executor that reaches a target through a same-device bind mount.
#[cfg(target_os = "linux")]
#[test]
fn a_target_reached_through_a_bind_mount_is_refused() {
    let Some(holder) = prepared("KIPPLE_TEST_BIND_MOUNT") else {
        return;
    };
    refused_through(&holder.join("inside"), &holder, "inside");
}

/// Freezes a fresh target on the mount `mounted`, then runs it as if reached from
/// `holder` through `component`: the run must stop at the mount and remove nothing.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]
fn refused_through(mounted: &std::path::Path, holder: &std::path::Path, component: &str) {
    let temp = TempDir::new_in(mounted).unwrap();
    let name = temp
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    write(&temp.path().join("inner/x"), b"on the other mount");
    let tree = freeze(&anchor_at(mounted, &format!("{name}/inner"))).tree;
    let through = anchor_at(holder, &format!("{component}/{name}/inner"));
    let marks = Marks::of(&tree);

    let outcome = run(&DeleteOp::for_platform_tests(through, tree, marks));

    assert!(
        matches!(&outcome, ItemOutcome::Failed { error, .. } if error.code == ErrorCode::MountBoundary),
        "{outcome:?}"
    );
    assert!(present(&temp.path().join("inner/x")), "inner/x stays");
}

/// Catches a delete on FAT, which can't remove the name of an open file (spec pinned 6).
#[test]
fn nothing_is_removed_on_fat() {
    let Some(volume) = prepared("KIPPLE_TEST_FAT") else {
        return;
    };
    let temp = TempDir::new_in(&volume).unwrap();
    write(&temp.path().join("target/x"), b"on FAT");
    // Windows may not give a FAT file the 128-bit ID kipple identifies entries by, and
    // then no root there is ever verified: refused before anything is frozen.
    if NoFollowFs.metadata(temp.path()).unwrap().identity.is_none() {
        return;
    }
    let anchor = anchor_at(temp.path(), "target");

    let frozen = freeze(&anchor);
    assert_eq!(
        frozen.file_system,
        FileSystemClass::Unsupported(Unsupported::NoPosixDelete)
    );
    let outcome = delete(&anchor, frozen.tree);

    assert!(
        matches!(&outcome, ItemOutcome::Failed { error, .. } if error.code == ErrorCode::UnsupportedFileSystem),
        "{outcome:?}"
    );
    assert!(present(&temp.path().join("target/x")), "target/x stays");
}

/// Catches 128-bit `ReFS` file IDs (Dev Drive) being mishandled where the runner can make
/// such a volume.
#[cfg(windows)]
#[test]
fn a_tree_on_refs_is_frozen_and_removed() {
    let Some(volume) = std::env::var_os("KIPPLE_TEST_REFS").map(PathBuf::from) else {
        return;
    };
    let temp = TempDir::new_in(&volume).unwrap();
    write(&temp.path().join("target/sub/x"), b"on ReFS");
    let anchor = anchor_at(temp.path(), "target");

    let frozen = freeze(&anchor);
    assert_eq!(
        frozen.file_system,
        FileSystemClass::Supported(kipple_core::LocalFileSystem::Refs)
    );
    let outcome = delete(&anchor, frozen.tree);

    assert_eq!(outcome.status(), kipple_core::Status::Done, "{outcome:?}");
    assert!(!present(&temp.path().join("target")), "target is gone");
}
