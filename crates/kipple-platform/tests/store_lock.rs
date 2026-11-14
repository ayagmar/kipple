//! The platform's [`StoreLocks`] on files in a temp dir (spec M4 pinned 13). Linux takes
//! the `flock` Codex's installer takes; macOS and Windows hold none, which leaves every
//! release alone there. Holders here are two descriptors of one process, which `flock`
//! keeps apart as it keeps two processes apart; the `native` profile's gate holds one from a
//! second process.
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]

use std::fs;
use std::path::{Path, PathBuf};

use kipple_core::{LockGap, StoreLocks as _};
use kipple_platform::FileLocks;
use tempfile::TempDir;

/// An empty lock file named `name` in `dir`, as the installer leaves one.
fn lock_file(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, b"").unwrap();
    path
}

/// Catches a lock taken while the installer holds it, and one never let go: a lock held
/// elsewhere is `Held` until its holder lets go, and then it is free.
#[cfg(target_os = "linux")]
#[test]
fn held_install_lock_skips() {
    let dir = TempDir::new().unwrap();
    let lock = lock_file(dir.path(), "install.lock");

    let installer = FileLocks.try_hold(std::slice::from_ref(&lock)).unwrap();
    let busy = FileLocks.try_hold(std::slice::from_ref(&lock)).err();
    drop(installer);
    let free = FileLocks.try_hold(std::slice::from_ref(&lock));

    assert_eq!(busy, Some(LockGap::Held));
    assert!(free.is_ok(), "expected the lock free once let go: {free:?}");
}

/// Catches a missing lock being created, which makes a store kipple never read look like
/// one, or read as free.
#[cfg(target_os = "linux")]
#[test]
fn missing_lock_file_skips() {
    let dir = TempDir::new().unwrap();
    let lock = dir.path().join("install.lock");

    let held = FileLocks.try_hold(std::slice::from_ref(&lock)).err();

    assert_eq!(held, Some(LockGap::Missing));
    assert!(!lock.exists(), "expected no lock file made");
    assert_eq!(FileLocks.try_hold(&[]).err(), Some(LockGap::Missing));
}

/// Catches the daemon's store held under one of its two locks, and the one taken kept
/// after the other failed: every lock is held, or none.
#[cfg(target_os = "linux")]
#[test]
fn held_daemon_lock_skips() {
    let dir = TempDir::new().unwrap();
    let install = lock_file(dir.path(), "install.lock");
    let daemon = lock_file(dir.path(), "daemon.lock");
    let both = [install.clone(), daemon.clone()];

    let updater = FileLocks.try_hold(std::slice::from_ref(&daemon)).unwrap();
    let busy = FileLocks.try_hold(&both).err();
    let install_free = FileLocks.try_hold(std::slice::from_ref(&install));
    drop(install_free);
    drop(updater);
    let held = FileLocks.try_hold(&both);

    assert_eq!(busy, Some(LockGap::Held));
    let free = FileLocks.try_hold(std::slice::from_ref(&install)).err();
    assert_eq!(free, Some(LockGap::Held), "both held together");
    drop(held);
}

/// Makes a FIFO at `path`.
#[cfg(target_os = "linux")]
#[expect(unsafe_code, reason = "libc's mkfifo is a raw call")]
fn make_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is NUL-terminated and lives across the call.
    let made = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(made, 0, "mkfifo failed");
}

/// Catches a link in a lock's place being followed to a file kipple never read, and a
/// FIFO there leaving kipple waiting for a writer: neither is a lock.
#[cfg(target_os = "linux")]
#[test]
fn a_lock_that_isnt_a_plain_file_is_unreadable() {
    let dir = TempDir::new().unwrap();
    let real = lock_file(dir.path(), "real.lock");
    let link = dir.path().join("install.lock");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let fifo = dir.path().join("fifo.lock");
    make_fifo(&fifo);
    let lock_dir = dir.path().join("install.lock.d");
    fs::create_dir(&lock_dir).unwrap();

    assert_eq!(FileLocks.try_hold(&[link]).err(), Some(LockGap::Unreadable));
    assert_eq!(
        FileLocks.try_hold(&[lock_dir]).err(),
        Some(LockGap::Unreadable)
    );
    assert_eq!(FileLocks.try_hold(&[fifo]).err(), Some(LockGap::Unreadable));
}

/// Catches an OS without a lock known to match Codex's taking none and going on: there
/// every release is left alone.
#[cfg(not(target_os = "linux"))]
#[test]
fn no_lock_is_held_off_linux() {
    let dir = TempDir::new().unwrap();
    let lock = lock_file(dir.path(), "install.lock");

    let held = FileLocks.try_hold(&[lock]).err();

    assert_eq!(held, Some(LockGap::Unsupported));
}
