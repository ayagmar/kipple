//! What a piece of the screen is, so a theme can say how it looks.

use kipple_core::Bucket;

/// A semantic role: the screen asks the theme for one and never names a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// Ordinary text.
    Text,
    /// Secondary text: hints, notes, rule titles beside a path.
    Muted,
    /// A message that answers a key.
    Accent,
    /// The lines between panes.
    Border,
    /// Headings and the screen's name.
    Title,
    /// A label in front of its value.
    Label,
    /// The focused line, patched over each of its spans.
    Cursor,
    /// The mark of a selected finding.
    Selected,
    /// Safe to clean.
    Safe,
    /// To review.
    Review,
    /// Protected.
    Protected,
    /// Something that makes the report partial.
    Warning,
    /// Something that failed.
    Error,
    /// A Safe finding's size bar.
    BarSafe,
    /// A Review finding's size bar.
    BarReview,
    /// A Protected finding's size bar.
    BarProtected,
    /// The empty part of a bar.
    BarTrack,
    /// A key.
    Key,
    /// What a key does.
    KeyLabel,
    /// The level in force.
    LevelCurrent,
    /// The other levels.
    LevelOther,
    /// What the user keeps: brand amber.
    Kept,
}

impl Role {
    /// Every role, in the order of a theme file.
    pub const ALL: [Self; 22] = [
        Self::Text,
        Self::Muted,
        Self::Accent,
        Self::Border,
        Self::Title,
        Self::Label,
        Self::Cursor,
        Self::Selected,
        Self::Safe,
        Self::Review,
        Self::Protected,
        Self::Warning,
        Self::Error,
        Self::BarSafe,
        Self::BarReview,
        Self::BarProtected,
        Self::BarTrack,
        Self::Key,
        Self::KeyLabel,
        Self::LevelCurrent,
        Self::LevelOther,
        Self::Kept,
    ];

    /// The role a bucket's name and its findings' states are painted in, in the CLI
    /// and on the home screen.
    pub(crate) const fn bucket(bucket: Bucket) -> Self {
        match bucket {
            Bucket::Safe => Self::Safe,
            Bucket::Review => Self::Review,
            Bucket::Protected => Self::Protected,
        }
    }

    /// The role's name in a theme file.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Muted => "muted",
            Self::Accent => "accent",
            Self::Border => "border",
            Self::Title => "title",
            Self::Label => "label",
            Self::Cursor => "cursor",
            Self::Selected => "selected",
            Self::Safe => "safe",
            Self::Review => "review",
            Self::Protected => "protected",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::BarSafe => "bar.safe",
            Self::BarReview => "bar.review",
            Self::BarProtected => "bar.protected",
            Self::BarTrack => "bar.track",
            Self::Key => "key",
            Self::KeyLabel => "key.label",
            Self::LevelCurrent => "level.current",
            Self::LevelOther => "level.other",
            Self::Kept => "kept",
        }
    }

    /// The role a theme file names `name`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.name() == name)
    }
}
