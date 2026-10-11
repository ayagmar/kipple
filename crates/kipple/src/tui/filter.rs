//! The home screen's fuzzy filter: a query matches when its characters appear in order,
//! not necessarily together, so `webnm` finds `web/node_modules`.

/// Whether every character of `query` appears in `haystack` in order, ignoring case and
/// the query's spaces. `haystack` is already lowercase. An empty query matches
/// everything.
pub(crate) fn matches(query: &str, haystack: &str) -> bool {
    let mut rest = haystack.chars();
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .all(|wanted| rest.any(|c| c == wanted))
}
