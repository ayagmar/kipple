//! The themes the home screen can draw with (D-055): every theme of the catalog drawn
//! for this terminal once, the one in force, and the one settings previews. A preview
//! only changes the look; it is kept by writing `ui.theme`, and leaving settings
//! without that puts the theme in force back.

use crate::theme::{Catalog, Colours, DEFAULT, Depth, Origin, Theme, ThemeError};

/// One theme to choose from.
#[derive(Debug, Clone)]
pub(crate) struct Choice {
    pub(crate) name: String,
    pub(crate) origin: Origin,
    /// The theme drawn for this terminal, or why it can't be used.
    pub(crate) drawn: Result<Theme, String>,
}

/// The themes, the one in force and the one previewed.
#[derive(Debug, Clone)]
pub(crate) struct Picker {
    choices: Vec<Choice>,
    active: usize,
    preview: Option<usize>,
    pub(crate) colours: Colours,
}

impl Default for Picker {
    /// The built-in themes, `kipple` in force, in full colour.
    fn default() -> Self {
        let colours = Colours::On(Depth::TrueColor);
        let choices = choices(Catalog::builtin(), colours);
        let active = position(&choices, DEFAULT).unwrap_or(0);
        Self {
            choices,
            active,
            preview: None,
            colours,
        }
    }
}

/// Which theme the settings row shows, from 1, of how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) shown: usize,
    pub(crate) count: usize,
}

impl Picker {
    /// The themes of `catalog` drawn with `colours`, `active` in force.
    ///
    /// # Errors
    /// When there is no theme `active`, or it can't be used.
    pub(crate) fn new(
        catalog: Catalog,
        active: &str,
        colours: Colours,
    ) -> Result<Self, ThemeError> {
        let refused = catalog.find(active).map(|entry| entry.theme.clone());
        match refused {
            None => {
                return Err(ThemeError {
                    file: None,
                    position: None,
                    message: format!("there is no theme {active}; `kipple theme list` lists them"),
                });
            }
            Some(Err(error)) => return Err(error),
            Some(Ok(_)) => {}
        }
        let choices = choices(catalog, colours);
        let active = position(&choices, active).unwrap_or(0);
        Ok(Self {
            choices,
            active,
            preview: None,
            colours,
        })
    }

    /// The theme to draw with: the one previewed when it can be used, else the one in
    /// force.
    pub(crate) fn theme(&self) -> &Theme {
        let previewed = self
            .preview
            .and_then(|at| self.choices.get(at))
            .and_then(|choice| choice.drawn.as_ref().ok());
        let active = self
            .choices
            .get(self.active)
            .and_then(|choice| choice.drawn.as_ref().ok());
        previewed.or(active).unwrap_or(&FALLBACK)
    }

    /// The theme the settings row shows: the one previewed, else the one in force.
    pub(crate) fn shown(&self) -> Option<&Choice> {
        self.choices.get(self.preview.unwrap_or(self.active))
    }

    /// Which theme the row shows, and how many there are.
    pub(crate) fn place(&self) -> Place {
        Place {
            shown: self.preview.unwrap_or(self.active) + 1,
            count: self.choices.len(),
        }
    }

    /// Whether the row shows a theme other than the one in force.
    pub(crate) fn previewing(&self) -> bool {
        self.preview.is_some_and(|at| at != self.active)
    }

    /// Previews the theme `by` places from the one shown, wrapping around.
    pub(crate) fn step(&mut self, by: isize) {
        let count = self.choices.len();
        if count == 0 {
            return;
        }
        let at = self.preview.unwrap_or(self.active);
        let moved = at
            .cast_signed()
            .saturating_add(by)
            .rem_euclid(count.cast_signed());
        self.preview = Some(moved.cast_unsigned());
    }

    /// Stops previewing: the theme in force is drawn again.
    pub(crate) const fn revert(&mut self) {
        self.preview = None;
    }

    /// Puts `name` in force, as the config now says, when it can be used.
    pub(crate) fn adopt(&mut self, name: &str) {
        if let Some(at) = position(&self.choices, name)
            .filter(|&at| self.choices.get(at).is_some_and(|c| c.drawn.is_ok()))
        {
            self.active = at;
            self.preview = None;
        }
    }
}

/// What to draw with should no theme be usable, which [`Picker::new`] rules out.
static FALLBACK: Theme = Theme::EMPTY;

fn choices(catalog: Catalog, colours: Colours) -> Vec<Choice> {
    catalog
        .entries
        .into_iter()
        .map(|entry| Choice {
            name: entry.name,
            origin: entry.origin,
            drawn: entry
                .theme
                .map(|theme| theme.drawn(colours))
                .map_err(|error| error.to_string()),
        })
        .collect()
}

/// Where the usable-or-not theme `name` is among `choices`. A user file that takes a
/// built-in's name comes after the built-in, so the built-in is found.
fn position(choices: &[Choice], name: &str) -> Option<usize> {
    choices.iter().position(|choice| choice.name == name)
}
