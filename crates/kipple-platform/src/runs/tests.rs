//! The run store's lock across processes, a full disk, and the space check: what needs
//! the store's insides or the platform runner. Each test names what it catches. The rest
//! is in the `native` test target.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kipple_core::ToolError;

use super::*;
use crate::environment::Environment;
use crate::runner;

/// A clock stopped at 2026-10-30T14:25:01Z.
#[derive(Debug)]
struct Stopped;

impl Clock for Stopped {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_793_370_301)
    }
}

fn store(data: &Path) -> FileRunStore {
    FileRunStore::new(Some(data.to_owned()), Arc::new(Stopped))
}

/// The data directory a child process was given: its last argument.
fn given() -> PathBuf {
    let last: Option<OsString> = std::env::args_os().last();
    PathBuf::from(last.unwrap())
}

/// Runs this test binary's ignored test `name` with `data` as its argument, waiting at
/// most `deadline`.
fn child(name: &str, data: &Path, deadline: Duration) -> Result<runner::Output, ToolError> {
    let unusable = |error| ToolError::Start(Arc::new(error));
    let exe = std::env::current_exe().map_err(unusable)?;
    let data = data
        .to_str()
        .ok_or_else(|| unusable(io::ErrorKind::InvalidInput.into()))?;
    let test = format!("runs::tests::{name}");
    let args = ["--exact", test.as_str(), "--ignored", data];
    runner::run(&exe, &args, &Environment::default(), deadline)
}

/// Not a test: the second process of
/// `a_second_process_waits_a_second_for_the_lock_then_refuses`.
#[test]
#[ignore = "started by a_second_process_waits_a_second_for_the_lock_then_refuses"]
fn waits_for_the_held_lock() {
    let store = store(&given());
    assert_eq!(store.lock_state(), Ok(LockState::Held));
    let started = Instant::now();

    let refused = store.lock(&CancelToken::new());

    assert!(
        matches!(refused, Err(StoreRefusal::Busy)),
        "a held lock refuses: {refused:?}"
    );
    assert!(
        started.elapsed() >= LOCK_WAIT,
        "it waited {:?}",
        started.elapsed()
    );
}

/// Catches two mutating commands running at once (pinned 10): while one process holds
/// the lock, another waits [`LOCK_WAIT`] and refuses as busy, and `history` in it sees
/// the lock held.
#[test]
fn a_second_process_waits_a_second_for_the_lock_then_refuses() {
    let temp = tempfile::TempDir::new().unwrap();
    let store = store(temp.path());
    let held = store.lock(&CancelToken::new()).unwrap();

    let output = child(
        "waits_for_the_held_lock",
        temp.path(),
        Duration::from_secs(20),
    )
    .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    drop(held);
    assert_eq!(store.lock_state(), Ok(LockState::Free));
}

/// Not a test: the process of `a_lock_whose_holder_died_is_free_again`. It takes the
/// lock, says so, and waits until it is killed.
#[test]
#[ignore = "started by a_lock_whose_holder_died_is_free_again"]
fn holds_the_lock_until_killed() {
    let data = given();
    let _held = store(&data).lock(&CancelToken::new()).unwrap();
    fs::write(data.join("held"), b"").unwrap();
    loop {
        thread::park();
    }
}

/// Catches a lock that outlives the process holding it, as a lock file's mere presence
/// would: a crashed apply would block every later one.
#[test]
fn a_lock_whose_holder_died_is_free_again() {
    let temp = tempfile::TempDir::new().unwrap();
    let data = temp.path().to_owned();
    let holder =
        thread::spawn(move || child("holds_the_lock_until_killed", &data, Duration::from_secs(5)));
    let store = store(temp.path());
    let started = Instant::now();
    while !temp.path().join("held").exists() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the child took the lock"
        );
        thread::park_timeout(RETRY);
    }
    assert_eq!(store.lock_state(), Ok(LockState::Held));

    let ended = holder.join().unwrap();

    assert!(
        ended.is_err(),
        "the child was killed at its deadline: {ended:?}"
    );
    // Windows may take a moment to release a dead process's locks: `lock` waits for it.
    assert!(
        store.lock(&CancelToken::new()).is_ok(),
        "the lock is taken again"
    );
}

/// How a [`Failing`] journal fails.
#[derive(Debug, Clone, Copy)]
enum Fault {
    /// The disk fills after this many more bytes: a write writes what fits, then fails.
    Full(usize),
    /// Every sync fails.
    Sync,
}

/// A journal file on a disk that fails as `fault` says.
#[derive(Debug)]
struct Failing {
    file: File,
    fault: Fault,
}

impl Write for Failing {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Fault::Full(room) = &mut self.fault else {
            return self.file.write(buf);
        };
        if *room == 0 {
            return Err(io::ErrorKind::StorageFull.into());
        }
        let fits = buf.len().min(*room);
        *room -= fits;
        self.file.write(&buf[..fits])
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for Failing {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.file.seek(to)
    }
}

impl Log for Failing {
    fn size(&self) -> io::Result<u64> {
        self.file.size()
    }

    fn cut(&self, len: u64) -> io::Result<()> {
        self.file.cut(len)
    }

    fn sync(&self) -> io::Result<()> {
        match self.fault {
            Fault::Sync => Err(io::ErrorKind::Other.into()),
            Fault::Full(_) => self.file.sync(),
        }
    }
}

/// Catches a record a full disk or a failed sync left half-written (pinned 8): the
/// append fails, and the journal is cut back to the sound records, the torn tail it was
/// asked to write over gone too.
#[test]
fn a_failed_append_leaves_only_the_sound_records() {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().join(JOURNAL);
    let sound = b"{\"one\":1}\n";
    for fault in [Fault::Full(0), Fault::Full(5), Fault::Sync] {
        fs::write(&path, [&sound[..], b"{\"torn"].concat()).unwrap();
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        let mut journal = Failing { file, fault };

        let failed = append_at(&mut journal, sound.len() as u64, b"{\"two\":2}\n");

        assert_eq!(
            failed.map_err(|error| error.code),
            Err(ErrorCode::Journal),
            "{fault:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), sound, "{fault:?}");
    }
}

/// Catches a plan stored on a disk with no room for it, or checked only after its run
/// was created (pinned 7): the store refuses with the space needed and free, and no run
/// is left behind.
#[test]
fn a_plan_larger_than_the_free_space_is_refused_before_anything_is_written() {
    let temp = tempfile::TempDir::new().unwrap();
    let store = store(temp.path());
    let lock = store.lock(&CancelToken::new()).unwrap();
    let runs = Folder::open(&temp.path().join(RUNS)).unwrap();
    let (plan, first) = (vec![b'p'; 4096], b"first\n");

    let refused = store.create_in(&runs, 4096, &plan, first);

    assert_eq!(
        refused,
        Err(StoreRefusal::Space {
            needed: 4096 + 6,
            free: 4096
        })
    );
    assert_eq!(store.runs(), Ok(Vec::new()));
    assert!(
        store.create(&lock, &plan, first).is_ok(),
        "a disk with room stores it"
    );
}

/// Catches a journal or plan opened without write-through, which a power loss could
/// lose after `append` returned (pinned 7, Windows).
#[cfg(windows)]
#[test]
fn the_runs_files_are_written_through() {
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_MODE_INFORMATION, FILE_WRITE_THROUGH, FileModeInformation, NtQueryInformationFile,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    #[expect(
        unsafe_code,
        reason = "windows-sys exposes NtQueryInformationFile as a raw call"
    )]
    fn mode(file: &File) -> u32 {
        let mut mode = FILE_MODE_INFORMATION::default();
        let mut status = IO_STATUS_BLOCK::default();
        // SAFETY: the handle is open and `mode` is the struct the class names.
        let result = unsafe {
            NtQueryInformationFile(
                file.as_raw_handle(),
                &raw mut status,
                (&raw mut mode).cast(),
                u32::try_from(size_of::<FILE_MODE_INFORMATION>()).unwrap(),
                FileModeInformation,
            )
        };
        assert!(result >= 0, "{result:#x}");
        mode.Mode
    }

    let temp = tempfile::TempDir::new().unwrap();
    let folder = Folder::open(temp.path()).unwrap();

    let created = folder.create(JOURNAL).unwrap();
    assert_ne!(mode(&created) & FILE_WRITE_THROUGH, 0, "a new file");
    drop(created);
    let appended = folder.write(JOURNAL).unwrap();
    assert_ne!(
        mode(&appended) & FILE_WRITE_THROUGH,
        0,
        "a file appended to"
    );
}
