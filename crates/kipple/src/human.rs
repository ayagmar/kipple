//! Text for people: escaping and sizes.

use crate::json::{CompletenessOut, SizeOut};

/// `text` for the terminal: paths and OS messages may hold control characters.
pub(crate) fn escaped(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            let shown: Vec<char> = if c.is_control() {
                c.escape_default().collect()
            } else {
                vec![c]
            };
            shown
        })
        .collect()
}

/// A byte count in binary units, such as `8.0 MiB`.
pub(crate) fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if count < 1024 {
        return format!("{count} B");
    }
    let mut value = count;
    let mut unit = 0;
    while value >= 1024 * 1024 && unit + 1 < UNITS.len() {
        value /= 1024;
        unit += 1;
    }
    // One decimal, computed in integers: tenths of the unit, rounded down.
    let tenths = value * 10 / 1024;
    let name = UNITS.get(unit).copied().unwrap_or("PiB");
    format!("{}.{} {name}", tenths / 10, tenths % 10)
}

/// What removing an item frees: allocated storage where known, else apparent size. A
/// partial measurement is a lower bound.
pub(crate) fn size(size: Option<&SizeOut>) -> String {
    let Some(size) = size else {
        return "unsized".to_owned();
    };
    let shown = bytes(size.allocated.unwrap_or(size.apparent));
    match size.completeness {
        CompletenessOut::Complete => shown,
        CompletenessOut::Incomplete | CompletenessOut::Cancelled => format!(">= {shown}"),
    }
}
