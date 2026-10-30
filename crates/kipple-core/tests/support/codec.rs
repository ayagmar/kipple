//! The name codec tests store names with. Include with
//! `#[path = "support/codec.rs"] mod codec;`.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use kipple_core::NameCodec;

/// Stores names as the OS does: bytes on Unix, UTF-16 code units on Windows, where a
/// name holding `:` is refused as `NameCodec` requires.
#[derive(Debug)]
pub(crate) struct TestCodec;

impl NameCodec for TestCodec {
    #[cfg(unix)]
    fn encode(&self, name: &OsStr) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt as _;
        name.as_bytes().to_vec()
    }

    #[cfg(unix)]
    fn decode(&self, bytes: &[u8]) -> Option<OsString> {
        use std::os::unix::ffi::OsStrExt as _;
        Some(OsStr::from_bytes(bytes).to_owned())
    }

    #[cfg(unix)]
    fn decode_path(&self, bytes: &[u8]) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStrExt as _;
        Some(PathBuf::from(OsStr::from_bytes(bytes)))
    }

    #[cfg(windows)]
    fn decode_path(&self, bytes: &[u8]) -> Option<PathBuf> {
        use std::os::windows::ffi::OsStringExt as _;
        let (units, rest) = bytes.as_chunks::<2>();
        if !rest.is_empty() {
            return None;
        }
        let wide: Vec<u16> = units.iter().map(|unit| u16::from_le_bytes(*unit)).collect();
        Some(PathBuf::from(OsString::from_wide(&wide)))
    }

    #[cfg(windows)]
    fn encode(&self, name: &OsStr) -> Vec<u8> {
        use std::os::windows::ffi::OsStrExt as _;
        name.encode_wide().flat_map(u16::to_le_bytes).collect()
    }

    #[cfg(windows)]
    fn decode(&self, bytes: &[u8]) -> Option<OsString> {
        use std::os::windows::ffi::OsStringExt as _;
        let (units, rest) = bytes.as_chunks::<2>();
        if !rest.is_empty() {
            return None;
        }
        let wide: Vec<u16> = units.iter().map(|unit| u16::from_le_bytes(*unit)).collect();
        if wide.contains(&u16::from(b':')) {
            return None;
        }
        Some(OsString::from_wide(&wide))
    }
}
