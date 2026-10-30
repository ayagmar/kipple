//! How a stored frozen tree keeps names: exactly as the OS holds them.

use std::ffi::{OsStr, OsString};

use kipple_core::NameCodec;

/// Stores each name in the OS's own form.
///
/// That is its bytes on Linux and macOS, and its UTF-16 code units as little-endian bytes
/// on Windows. A Windows name holding `:` is refused, since an NT-relative open reads
/// `a:b` as the stream `b` of `a`, not as an entry.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsNames;

impl NameCodec for OsNames {
    fn encode(&self, name: &OsStr) -> Vec<u8> {
        crate::os::raw_bytes(name)
    }

    #[cfg(unix)]
    fn decode(&self, bytes: &[u8]) -> Option<OsString> {
        use std::os::unix::ffi::OsStrExt as _;
        Some(OsStr::from_bytes(bytes).to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches a stored plan whose name `a:b` would make the Windows executor open, and
    /// remove, the stream `b` of the file `a` instead of an entry.
    #[cfg(windows)]
    #[test]
    fn a_stored_windows_name_holding_a_colon_is_refused() {
        let stored = OsNames.encode(OsStr::new("a:b"));
        assert_eq!(OsNames.decode(&stored), None);
        assert_eq!(OsNames.decode(b"a"), None, "half a code unit");
    }

    /// Catches a name that doesn't survive storing: a non-UTF-8 name on Unix, an
    /// ill-formed UTF-16 one on Windows, each removed later by a lossy copy.
    #[test]
    fn every_name_the_os_can_hold_survives_storing() {
        #[cfg(unix)]
        let name = {
            use std::os::unix::ffi::OsStrExt as _;
            OsStr::from_bytes(b"name-\xe9 \xff").to_owned()
        };
        #[cfg(windows)]
        let name = {
            use std::os::windows::ffi::OsStringExt as _;
            OsString::from_wide(&[0x61, 0xD800, 0x62])
        };
        assert_eq!(OsNames.decode(&OsNames.encode(&name)), Some(name));
    }
}
