//! Where profiles are kept: the [`ProfileStore`] port, which moves bytes only, and the
//! reads and writes through it that are the only way to a [`Profile`] (D-068).

use std::fmt;

use super::{DraftProfile, Profile, ProfileName};
use crate::format::{FormatError, decode_profile, encode_profile};
use crate::frozen::NameCodec;
use crate::mutation::Unsupported;
use crate::outcome::ApplyError;

/// Why the profile store refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileStoreError {
    /// kipple's data directory isn't known.
    #[error("kipple's data directory isn't known")]
    NoDataDir,
    /// The data directory is on a file system kipple doesn't write its records to
    /// (D-065), network homes included.
    #[error("kipple's data directory is on an unsupported file system")]
    FileSystem(Unsupported),
    /// A profile of that name exists.
    #[error("a profile of that name exists")]
    Exists,
    /// There is no profile of that name.
    #[error("there is no profile of that name")]
    Missing,
    /// It couldn't be read.
    #[error("the profile couldn't be read")]
    Read(ApplyError),
    /// It couldn't be written durably, or removed.
    #[error("the profile couldn't be written")]
    Write(ApplyError),
    /// This build has no profile store.
    #[error("this build can't store profiles")]
    Unsupported,
}

/// Where kipple keeps its profiles: `<data>/profiles/<name>.json`, 0600 in a 0700
/// directory, opened without following links from the verified data directory.
/// Implemented by the platform, faked in tests.
///
/// It moves bytes only; the core encodes and decodes them. Every write is durable before
/// it returns, and a replacement is atomic: a crash leaves the old profile or the new
/// one, never a mix.
pub trait ProfileStore: fmt::Debug + Send + Sync {
    /// The bytes of the profile `name`, at most [`PROFILE_LIMIT`](crate::format::PROFILE_LIMIT)
    /// and one more, or `None` when there is none.
    ///
    /// # Errors
    /// When the store refuses or the file can't be read.
    fn read(&self, name: &ProfileName) -> Result<Option<Vec<u8>>, ProfileStoreError>;

    /// Writes a new profile `name` holding `bytes`, refusing if one exists.
    ///
    /// # Errors
    /// [`ProfileStoreError::Exists`] when it exists, or why it couldn't be written.
    fn create_new(&self, name: &ProfileName, bytes: &[u8]) -> Result<(), ProfileStoreError>;

    /// Replaces the profile `name` with `bytes`, atomically.
    ///
    /// # Errors
    /// [`ProfileStoreError::Missing`] when there is none, or why it couldn't be written.
    fn replace(&self, name: &ProfileName, bytes: &[u8]) -> Result<(), ProfileStoreError>;

    /// Removes the profile `name`.
    ///
    /// # Errors
    /// [`ProfileStoreError::Missing`] when there is none, or why it couldn't be removed.
    fn remove(&self, name: &ProfileName) -> Result<(), ProfileStoreError>;

    /// Every profile's name, sorted, leaving out what a crash left mid-write.
    ///
    /// # Errors
    /// When the store refuses or the directory can't be read.
    fn list(&self) -> Result<Vec<ProfileName>, ProfileStoreError>;
}

/// Why a profile couldn't be read or stored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// The store refused.
    #[error(transparent)]
    Store(#[from] ProfileStoreError),
    /// The file isn't a profile this kipple reads: another format, a newer version
    /// ([`FormatError::Newer`]), or an invalid value.
    #[error(transparent)]
    Format(#[from] FormatError),
    /// The file holds another profile than its name says.
    #[error("the file holds another profile than its name says")]
    Misnamed,
    /// What was read back isn't what was written.
    #[error("the profile read back isn't the one written")]
    ReadBack,
}

/// The profile `name`, read from `store`.
///
/// # Errors
/// When the store refuses, there is no such profile, or what it holds isn't a
/// `kipple.profile/v1` document named `name`.
pub fn read(
    store: &dyn ProfileStore,
    name: &ProfileName,
    codec: &dyn NameCodec,
) -> Result<Profile, ProfileError> {
    let bytes = store.read(name)?.ok_or(ProfileStoreError::Missing)?;
    let contents = decode_profile(&bytes, codec)?;
    if contents.name != *name {
        return Err(ProfileError::Misnamed);
    }
    Ok(Profile { contents })
}

/// Stores `draft` as a new profile and reads it back: the profile exactly as shown.
///
/// # Errors
/// When `draft` wouldn't read back as itself (it is then not stored), one of that name
/// exists, the store refuses, or what it reads back differs.
pub fn create(
    store: &dyn ProfileStore,
    draft: &DraftProfile,
    codec: &dyn NameCodec,
) -> Result<Profile, ProfileError> {
    store.create_new(&draft.contents.name, &readable(draft, codec)?)?;
    read_back(store, draft, codec)
}

/// Replaces the profile `draft` names with `draft`, atomically, and reads it back.
///
/// # Errors
/// When `draft` wouldn't read back as itself (the stored profile is then kept), there is
/// no such profile, the store refuses, or what it reads back differs.
pub fn replace(
    store: &dyn ProfileStore,
    draft: &DraftProfile,
    codec: &dyn NameCodec,
) -> Result<Profile, ProfileError> {
    store.replace(&draft.contents.name, &readable(draft, codec)?)?;
    read_back(store, draft, codec)
}

/// `draft` encoded, once it is known to decode to itself: a file no read accepts is
/// never stored.
fn readable(draft: &DraftProfile, codec: &dyn NameCodec) -> Result<Vec<u8>, ProfileError> {
    let bytes = encode_profile(&draft.contents, codec);
    if decode_profile(&bytes, codec)? == draft.contents {
        Ok(bytes)
    } else {
        Err(ProfileError::ReadBack)
    }
}

fn read_back(
    store: &dyn ProfileStore,
    draft: &DraftProfile,
    codec: &dyn NameCodec,
) -> Result<Profile, ProfileError> {
    let profile = read(store, &draft.contents.name, codec)?;
    if profile.contents == draft.contents {
        Ok(profile)
    } else {
        Err(ProfileError::ReadBack)
    }
}

/// Removes the profile `name`. It only takes authority away, so nothing is asked.
///
/// # Errors
/// When there is no such profile or the store refuses.
pub fn remove(store: &dyn ProfileStore, name: &ProfileName) -> Result<(), ProfileError> {
    Ok(store.remove(name)?)
}

/// Every stored profile's name.
///
/// # Errors
/// When the store refuses.
pub fn list(store: &dyn ProfileStore) -> Result<Vec<ProfileName>, ProfileError> {
    Ok(store.list()?)
}
