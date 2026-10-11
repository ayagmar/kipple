//! The grammars of schema v1: IDs, literal names, patterns, durations and byte sizes
//! (`docs/06-adapters-and-rules.md` §3).

use std::time::Duration;

use kipple_core::{Name, Pattern};

/// The longest pack ID.
const MAX_PACK_ID: usize = 64;
/// The longest pattern, in characters.
const MAX_PATTERN: usize = 128;
/// The most components a pattern may have.
const MAX_COMPONENTS: usize = 8;
/// The longest component.
const MAX_COMPONENT: usize = 64;

const DAY: u64 = 24 * 60 * 60;
/// The longest duration a predicate may name: 100 years.
pub(super) const MAX_DURATION: Duration = Duration::from_secs(100 * 365 * DAY);

/// `[a-z0-9-]+(\.[a-z0-9-]+)+`, at most 64 characters.
pub(super) fn is_pack_id(id: &str) -> bool {
    id.len() <= MAX_PACK_ID && id.split('.').count() >= 2 && id.split('.').all(is_id_part)
}

/// `<pack id>.<name>`, with a name of `[a-z0-9-]+`.
pub(super) fn is_rule_id(id: &str, pack: &str) -> bool {
    id.strip_prefix(pack)
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(is_id_part)
}

fn is_id_part(part: &str) -> bool {
    !part.is_empty()
        && part
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A literal name: one pattern component without wildcards.
pub(super) fn name(text: &str) -> Result<Name, String> {
    component(text, false)?;
    Ok(Name(text.to_owned()))
}

/// A relative pattern of 1 to 8 components separated by `/`.
pub(super) fn pattern(text: &str) -> Result<Pattern, String> {
    if text.chars().count() > MAX_PATTERN {
        return Err(format!("a pattern has at most {MAX_PATTERN} characters"));
    }
    let components: Vec<&str> = text.split('/').collect();
    if components.len() > MAX_COMPONENTS {
        return Err(format!("a pattern has at most {MAX_COMPONENTS} components"));
    }
    for part in &components {
        component(part, true)?;
    }
    Ok(Pattern(components.into_iter().map(str::to_owned).collect()))
}

/// Whether a pattern component has a wildcard.
pub(super) fn is_literal(component: &str) -> bool {
    !component.contains(['*', '?'])
}

/// One component: 1 to 64 characters from `[A-Za-z0-9._@+-]` (plus `*` and `?` in a
/// pattern), and not `.` or `..`. So absolute paths, empty components, `\`, `:` (drives
/// and alternate streams) and control characters are all refused.
fn component(text: &str, wildcards: bool) -> Result<(), String> {
    if text.is_empty() {
        return Err(
            "empty component: a pattern is relative, with no leading, trailing or doubled `/`"
                .to_owned(),
        );
    }
    if text.contains("**") {
        return Err("`**` is not allowed: a pattern never searches recursively".to_owned());
    }
    if text == "." || text == ".." {
        return Err(format!("`{text}` is not allowed as a component"));
    }
    if text.chars().count() > MAX_COMPONENT {
        return Err(format!(
            "a component has at most {MAX_COMPONENT} characters"
        ));
    }
    let allowed = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(c, '.' | '_' | '@' | '+' | '-')
            || (wildcards && matches!(c, '*' | '?'))
    };
    text.chars()
        .find(|&c| !allowed(c))
        .map_or(Ok(()), |c| Err(format!("{c:?} is not allowed in a name")))
}

/// `^[1-9][0-9]{0,4}(s|m|h|d|w|y)$`, where a year is 365 days.
pub(super) fn duration(text: &str) -> Option<Duration> {
    let unit = text.chars().last()?;
    let number = text.strip_suffix(unit)?;
    let seconds = match unit {
        's' => 1,
        'm' => 60,
        'h' => 60 * 60,
        'd' => DAY,
        'w' => 7 * DAY,
        'y' => 365 * DAY,
        _ => return None,
    };
    let count = digits(number, 1..=5)?;
    (!number.starts_with('0')).then(|| Duration::from_secs(count * seconds))
}

/// `^[0-9]{1,6}(B|KB|MB|GB|TB|KiB|MiB|GiB|TiB)$`: KB is 1000 bytes, KiB is 1024.
pub(super) fn bytes(text: &str) -> Option<u64> {
    let start = text.find(|c: char| !c.is_ascii_digit())?;
    let (number, unit) = text.split_at(start);
    let multiplier: u64 = match unit {
        "B" => 1,
        "KB" => 1000,
        "MB" => 1000_u64.pow(2),
        "GB" => 1000_u64.pow(3),
        "TB" => 1000_u64.pow(4),
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        _ => return None,
    };
    Some(digits(number, 1..=6)? * multiplier)
}

/// `text` as a number of `len` ASCII digits.
fn digits(text: &str, len: std::ops::RangeInclusive<usize>) -> Option<u64> {
    (len.contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}
