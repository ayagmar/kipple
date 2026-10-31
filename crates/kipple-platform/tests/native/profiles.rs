//! The profile store on the real file system (spec M3c pinned 11): what it stores reads
//! back byte for byte and only the user can read it, a replacement is atomic, what a
//! crash leaves is never read as a profile, and it refuses what it can't trust. Each test
//! names what it catches.
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use kipple_core::profile::{ProfileName, ProfileStore as _, ProfileStoreError};
use kipple_core::{ErrorCode, Unsupported};
use kipple_platform::FileProfileStore;
use tempfile::TempDir;

use crate::executor::support::{link, present};

fn store(data: &Path) -> FileProfileStore {
    FileProfileStore::new(Some(data.to_owned()))
}

fn name(text: &str) -> ProfileName {
    ProfileName::parse(text).unwrap()
}

/// The names in the directory at `path`, sorted.
fn names(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Catches a profile that doesn't read back as stored, one kept under another name,
/// files others can read, or a store that makes its folders just to read or list.
#[test]
fn a_profile_reads_back_as_written_and_only_the_user_can_read_it() {
    let temp = TempDir::new().unwrap();
    let data = temp.path().join("data");
    let store = store(&data);
    let daily = name("daily");
    assert_eq!(store.read(&daily), Ok(None), "no store yet is no profile");
    assert_eq!(store.list(), Ok(Vec::new()));
    assert!(!present(&data), "reading made nothing");

    store.create_new(&daily, b"{\"first\":1}\n").unwrap();
    store.create_new(&name("a-0"), b"{}").unwrap();

    assert_eq!(store.read(&daily), Ok(Some(b"{\"first\":1}\n".to_vec())));
    assert_eq!(store.list(), Ok(vec![name("a-0"), daily.clone()]));
    let profiles = data.join("profiles");
    assert_eq!(names(&profiles), ["a-0.json", "daily.json"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&data), 0o700);
        assert_eq!(mode(&profiles), 0o700);
        assert_eq!(mode(&profiles.join("daily.json")), 0o600);
    }

    store.replace(&daily, b"{\"second\":2}\n").unwrap();
    assert_eq!(store.read(&daily), Ok(Some(b"{\"second\":2}\n".to_vec())));
    store.remove(&daily).unwrap();
    assert_eq!(store.read(&daily), Ok(None));
    assert_eq!(names(&profiles), ["a-0.json"], "no temporary file is left");
}

/// Catches `profile create` overwriting a profile of the same name, or `review` and
/// `remove` making up one that isn't there.
#[test]
fn a_new_profile_never_replaces_one_and_a_missing_one_is_never_replaced_or_removed() {
    let temp = TempDir::new().unwrap();
    let store = store(temp.path());
    let daily = name("daily");
    store.create_new(&daily, b"kept").unwrap();

    assert_eq!(
        store.create_new(&daily, b"other"),
        Err(ProfileStoreError::Exists)
    );
    assert_eq!(store.read(&daily), Ok(Some(b"kept".to_vec())));
    assert_eq!(names(&temp.path().join("profiles")), ["daily.json"]);
    let weekly = name("weekly");
    assert_eq!(
        store.replace(&weekly, b"new"),
        Err(ProfileStoreError::Missing)
    );
    assert_eq!(store.remove(&weekly), Err(ProfileStoreError::Missing));
    assert_eq!(store.read(&weekly), Ok(None));
}

/// Catches a profile rewritten in place, which a crash or a full disk midway would leave
/// torn: the replacement is a new file renamed over the old one, so a reader that had
/// the old one open still reads it whole.
#[test]
fn a_replacement_is_a_new_file_renamed_over_the_old_one() {
    let temp = TempDir::new().unwrap();
    let store = store(temp.path());
    let daily = name("daily");
    store.create_new(&daily, b"old profile").unwrap();
    let held = fs::File::open(temp.path().join("profiles/daily.json")).unwrap();

    store.replace(&daily, b"new").unwrap();

    assert_eq!(io::read_to_string(held).unwrap(), "old profile");
    assert_eq!(store.read(&daily), Ok(Some(b"new".to_vec())));
    assert_eq!(names(&temp.path().join("profiles")), ["daily.json"]);
}

/// Catches a temporary file left under this process's PID (a crash, or a process with
/// the same PID in another PID namespace sharing the data directory) blocking every later
/// save of that profile, or being removed or renamed into place by it.
#[test]
fn a_temporary_name_this_pid_left_taken_doesnt_block_a_save_and_stays() {
    let temp = TempDir::new().unwrap();
    let store = store(temp.path());
    let profiles = temp.path().join("profiles");
    fs::create_dir(&profiles).unwrap();
    let pid = std::process::id();
    // The name an earlier release wrote to, then the ones this one tries first.
    let mut left = vec![format!("daily.json.{pid}.partial")];
    left.extend((0..3).map(|n| format!("daily.json.{pid}.{n}.partial")));
    for name in &left {
        fs::write(profiles.join(name), b"{\"torn").unwrap();
    }

    store.create_new(&name("daily"), b"{}").unwrap();
    store.replace(&name("daily"), b"{\"second\":2}").unwrap();

    assert_eq!(
        store.read(&name("daily")),
        Ok(Some(b"{\"second\":2}".to_vec()))
    );
    left.push("daily.json".to_owned());
    left.sort();
    assert_eq!(
        names(&profiles),
        left,
        "a taken name was removed or renamed"
    );
}

/// Catches what a crash leaves read or listed as a profile (pinned 11): a temporary file
/// written part-way before the rename, or a name that isn't a profile's, never is, and
/// they don't stop the next replacement or removal.
#[test]
fn what_a_crash_leaves_is_never_read_or_listed() {
    let temp = TempDir::new().unwrap();
    let store = store(temp.path());
    let daily = name("daily");
    store.create_new(&daily, b"old").unwrap();
    let profiles = temp.path().join("profiles");
    let strays = [
        "daily.json.4242.partial",
        "weekly.json.4242.partial",
        "Upper.json",
        "notes.txt",
    ];
    for stray in strays {
        fs::write(profiles.join(stray), b"{\"torn").unwrap();
    }

    assert_eq!(store.read(&daily), Ok(Some(b"old".to_vec())));
    assert_eq!(store.read(&name("weekly")), Ok(None));
    assert_eq!(store.list(), Ok(vec![daily.clone()]));

    store.replace(&daily, b"new").unwrap();
    assert_eq!(store.read(&daily), Ok(Some(b"new".to_vec())));
    store.remove(&daily).unwrap();
    assert_eq!(store.list(), Ok(Vec::new()));
    let mut left: Vec<String> = strays.iter().map(|&stray| stray.to_owned()).collect();
    left.sort();
    assert_eq!(names(&profiles), left, "only the store's own names changed");
}

/// Catches a store that follows a link put in place of the data directory, `profiles/`
/// or a profile (pinned 11): it would read authority from, write to or remove wherever
/// the link leads. A directory with a profile's name is refused too.
#[test]
fn a_link_in_place_of_the_data_directory_the_profiles_or_a_profile_is_refused() {
    let temp = TempDir::new().unwrap();
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join("daily.json"), b"not kipple's").unwrap();
    let daily = name("daily");
    let refused = |result: Result<(), ProfileStoreError>| matches!(result, Err(ProfileStoreError::Write(error)) if error.code == ErrorCode::ThroughLink);

    let linked_data = temp.path().join("linked-data");
    link(&elsewhere, &linked_data);
    assert!(
        refused(store(&linked_data).create_new(&daily, b"x")),
        "the data directory is a link"
    );
    let linked = temp.path().join("linked");
    fs::create_dir(&linked).unwrap();
    link(&elsewhere, &linked.join("profiles"));
    assert!(
        refused(store(&linked).create_new(&daily, b"x")),
        "profiles/ is a link"
    );
    assert!(store(&linked).read(&daily).is_err(), "read through a link");
    assert!(store(&linked).list().is_err(), "listed through a link");

    let real = temp.path().join("real");
    let store = store(&real);
    store.create_new(&name("other"), b"x").unwrap();
    link(
        &elsewhere.join("daily.json"),
        &real.join("profiles/daily.json"),
    );
    assert!(store.read(&daily).is_err(), "read through a link");
    assert!(
        store.replace(&daily, b"x").is_err(),
        "replaced through a link"
    );
    assert!(store.remove(&daily).is_err(), "a link removed");
    assert!(present(&real.join("profiles/daily.json")), "the link stays");
    fs::create_dir(real.join("profiles/weekly.json")).unwrap();
    let weekly = name("weekly");
    assert!(store.read(&weekly).is_err(), "a directory read");
    assert!(
        store.replace(&weekly, b"x").is_err(),
        "a directory replaced"
    );
    assert!(store.remove(&weekly).is_err(), "a directory removed");
    assert!(
        real.join("profiles/weekly.json").is_dir(),
        "the directory stays"
    );

    assert_eq!(names(&elsewhere), ["daily.json"], "nothing was made there");
    assert_eq!(
        fs::read(elsewhere.join("daily.json")).unwrap(),
        b"not kipple's"
    );
}

/// Catches a store used when kipple's data directory is unknown: unknown is never an
/// empty list or a place to write.
#[test]
fn an_unknown_data_directory_refuses() {
    let store = FileProfileStore::new(None);
    let daily = name("daily");

    assert_eq!(store.list(), Err(ProfileStoreError::NoDataDir));
    assert_eq!(store.read(&daily), Err(ProfileStoreError::NoDataDir));
    assert_eq!(
        store.create_new(&daily, b"x"),
        Err(ProfileStoreError::NoDataDir)
    );
    assert_eq!(
        store.replace(&daily, b"x"),
        Err(ProfileStoreError::NoDataDir)
    );
    assert_eq!(store.remove(&daily), Err(ProfileStoreError::NoDataDir));
}

/// Catches a profile written where a full disk stopped it, or a failed write that
/// replaced the old profile or left its temporary file (pinned 11). A file size limit
/// stands in for a full disk: no disk here fills on demand.
#[cfg(unix)]
#[test]
#[expect(unsafe_code, reason = "libc's setrlimit and signal are raw calls")]
fn a_write_the_disk_refuses_keeps_the_old_profile_and_leaves_nothing() {
    let temp = TempDir::new().unwrap();
    let store = store(temp.path());
    let daily = name("daily");
    store.create_new(&daily, b"old").unwrap();
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a live `rlimit` for both calls; the signal a write past the limit raises is
    // ignored, so the write fails with EFBIG instead of ending this test's process.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &raw mut limit), 0);
        limit.rlim_cur = 64;
        libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &raw const limit), 0);
    }
    let big = vec![b'x'; 4096];

    let replaced = store.replace(&daily, &big);
    let created = store.create_new(&name("weekly"), &big);

    assert!(
        matches!(replaced, Err(ProfileStoreError::Write(_))),
        "{replaced:?}"
    );
    assert!(
        matches!(created, Err(ProfileStoreError::Write(_))),
        "{created:?}"
    );
    assert_eq!(store.read(&daily), Ok(Some(b"old".to_vec())));
    assert_eq!(names(&temp.path().join("profiles")), ["daily.json"]);
}

/// Catches profiles kept on FAT, whose renames and syncs a profile can't rely on, or
/// refused only after `profiles/` was made there.
#[test]
fn a_store_on_fat_refuses() {
    let fat = std::env::var_os("KIPPLE_TEST_FAT").map(PathBuf::from);
    assert!(
        fat.is_some() || std::env::var_os("KIPPLE_TEST_MOUNTS_REQUIRED").is_none(),
        "KIPPLE_TEST_FAT must name a prepared mount"
    );
    let Some(fat) = fat else {
        return;
    };
    let temp = TempDir::new_in(&fat).unwrap();

    let refused = store(temp.path()).create_new(&name("daily"), b"x");

    assert_eq!(
        refused,
        Err(ProfileStoreError::FileSystem(Unsupported::NoPosixDelete))
    );
    assert!(
        !present(&temp.path().join("profiles")),
        "nothing is made there"
    );
}
