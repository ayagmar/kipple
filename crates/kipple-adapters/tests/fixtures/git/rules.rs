//! The built-in rules as one platform ships them, wherever the test runs.

use kipple_adapters::builtin;
use kipple_core::{Platform, Rule};

/// The built-in rules that ship on `platform`.
#[expect(
    clippy::unwrap_used,
    reason = "a built-in pack that doesn't validate fails the test"
)]
pub(crate) fn on(platform: Platform) -> Vec<Rule> {
    let packs = builtin::packs(&semver::Version::new(0, 0, 0)).unwrap();
    let rules = packs.into_iter().flat_map(|pack| pack.rules);
    rules
        .filter(|rule| rule.platforms.contains(&platform))
        .collect()
}
