//! What every command composes: the built-in integrations and packs, and an engine over
//! the platform's probes for the ones that run on this OS.

use std::collections::BTreeMap;

use kipple_adapters::builtin;
use kipple_core::{Engine, IntegrationDescriptor, IntegrationId, Lens, Pack, Rule};
use kipple_platform::{NoFollowFs, PLATFORM, Walker};

/// The built-in integrations and packs, whatever platform they support.
#[derive(Debug)]
pub struct Builtin {
    /// Every integration's descriptor, by ID.
    pub descriptors: Vec<IntegrationDescriptor>,
    /// Every pack, by ID.
    pub packs: Vec<Pack>,
}

impl Builtin {
    /// Loads the built-in packs, validated against this engine.
    ///
    /// # Errors
    /// A message naming each error of a built-in pack, which is a bug in kipple.
    pub fn load() -> Result<Self, String> {
        let engine = semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .map_err(|e| format!("kipple's own version is not semantic: {e}"))?;
        let mut packs = builtin::packs(&engine).map_err(|errors| {
            let lines: Vec<String> = errors
                .iter()
                .map(|e| {
                    format!(
                        "{}:{}:{}: {} {}",
                        e.file,
                        e.line,
                        e.column,
                        e.code.name(),
                        e.message
                    )
                })
                .collect();
            format!("a built-in rule pack is invalid:\n{}", lines.join("\n"))
        })?;
        packs.sort_by(|a, b| a.id.cmp(&b.id));
        let mut descriptors = builtin::descriptors();
        descriptors.sort_by_key(|d| d.id);
        Ok(Self { descriptors, packs })
    }

    /// Every rule of every pack, in pack order.
    pub fn rules(&self) -> impl Iterator<Item = (&Pack, &Rule)> {
        self.packs
            .iter()
            .flat_map(|pack| pack.rules.iter().map(move |rule| (pack, rule)))
    }

    /// Each integration's lens.
    #[must_use]
    pub fn lenses(&self) -> BTreeMap<IntegrationId, Lens> {
        self.descriptors.iter().map(|d| (d.id, d.lens)).collect()
    }

    /// An engine over the integrations and rules that run on this platform.
    ///
    /// # Errors
    /// When the sizing thread pool can't start, or a built-in rule's integration doesn't
    /// run where the rule does, which is a bug in kipple.
    pub fn engine(&self) -> Result<Engine, String> {
        let integrations = builtin::integrations()
            .into_iter()
            .filter(|i| i.descriptor().platforms.contains(&PLATFORM))
            .collect();
        let rules = self
            .rules()
            .filter(|(_, rule)| rule.platforms.contains(&PLATFORM))
            .map(|(_, rule)| rule.clone())
            .collect();
        let walker = Walker::new().map_err(|e| format!("could not start sizing: {e}"))?;
        Engine::new(Box::new(NoFollowFs), Box::new(walker), integrations, rules)
            .map_err(|e| e.to_string())
    }
}
