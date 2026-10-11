//! A home trash as the freedesktop.org Trash specification 1.0 lays it out: each trashed
//! file in `files`, with its original path and deletion date in `info`.

use super::tree::FixtureTree;

/// A trash at `at` holding one file.
pub(crate) fn trash(tree: &mut FixtureTree, at: &str) {
    tree.sized(&format!("{at}/files/old-video.mkv"), 64 * 1024 * 1024)
        .file(
            &format!("{at}/info/old-video.mkv.trashinfo"),
            b"[Trash Info]\nPath=/home/demo/old-video.mkv\nDeletionDate=2026-09-01T10:00:00\n",
        );
}
