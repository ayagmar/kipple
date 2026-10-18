//! The `[ui]` table: how the home screen looks. It reads no key yet, so any key in it
//! is unknown.

use toml_edit::TableLike;

use super::parse::{ParseError, Reader};

/// What `[ui]` says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiConfig {}

impl Reader<'_> {
    pub(super) fn ui(&self, ui: &dyn TableLike) -> Result<UiConfig, ParseError> {
        self.only("ui", ui, &[])?;
        Ok(UiConfig {})
    }
}
