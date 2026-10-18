//! The `[ui]` table: how the home screen looks. `theme` is only checked as a name here;
//! whether such a theme exists is up to the commands that draw (D-055).

use toml_edit::TableLike;

use super::parse::{ParseError, Reader};
use crate::theme::valid_name;

/// What `[ui]` says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiConfig {
    /// `ui.theme`: the theme's name.
    pub theme: Option<String>,
}

impl Reader<'_> {
    pub(super) fn ui(&self, ui: &dyn TableLike) -> Result<UiConfig, ParseError> {
        self.only("ui", ui, &["theme"])?;
        let theme = match ui.get("theme") {
            None => None,
            Some(item) => Some(
                item.as_str()
                    .filter(|name| valid_name(name))
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        self.bad(
                            item,
                            "ui.theme",
                            "a theme name: lowercase letters, digits and -",
                        )
                    })?,
            ),
        };
        Ok(UiConfig { theme })
    }
}
