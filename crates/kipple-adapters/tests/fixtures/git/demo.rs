//! What `demo-home` shows of git worktrees. `demo_home.rs` calls this, beside a `v2_56_0`
//! module. The demo tree's place isn't known here, so its links are relative.

use super::tree::FixtureTree;
use super::v2_56_0::{self, APP, Links};

/// The worktree that has a commit: Codex's `cd34`, on branch `done`, clean.
pub(crate) const DONE: &str = "home/.codex/worktrees/cd34/app";

/// The empty tree, which the demo's one commit records.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The demo's one commit, of [`EMPTY_TREE`], with fixed names and times.
const COMMIT: &str = "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
author Demo <demo@example.invalid> 1767225600 +0000\n\
committer Demo <demo@example.invalid> 1767225600 +0000\n\nDone\n";

/// [`COMMIT`]'s object ID, the SHA-1 of its loose object before compression. An edit to
/// the commit recomputes it (`git hash-object -t commit`); nothing here can.
const COMMIT_ID: &str = "33a5a2afa3b344388435a334337c4a8d0a22e421";

/// Adds S5 git §8's eight worktrees to the demo tree, and one more that has a commit, so
/// the Linux demo has a worktree kipple can remove.
pub(crate) fn demo(tree: &mut FixtureTree) {
    v2_56_0::eight(tree, Links::Relative);
    done(tree);
}

/// A clean worktree of [`APP`] at [`DONE`], whose branch `done` holds one commit of the
/// empty tree. Its objects are written as git stores them, so git reads the repository
/// with nothing run to build it, and the bytes are the same on every system.
fn done(tree: &mut FixtureTree) {
    v2_56_0::linked(tree, APP, "cd34", DONE, Links::Relative);
    // Written after the admin entry `linked` makes, so this HEAD is the one built.
    tree.file(
        &format!("{APP}/.git/worktrees/cd34/HEAD"),
        b"ref: refs/heads/done\n",
    )
    .file(
        &format!("{APP}/.git/refs/heads/done"),
        format!("{COMMIT_ID}\n").as_bytes(),
    );
    loose(tree, EMPTY_TREE, "tree", b"");
    loose(tree, COMMIT_ID, "commit", COMMIT.as_bytes());
}

/// The loose object `id` of `kind` holding `body`, in [`APP`]'s object store.
fn loose(tree: &mut FixtureTree, id: &str, kind: &str, body: &[u8]) {
    let mut object = format!("{kind} {}\0", body.len()).into_bytes();
    object.extend_from_slice(body);
    let (dir, file) = id.split_at(2);
    tree.file(
        &format!("{APP}/.git/objects/{dir}/{file}"),
        &zlib_stored(&object),
    );
}

/// `data` as a zlib stream of one uncompressed block (RFC 1950, RFC 1951 §3.2.4), which
/// every zlib reads and needs no compressor to write.
#[expect(
    clippy::cast_possible_truncation,
    reason = "one block holds 64 KiB, and the demo's objects are a few hundred bytes"
)]
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let len = data.len() as u16;
    let mut stream = vec![0x78, 0x01, 0x01];
    stream.extend_from_slice(&len.to_le_bytes());
    stream.extend_from_slice(&(!len).to_le_bytes());
    stream.extend_from_slice(data);
    stream.extend_from_slice(&adler32(data).to_be_bytes());
    stream
}

/// The Adler-32 checksum zlib ends a stream with.
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let (a, b) = data.iter().fold((1_u32, 0_u32), |(a, b), &byte| {
        let a = (a + u32::from(byte)) % MOD;
        (a, (b + a) % MOD)
    });
    (b << 16) | a
}
