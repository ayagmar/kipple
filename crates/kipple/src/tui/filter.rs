//! The home screen's filter: the query splits on spaces, and every term must appear
//! whole, ignoring case, in the finding's path as the screen shows it or in its rule's
//! title, so `web node` finds `~/projects/web/node_modules` and `/target` never finds
//! `tool/.venv`.

use std::path::{MAIN_SEPARATOR, Path};

use crate::human::homed;

/// What the filter searches for a finding, lowercased: its rule's title, and its path as
/// the screen shows it, from `~` below home and with `/` between its parts on every
/// system.
#[derive(Debug)]
pub(crate) struct Haystack {
    title: String,
    path: String,
}

impl Haystack {
    pub(crate) fn new(title: &str, path: &Path, home: Option<&Path>) -> Self {
        Self {
            title: title.to_lowercase(),
            path: homed(path, home)
                .replace(MAIN_SEPARATOR, "/")
                .to_lowercase(),
        }
    }
}

/// Whether every space-separated term of `query` appears in `haystack`'s path or title,
/// ignoring case, with the system's separator read as `/`. An empty query matches
/// everything.
pub(crate) fn matches(query: &str, haystack: &Haystack) -> bool {
    query.split_whitespace().all(|term| {
        let term = term.replace(MAIN_SEPARATOR, "/").to_lowercase();
        haystack.path.contains(&term) || haystack.title.contains(&term)
    })
}
