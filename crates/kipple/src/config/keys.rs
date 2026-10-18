//! The `[keys]` table: which key does what on the home screen. It reads no key yet, so
//! any key in it is unknown.

use toml_edit::TableLike;

use super::parse::{ParseError, Reader};

/// What `[keys]` says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeysConfig {}

impl Reader<'_> {
    pub(super) fn keys(&self, keys: &dyn TableLike) -> Result<KeysConfig, ParseError> {
        self.only("keys", keys, &[])?;
        Ok(KeysConfig {})
    }
}
