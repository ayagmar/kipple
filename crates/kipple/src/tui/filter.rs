//! The home screen's fuzzy filter: a query matches when its characters appear in order,
//! not necessarily together, so `webnm` finds `web/node_modules`.

use std::path::{MAIN_SEPARATOR, Path};

/// What the filter searches for a finding: its rule's title and its path, lowercased, with
/// `/` between the path's parts on every system, as the tree writes them.
pub(crate) fn haystack(title: &str, path: &Path) -> String {
    format!("{title} {}", path.display())
        .replace(MAIN_SEPARATOR, "/")
        .to_lowercase()
}

/// Whether every character of `query` appears in `haystack` in order, ignoring case and
/// the query's spaces, with the system's separator read as `/`. `haystack` is already
/// lowercase. An empty query matches everything.
pub(crate) fn matches(query: &str, haystack: &str) -> bool {
    let mut rest = haystack.chars();
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if c == MAIN_SEPARATOR { '/' } else { c })
        .flat_map(char::to_lowercase)
        .all(|wanted| rest.any(|c| c == wanted))
}
