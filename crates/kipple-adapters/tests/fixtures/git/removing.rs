//! The built-in rules with git's pack replaced by `removing.toml`: the same pack, with
//! `builtin.git.worktree-remove` running `git-worktree-remove` behind pinned 17's
//! predicates. It is validated as a built-in pack is, so a predicate the integration
//! doesn't supply fails here first.

use kipple_adapters::builtin;
use kipple_adapters::pack::{self, Catalog, Origin};
use kipple_core::{IntegrationId, Platform, Rule};

/// Every built-in rule, git's from `removing.toml`, on every platform.
#[expect(
    clippy::unwrap_used,
    reason = "a pack that doesn't validate fails the test"
)]
pub(crate) fn all() -> Vec<Rule> {
    let engine = semver::Version::new(0, 0, 0);
    let integrations = builtin::descriptors();
    let catalog = Catalog {
        engine: &engine,
        integrations: &integrations,
    };
    let bytes = include_bytes!("removing.toml");
    let git = pack::validate("removing.toml", bytes, Origin::Builtin, catalog).unwrap();
    let others = builtin::packs(&engine).unwrap().into_iter();
    let others = others.flat_map(|pack| pack.rules);
    others
        .filter(|rule| rule.integration != IntegrationId::GIT)
        .chain(git.rules)
        .collect()
}

/// The rules that ship on `platform`, git's from `removing.toml`.
pub(crate) fn on(platform: Platform) -> Vec<Rule> {
    let rules = all().into_iter();
    rules
        .filter(|rule| rule.platforms.contains(&platform))
        .collect()
}
