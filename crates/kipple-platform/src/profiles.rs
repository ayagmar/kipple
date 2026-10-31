//! The profile store (spec M3c pinned 11, D-068): `<data>/profiles/<name>.json`.
//!
//! The data directory is opened without following a link, `profiles/` (mode 0700) in it
//! the same way, and every file in it relative to that handle (mode 0600), never through
//! a link. A data directory on a file system the run store refuses (D-065), network and
//! user-space ones included, is refused. The store moves bytes; the core decides what
//! they say.
//!
//! A profile is written to a temporary name in `profiles/` that only this process uses,
//! synced, renamed to its name (over the old one when it is replaced) and `profiles/`
//! synced, before the call returns. A crash leaves the old profile or the new one, never
//! a mix, and at worst a temporary file that [`ProfileStore::list`] leaves out.

use std::io::{self, Read as _, Write as _};
use std::path::Path;
use std::path::PathBuf;

use kipple_core::format::PROFILE_LIMIT;
use kipple_core::profile::{ProfileName, ProfileStore, ProfileStoreError};
use kipple_core::{ApplyError, ErrorCode, FileSystemClass};

use crate::os::folder::{Folder, create_synced, failed, made_on, regular};

/// The folder under the data directory that holds the profiles.
const PROFILES: &str = "profiles";

/// What a profile's file name ends with.
const SUFFIX: &str = ".json";

/// What a temporary file's name ends with.
const PARTIAL: &str = ".partial";

/// The platform's [`ProfileStore`]: files under kipple's data directory.
#[derive(Debug, Clone)]
pub struct FileProfileStore {
    /// kipple's data directory, `None` when it didn't resolve.
    data: Option<PathBuf>,
}

/// Whether a write makes a new profile or replaces one.
#[derive(Debug, Clone, Copy)]
enum Placing {
    /// The name must be free.
    New,
    /// The name's file is replaced in one step.
    Over,
}

/// Which way of failing a step counts as.
type Failure = fn(ApplyError) -> ProfileStoreError;

impl FileProfileStore {
    /// The store under `data`, kipple's data directory, `None` when it isn't known.
    #[must_use]
    pub const fn new(data: Option<PathBuf>) -> Self {
        Self { data }
    }

    fn data(&self) -> Result<&Path, ProfileStoreError> {
        self.data.as_deref().ok_or(ProfileStoreError::NoDataDir)
    }

    /// `<data>/profiles`, opened and on a supported file system; `None` when it, or the
    /// data directory, is missing.
    fn open(&self, as_: Failure) -> Result<Option<Folder>, ProfileStoreError> {
        let opened = Folder::open(self.data()?).and_then(|data| data.open_in(PROFILES));
        match opened {
            Ok(profiles) => {
                supported(profiles.file_system(), as_)?;
                Ok(Some(profiles))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(as_(failed(error))),
        }
    }

    /// `<data>/profiles`, opened, making it and the data directory first when they are
    /// missing. Their file system is checked before anything is made.
    fn make(&self) -> Result<Folder, ProfileStoreError> {
        let as_: Failure = ProfileStoreError::Write;
        let data = self.data()?;
        supported(made_on(data), as_)?;
        let data = create_synced(data)
            .and_then(|()| Folder::open(data))
            .map_err(|error| as_(failed(error)))?;
        supported(data.file_system(), as_)?;
        match data.create_dir(PROFILES) {
            Ok(()) => data.sync(),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
        .and_then(|()| data.open_in(PROFILES))
        .map_err(|error| as_(failed(error)))
    }
}

impl ProfileStore for FileProfileStore {
    fn read(&self, name: &ProfileName) -> Result<Option<Vec<u8>>, ProfileStoreError> {
        let as_: Failure = ProfileStoreError::Read;
        let Some(profiles) = self.open(as_)? else {
            return Ok(None);
        };
        let file = match profiles.read(&file_name(name)).and_then(regular) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(as_(failed(error))),
        };
        let mut bytes = Vec::new();
        file.take(PROFILE_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| as_(failed(error)))?;
        Ok(Some(bytes))
    }

    fn create_new(&self, name: &ProfileName, bytes: &[u8]) -> Result<(), ProfileStoreError> {
        write(&self.make()?, name, bytes, Placing::New)
    }

    fn replace(&self, name: &ProfileName, bytes: &[u8]) -> Result<(), ProfileStoreError> {
        let as_: Failure = ProfileStoreError::Write;
        let profiles = self.open(as_)?.ok_or(ProfileStoreError::Missing)?;
        match profiles.read(&file_name(name)).and_then(regular) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ProfileStoreError::Missing);
            }
            Err(error) => return Err(as_(failed(error))),
        }
        write(&profiles, name, bytes, Placing::Over)
    }

    fn remove(&self, name: &ProfileName) -> Result<(), ProfileStoreError> {
        let as_: Failure = ProfileStoreError::Write;
        let profiles = self.open(as_)?.ok_or(ProfileStoreError::Missing)?;
        match profiles.remove(&file_name(name)) {
            Ok(()) => profiles.sync().map_err(|error| as_(unsynced(error))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Err(ProfileStoreError::Missing)
            }
            Err(error) => Err(as_(failed(error))),
        }
    }

    fn list(&self) -> Result<Vec<ProfileName>, ProfileStoreError> {
        let as_: Failure = ProfileStoreError::Read;
        let Some(profiles) = self.open(as_)? else {
            return Ok(Vec::new());
        };
        let mut names: Vec<ProfileName> = profiles
            .names()
            .map_err(|error| as_(failed(error)))?
            .iter()
            .filter_map(|name| name.to_str()?.strip_suffix(SUFFIX))
            .filter_map(ProfileName::parse)
            .collect();
        names.sort();
        Ok(names)
    }
}

/// The file the profile `name` is kept in.
fn file_name(name: &ProfileName) -> String {
    format!("{name}{SUFFIX}")
}

/// Writes `bytes` as the profile `name` in `profiles`, placed as `placing` says: to a
/// temporary file this process alone names, synced, renamed into place, and `profiles`
/// synced. A write that fails leaves the old profile, if any, and no temporary file.
fn write(
    profiles: &Folder,
    name: &ProfileName,
    bytes: &[u8],
    placing: Placing,
) -> Result<(), ProfileStoreError> {
    let as_: Failure = ProfileStoreError::Write;
    let (to, temp) = (
        file_name(name),
        format!("{name}{SUFFIX}.{}{PARTIAL}", std::process::id()),
    );
    let mut file = profiles.create(&temp).map_err(|error| as_(failed(error)))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let placed = written.and_then(|()| match placing {
        Placing::New => profiles.rename(&temp, &to),
        Placing::Over => profiles.rename_replace(&temp, &to),
    });
    if let Err(error) = placed {
        profiles.remove(&temp).ok();
        return Err(match (placing, error.kind()) {
            (Placing::New, io::ErrorKind::AlreadyExists) => ProfileStoreError::Exists,
            _ => as_(failed(error)),
        });
    }
    profiles.sync().map_err(|error| as_(unsynced(error)))
}

/// Refuses a file system outside the executor's allowlist, or one that can't be told.
fn supported(class: io::Result<FileSystemClass>, as_: Failure) -> Result<(), ProfileStoreError> {
    match class {
        Ok(FileSystemClass::Supported(_)) => Ok(()),
        Ok(FileSystemClass::Unsupported(why)) => Err(ProfileStoreError::FileSystem(why)),
        Err(error) => Err(as_(failed(error))),
    }
}

/// A profile was written but couldn't be made durable.
fn unsynced(error: io::Error) -> ApplyError {
    ApplyError::io(ErrorCode::Unsynced, error)
}
