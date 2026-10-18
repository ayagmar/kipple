//! The `[ui]` table: how the home screen looks (`docs/03-functional-spec.md` §6). The
//! theme is read here only as a name; whether a theme of that name exists is for the
//! commands that draw (D-055).

use std::io::{self, Write};

use toml_edit::{Item, TableLike, Value};

use super::parse::{ParseError, Reader};
use crate::theme::{self, Glyphs, valid_name};

/// What `[ui]` says, with its defaults filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiConfig {
    /// `ui.theme`: the theme's name, when one is chosen.
    pub theme: Option<String>,
    /// `ui.summary`: whether the band with each bucket's total is shown.
    pub summary: bool,
    /// `ui.detail`: where the detail pane goes.
    pub detail: DetailPlace,
    /// `ui.bars`: whether findings have size bars.
    pub bars: bool,
    /// `ui.glyphs`: which symbols to draw with. `--ascii` wins.
    pub glyphs: Glyphs,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: None,
            summary: true,
            detail: DetailPlace::Auto,
            bars: true,
            glyphs: Glyphs::Unicode,
        }
    }
}

/// Where the detail pane goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DetailPlace {
    /// Right of the tree on a wide terminal, below it on a narrow one.
    #[default]
    Auto,
    /// Always right of the tree.
    Right,
    /// Always below the tree.
    Bottom,
    /// Not shown.
    Off,
}

impl DetailPlace {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(Self::Auto),
            "right" => Some(Self::Right),
            "bottom" => Some(Self::Bottom),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Right => "right",
            Self::Bottom => "bottom",
            Self::Off => "off",
        }
    }
}

fn glyphs_named(name: &str) -> Option<Glyphs> {
    match name {
        "unicode" => Some(Glyphs::Unicode),
        "ascii" => Some(Glyphs::Ascii),
        _ => None,
    }
}

const fn glyphs_name(glyphs: Glyphs) -> &'static str {
    match glyphs {
        Glyphs::Unicode => "unicode",
        Glyphs::Ascii => "ascii",
    }
}

impl UiConfig {
    /// Its part of `kipple config show`: every key, the defaults filled in.
    pub(super) fn write_show(&self, out: &mut dyn Write) -> io::Result<()> {
        let theme = self
            .theme
            .clone()
            .unwrap_or_else(|| format!("{} (default)", theme::DEFAULT));
        writeln!(out, "{:<22}{theme}", "ui.theme")?;
        writeln!(out, "{:<22}{}", "ui.summary", self.summary)?;
        writeln!(out, "{:<22}{}", "ui.detail", self.detail.name())?;
        writeln!(out, "{:<22}{}", "ui.bars", self.bars)?;
        writeln!(out, "{:<22}{}", "ui.glyphs", glyphs_name(self.glyphs))
    }
}

impl Reader<'_> {
    pub(super) fn ui(&self, ui: &dyn TableLike) -> Result<UiConfig, ParseError> {
        self.only("ui", ui, &["theme", "summary", "detail", "bars", "glyphs"])?;
        let mut config = UiConfig::default();
        if let Some(item) = ui.get("theme") {
            let name = item.as_str().filter(|name| valid_name(name));
            let wanted = "a theme name: lowercase letters, digits and -";
            config.theme = Some(
                name.ok_or_else(|| self.bad(item, "ui.theme", wanted))?
                    .to_owned(),
            );
        }
        if let Some(item) = ui.get("summary") {
            config.summary = self.flag(item, "ui.summary")?;
        }
        if let Some(item) = ui.get("detail") {
            let place = item.as_str().and_then(DetailPlace::from_name);
            let wanted = "auto, right, bottom or off";
            config.detail = place.ok_or_else(|| self.bad(item, "ui.detail", wanted))?;
        }
        if let Some(item) = ui.get("bars") {
            config.bars = self.flag(item, "ui.bars")?;
        }
        if let Some(item) = ui.get("glyphs") {
            let glyphs = item.as_str().and_then(glyphs_named);
            config.glyphs =
                glyphs.ok_or_else(|| self.bad(item, "ui.glyphs", "unicode or ascii"))?;
        }
        Ok(config)
    }

    fn flag(&self, item: &Item, key: &str) -> Result<bool, ParseError> {
        item.as_value()
            .and_then(Value::as_bool)
            .ok_or_else(|| self.bad(item, key, "true or false"))
    }
}
