//! The release gate (spec M4 pinned 39): what `builtin.codex.stale-releases` rests on, on
//! the real OS. A lock held by a second process, as Codex's installer holds one, and the
//! kernel letting it go when that process dies; the stamp `selection-history` records, read
//! from a real `lstat`; and a same-device bind mount told apart by its mount ID. Linux
//! ships the rule, so its step is required; macOS and Windows show what they have.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that fails to build fails the test"
)]

use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use kipple_core::{FsProbe as _, LockGap, Mount, StoreLocks as _};
use kipple_platform::{FileLocks, NoFollowFs};
use tempfile::TempDir;

/// Names the lock [`hold_a_lock_for_another_process`] holds, in the process it runs in.
const HOLD: &str = "KIPPLE_TEST_HOLD_LOCK";

/// What starts the line the second process says whether it holds the lock on.
const SAID: &str = "kipple-gate-lock: ";

/// The second process: holds the lock `HOLD` names, says so, and waits for its parent to
/// close its input or kill it. Run as an ordinary test, it does nothing.
#[test]
fn hold_a_lock_for_another_process() {
    let Some(lock) = std::env::var_os(HOLD).map(PathBuf::from) else {
        return;
    };
    let held = FileLocks.try_hold(std::slice::from_ref(&lock));
    let line = format!("{SAID}{:?}\n", held.as_ref().err());
    let mut stdout = std::io::stdout();
    stdout.write_all(line.as_bytes()).unwrap();
    stdout.flush().unwrap();
    let mut rest = Vec::new();
    let _closed = std::io::stdin().read_to_end(&mut rest);
    drop(held);
}

/// This test binary again, running only [`hold_a_lock_for_another_process`] on `lock`.
#[expect(
    clippy::disallowed_methods,
    reason = "the gate needs a second process to hold the lock, and it is this test binary"
)]
fn second_process(lock: &Path) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "gate::hold_a_lock_for_another_process",
            "--nocapture",
        ])
        .env(HOLD, lock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Waits until `child` said whether it holds the lock, and returns what it said.
fn said(child: &mut Child) -> String {
    let stdout = BufReader::new(child.stdout.take().unwrap());
    for line in stdout.lines() {
        if let Some(said) = line.unwrap().strip_prefix(SAID) {
            return said.to_owned();
        }
    }
    panic!("the second process ended without a word");
}

/// Catches a removal beside Codex's installer: a lock another process holds is `Held`,
/// and the kernel frees it when that process dies, so a crash never leaves a store locked
/// for good. Off Linux no lock is held at all.
#[test]
fn store_lock_held_by_a_second_process_skips() {
    let dir = TempDir::new().unwrap();
    let lock = dir.path().join("install.lock");
    fs::write(&lock, b"").unwrap();
    let mut child = second_process(&lock);

    let their_answer = said(&mut child);
    let ours = FileLocks.try_hold(std::slice::from_ref(&lock)).err();
    child.kill().unwrap();
    child.wait().unwrap();
    let after = FileLocks.try_hold(std::slice::from_ref(&lock)).err();

    if their_answer == "None" {
        assert_eq!(ours, Some(LockGap::Held), "held by the second process");
        assert_eq!(after, None, "freed when it died");
    } else {
        assert_eq!(their_answer, format!("{:?}", Some(LockGap::Unsupported)));
        assert_eq!(ours, Some(LockGap::Unsupported));
    }
}

/// Catches the stamp read through the link, or in another form than Codex writes, which
/// would read every `selection-history` as stale or a stale one as current: `<st_dev>
/// <st_ino> <ctime_sec>.<ctime_nsec>` of the link itself.
#[cfg(unix)]
#[test]
fn link_stamp_reads_a_real_lstat() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = TempDir::new().unwrap();
    let release = dir
        .path()
        .join("releases/0.166.0-x86_64-unknown-linux-musl");
    fs::create_dir_all(&release).unwrap();
    let current = dir.path().join("current");
    std::os::unix::fs::symlink(&release, &current).unwrap();

    let stamp = NoFollowFs.link_stamp(&current).unwrap();

    let link = fs::symlink_metadata(&current).unwrap();
    let expected = format!(
        "{} {} {}.{:09}",
        link.dev(),
        link.ino(),
        link.ctime(),
        link.ctime_nsec()
    );
    assert_eq!(stamp, expected);
    let target = fs::metadata(&current).unwrap();
    assert_ne!(target.ino(), link.ino(), "the link, not its target");
    assert!(
        NoFollowFs.link_stamp(&release).is_err(),
        "expected a directory to have no stamp"
    );
}

/// Catches a stamp made up where the OS keeps none Codex can read.
#[cfg(windows)]
#[test]
fn link_stamp_reads_a_real_lstat() {
    let dir = TempDir::new().unwrap();
    assert!(
        NoFollowFs.link_stamp(dir.path()).is_err(),
        "expected Windows to have no stamp"
    );
}

/// Catches a same-device bind mount inside a release passing for part of it: its device
/// is the store's, so only its mount ID shows it is another mount (S5 codex §7 row 7).
/// Off Linux the device tells mounts apart, which the other mount tests hold.
#[test]
fn a_bind_mount_has_its_own_mount_id() {
    let dir = TempDir::new().unwrap();
    let here = NoFollowFs.metadata(dir.path()).unwrap().mount;
    let Some(holder) = std::env::var_os("KIPPLE_TEST_BIND_MOUNT").map(PathBuf::from) else {
        assert!(
            matches!(here, Mount::Id(_) | Mount::ByDevice),
            "expected a mount read on an ordinary disk, got {here:?}"
        );
        return;
    };
    let above = NoFollowFs.metadata(&holder).unwrap();
    let inside = NoFollowFs.metadata(&holder.join("inside")).unwrap();

    assert_eq!(
        above.identity.map(|identity| identity.volume),
        inside.identity.map(|identity| identity.volume),
        "the bind mount shares its device"
    );
    assert!(
        matches!((above.mount, inside.mount), (Mount::Id(a), Mount::Id(b)) if a != b),
        "expected two mount IDs, got {:?} and {:?}",
        above.mount,
        inside.mount
    );
}
