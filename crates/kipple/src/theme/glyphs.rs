//! The symbols the screen draws with, apart from its colours: Unicode, or ASCII only
//! under `--ascii`.

use ratatui::symbols::{border, line};

/// Which symbols to draw with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Glyphs {
    /// Box drawing, block elements, arrows and bullets.
    #[default]
    Unicode,
    /// ASCII only.
    Ascii,
}

const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

const ASCII_LINES: line::Set<'static> = line::Set {
    vertical: "|",
    horizontal: "-",
    top_right: "+",
    top_left: "+",
    bottom_right: "+",
    bottom_left: "+",
    vertical_left: "+",
    vertical_right: "+",
    horizontal_down: "+",
    horizontal_up: "+",
    cross: "+",
};

impl Glyphs {
    /// The lines of a border, with round corners.
    #[must_use]
    pub const fn border(self) -> border::Set<'static> {
        match self {
            Self::Unicode => border::ROUNDED,
            Self::Ascii => ASCII_BORDER,
        }
    }

    /// The lines inside a border, with the joins where they meet it and each other.
    #[must_use]
    pub const fn lines(self) -> line::Set<'static> {
        match self {
            Self::Unicode => line::ROUNDED,
            Self::Ascii => ASCII_LINES,
        }
    }

    /// The separator between items on one line, with its spaces.
    #[must_use]
    pub const fn dot(self) -> &'static str {
        match self {
            Self::Unicode => " · ",
            Self::Ascii => " - ",
        }
    }

    /// A heading's fold marker and its space.
    #[must_use]
    pub const fn fold(self, collapsed: bool) -> &'static str {
        match (self, collapsed) {
            (Self::Unicode, false) => "▾ ",
            (Self::Unicode, true) => "▸ ",
            (Self::Ascii, false) => "v ",
            (Self::Ascii, true) => "> ",
        }
    }

    /// `text` followed by an ellipsis.
    #[must_use]
    pub fn ellipsis(self, text: &str) -> String {
        let ellipsis = match self {
            Self::Unicode => "…",
            Self::Ascii => "...",
        };
        format!("{text}{ellipsis}")
    }

    /// A bar `eighths` eighths of a cell long, padded to `cells` cells. ASCII draws whole
    /// cells only, rounded to the nearest.
    #[must_use]
    pub fn bar(self, eighths: u64, cells: usize) -> String {
        const PARTS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
        let full = usize::try_from(eighths / 8).unwrap_or(cells).min(cells);
        let part = usize::try_from(eighths % 8).unwrap_or(0);
        let mut bar = match self {
            Self::Ascii => "#".repeat((full + usize::from(part >= 4)).min(cells)),
            Self::Unicode => {
                let mut bar = "█".repeat(full);
                if full < cells {
                    bar.push_str(PARTS.get(part).copied().unwrap_or(""));
                }
                bar
            }
        };
        let drawn = bar.chars().count();
        bar.push_str(&" ".repeat(cells.saturating_sub(drawn)));
        bar
    }

    /// The mark of a selected finding.
    #[must_use]
    pub const fn mark(self) -> &'static str {
        match self {
            Self::Unicode => "●",
            Self::Ascii => "*",
        }
    }

    /// `keys` as help writes them: arrows spelled out in ASCII.
    #[must_use]
    pub fn keys(self, keys: &str) -> String {
        match self {
            Self::Unicode => keys.to_owned(),
            Self::Ascii => keys
                .replace('↓', "dn")
                .replace('↑', "up")
                .replace('←', "lt")
                .replace('→', "rt"),
        }
    }
}
