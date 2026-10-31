//! What a mutating command removes and records through.
//!
//! The settings it judges with, the engine that scans again, the run and profile
//! stores, the confined executor and trash, git, and the host's protections.
//!
//! A [`Machine`] is `Send`: a worker thread can take one and build its [`Session`]
//! there, so no port reference crosses threads.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use kipple_core::{
    ApplyEvent, ApplyPorts, Engine, EventSink, Judge, Method, Methods, MutationPorts, Protections,
    Rule, Session, SymbolicRoot,
};
use kipple_platform::{
    ConfinedFs, ConfinedTrash, Environment, FileProfileStore, FileRunStore, GitQuery, OsNames,
    clock, kipple_dirs,
};

use crate::config::{Settings, held_at};
use crate::scan;
use crate::setup::Builtin;

/// What every mutating command removes and records through, besides its scan.
#[derive(Debug)]
pub struct Machine {
    settings: Settings,
    engine: Engine,
    rules: Vec<Rule>,
    methods: Methods,
    protections: Protections,
    store: FileRunStore,
    profiles: FileProfileStore,
    trash: ConfinedTrash,
    git: GitQuery,
}

impl Machine {
    /// The machine `settings` and `--method` describe on the machine `env` describes:
    /// the built-in rules, the run and profile stores under kipple's data directory, the
    /// trash the roots resolve, and the host's protections, which hold that data
    /// directory and every state root.
    ///
    /// # Errors
    /// When the engine can't start, worded for the person.
    pub fn from_settings(
        builtin: &Builtin,
        env: &Environment,
        settings: Settings,
        method: Option<Method>,
    ) -> Result<Self, String> {
        let engine = builtin.engine(env)?;
        let roots = &settings.roots;
        let data = kipple_dirs(roots).data.path.ok();
        let clock: Arc<dyn kipple_core::Clock> = Arc::from(clock(env));
        let trash = match roots.get(SymbolicRoot::Trash) {
            Some(Ok(resolved)) => Some(resolved.path.clone()),
            Some(Err(_)) | None => None,
        };
        Ok(Self {
            rules: builtin.platform_rules(),
            methods: Methods {
                command_line: method,
                rules: BTreeMap::new(),
                config: settings.loaded.config.method,
            },
            protections: Protections::of_host(roots, data.as_deref(), &|path| held_at(path, roots)),
            store: FileRunStore::new(data.clone(), Arc::clone(&clock)),
            profiles: FileProfileStore::new(data),
            trash: ConfinedTrash::new(trash, clock),
            git: GitQuery::new(env),
            engine,
            settings,
        })
    }

    /// The settings it judges with.
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The engine that scans, and scans again at apply.
    #[must_use]
    pub const fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Where runs are kept.
    #[must_use]
    pub const fn store(&self) -> &FileRunStore {
        &self.store
    }

    /// Where profiles are kept.
    #[must_use]
    pub const fn profiles(&self) -> &FileProfileStore {
        &self.profiles
    }

    /// The judge its settings make.
    #[must_use]
    pub fn judge(&self) -> Judge<'_> {
        Judge {
            rules: &self.rules,
            policy: &self.settings.policy,
            methods: &self.methods,
            protections: &self.protections,
        }
    }

    /// The ports that freeze, delete, trash and ask git.
    #[must_use]
    pub fn mutation(&self) -> MutationPorts<'_> {
        MutationPorts {
            freezer: &ConfinedFs,
            executor: &ConfinedFs,
            trash: &self.trash,
            tracked: &self.git,
        }
    }

    /// Runs `then` with the session over it, on the calling thread.
    pub fn with_session<R>(&self, then: impl FnOnce(&Session<'_>) -> R) -> R {
        let request = scan::request(&self.settings, None);
        let rescan = self.engine.rescan(&request);
        let mutation = self.mutation();
        let session = Session {
            judge: self.judge(),
            store: &self.store,
            ports: ApplyPorts {
                rescan: &rescan,
                mutation: &mutation,
            },
            codec: &OsNames,
        };
        then(&session)
    }
}

/// Sends each event to the thread that reads them.
///
/// Such as the TUI's, which shows a run while a worker applies it. An event sent after
/// the reader left is dropped: the run's journal records what happened either way.
#[derive(Debug, Clone)]
pub struct ChannelSink(pub Sender<ApplyEvent>);

impl EventSink for ChannelSink {
    fn event(&mut self, event: ApplyEvent) {
        self.0.send(event).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches a port or setting that can't cross to a worker thread, which would keep
    /// the TUI planning and applying on its UI thread (spec M3c pinned 4, 7).
    #[test]
    fn a_machine_can_go_to_a_worker() {
        fn send<T: Send>() {}
        send::<Machine>();
    }
}
