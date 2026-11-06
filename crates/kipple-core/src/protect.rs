//! The host's protected sets: what no plan touches, whatever any rule says
//! (`docs/04-safety-model.md` §3.2, §3.9).
//!
//! The planner checks every selected item against them before it merges or freezes
//! anything, and apply checks again before it runs anything. They hold whether the
//! integration that declared a subresource ran, failed or is absent.
//!
//! A state root is closed unless every declaration that reaches a path consents: its owner
//! is supported in this scan, an area it opens lists the finding's integration, and nothing
//! it protects is at, inside or above the path.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path};

use crate::integration::{IntegrationDescriptor, IntegrationId};
use crate::policy::{HeldPath, Lineage, starts_with};
use crate::root::{Roots, SymbolicRoot};

/// One state root of a tool, as its integration declares it (04 §3.9). Everything in it is
/// closed unless an [`OpenArea`] opens it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateDeclaration {
    /// The root.
    pub root: SymbolicRoot,
    /// What stays closed even inside an open area.
    pub protected: &'static [ProtectedEntry],
    /// What may be removed, and by whose rules.
    pub open: &'static [OpenArea],
    /// Roots of the same integration this one stands aside for: a path inside one is that
    /// root's to answer for.
    pub delegates: &'static [SymbolicRoot],
    /// What is held when the root has no path.
    pub unresolved: Unresolved,
}

/// A path below a state root that stays closed, in `/`-separated form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedEntry {
    /// This path.
    Exact(&'static str),
    /// Every path whose last component starts as this path's does, such as
    /// `releases/.staging.`.
    Prefix(&'static str),
}

/// A part of a state root some integrations may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenArea {
    /// Where it is.
    pub at: Area,
    /// Whose findings may be there.
    pub integrations: &'static [IntegrationId],
}

/// Where an [`OpenArea`] is, below its root, in `/`-separated form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// Exactly this path.
    Exact(&'static str),
    /// Whatever is strictly inside this path.
    Inside(&'static str),
}

impl Area {
    /// Whether a path with the components `below` under its root is in this area, compared
    /// exactly: a name that differs only by case may be another file.
    fn holds(self, below: &[String]) -> bool {
        match self {
            Self::Exact(at) => below == parts(at),
            Self::Inside(at) => {
                let at = parts(at);
                below.len() > at.len() && below.starts_with(&at)
            }
        }
    }
}

impl ProtectedEntry {
    /// Whether a path with the components `below` under its root is at, inside or above
    /// this entry, ignoring case: a different case only ever matches more.
    fn touches(self, below: &[String]) -> bool {
        let below: Vec<String> = below.iter().map(|name| name.to_lowercase()).collect();
        match self {
            Self::Exact(entry) => {
                let entry = parts(&entry.to_lowercase());
                below.starts_with(&entry) || entry.starts_with(&below)
            }
            Self::Prefix(entry) => {
                let entry = parts(&entry.to_lowercase());
                let Some((last, parent)) = entry.split_last() else {
                    return true;
                };
                parent.starts_with(&below)
                    || (below.starts_with(parent)
                        && below
                            .get(parent.len())
                            .is_some_and(|name| name.starts_with(last.as_str())))
            }
        }
    }
}

/// The components of a `/`-separated path below a root.
fn parts(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

/// What is held when a state root has no path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unresolved {
    /// Every finding of the scan: nothing can be shown to be outside a store of credentials
    /// that can't be located.
    HoldAll,
    /// Only the findings of the integration that declared it.
    HoldOwn,
}

named! {
    /// Why the host protects a path whatever any rule says.
    Protection {
        /// A file system's root directory.
        FileSystemRoot = "file-system-root",
        /// The home directory itself, or a directory holding it.
        Home = "home",
        /// A subresource an integration declares, such as a credential store or a
        /// tool's config: at it, inside it or above it.
        Subresource = "subresource",
    }
}

/// Whether an integration recognised what it read in this scan, which is what lets its
/// state roots open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OwnerStatus {
    /// It ran to its end and verified the layout.
    Supported,
    /// Anything else: an unverified version, a failed discovery, or it never said.
    #[default]
    Unsupported,
}

/// Each integration's status in one scan. One the scan didn't run is unsupported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Owners(pub BTreeMap<IntegrationId, OwnerStatus>);

impl Owners {
    /// The status of `integration`.
    #[must_use]
    pub fn status(&self, integration: IntegrationId) -> OwnerStatus {
        self.0.get(&integration).copied().unwrap_or_default()
    }
}

/// A state root as the host located it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRoot {
    /// The integration that declared it.
    pub owner: IntegrationId,
    /// What it declared.
    pub declaration: StateDeclaration,
    /// Where the root is.
    pub held: HeldPath,
}

/// The paths the host protects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Protections {
    /// The home directory, matched by path and by identity. `None` when it is unknown:
    /// then no item can be shown not to hold it, and every one is protected.
    pub home: Option<HeldPath>,
    /// What stays closed to everything: kipple's own data, and each root that holds every
    /// finding because it has no path.
    pub subresources: Vec<HeldPath>,
    /// Every integration's state roots, whether it runs here or not.
    pub state: Vec<StateRoot>,
    /// The integrations with a state root that has no path and holds only their own
    /// findings.
    pub unresolved: BTreeSet<IntegrationId>,
}

/// One finding the host is asked about.
#[derive(Debug, Clone, Copy)]
struct Asked<'finding> {
    path: &'finding Path,
    lineage: &'finding Lineage,
    integration: IntegrationId,
    owners: &'finding Owners,
}

impl Protections {
    /// The host's protections: the home directory, kipple's data directory `data` (its
    /// runs and config) and the state roots of `descriptors`, which are every built-in
    /// integration's whether it runs here or not, each as `held` reads it from its path.
    /// An unknown `data` holds every finding, and a state root with no path holds what its
    /// [`Unresolved`] says. A root this platform doesn't have holds nothing.
    #[must_use]
    pub fn of_host(
        roots: &Roots,
        data: Option<&Path>,
        descriptors: &[IntegrationDescriptor],
        held: &dyn Fn(&Path) -> HeldPath,
    ) -> Self {
        let home = match roots.get(SymbolicRoot::Home) {
            Some(Ok(home)) => Some(held(&home.path)),
            Some(Err(_)) | None => None,
        };
        let mut protections = Self {
            home,
            subresources: vec![data.map_or_else(HeldPath::default, held)],
            ..Self::default()
        };
        let declared = descriptors.iter().flat_map(|descriptor| {
            let state = descriptor.state.iter();
            state.map(|declaration| (descriptor.id, *declaration))
        });
        for (owner, declaration) in declared {
            for root in roots.all(declaration.root) {
                let at = root.as_ref().ok().map(|root| held(&root.path));
                protections.add(owner, declaration, at);
            }
        }
        protections
    }

    /// Adds one path of a state root, or what it holds when `at` is `None`.
    fn add(&mut self, owner: IntegrationId, declaration: StateDeclaration, at: Option<HeldPath>) {
        match (at, declaration.unresolved) {
            (Some(held), _) => self.state.push(StateRoot {
                owner,
                declaration,
                held,
            }),
            (None, Unresolved::HoldAll) => self.subresources.push(HeldPath::default()),
            (None, Unresolved::HoldOwn) => {
                self.unresolved.insert(owner);
            }
        }
    }

    /// What protects an item at `path` that `integration` found, with the identities
    /// `lineage` of it and the directories above it, if anything does. `owners` says who
    /// was supported in the scan that found it.
    #[must_use]
    pub fn check(
        &self,
        path: &Path,
        lineage: &Lineage,
        integration: IntegrationId,
        owners: &Owners,
    ) -> Option<Protection> {
        if path.parent().is_none() {
            return Some(Protection::FileSystemRoot);
        }
        if self
            .home
            .as_ref()
            .is_none_or(|home| holds_home(path, lineage, home))
        {
            return Some(Protection::Home);
        }
        let asked = Asked {
            path,
            lineage,
            integration,
            owners,
        };
        let closed = self
            .subresources
            .iter()
            .any(|held| held.holds(path, lineage))
            || self.unresolved.contains(&integration)
            || self
                .state
                .iter()
                .filter(|root| root.held.holds(path, lineage))
                .any(|root| !self.consents(root, asked));
        closed.then_some(Protection::Subresource)
    }

    /// Whether `root`, which reaches the path, lets the finding through. A path above a
    /// root never gets through: removing it removes all of the root.
    fn consents(&self, root: &StateRoot, asked: Asked<'_>) -> bool {
        below(&root.held, asked)
            .is_some_and(|below| self.stands_aside(root, asked) || root.opens(&below, asked))
    }

    /// Whether the path is inside a root that `root` delegates to, of the same integration,
    /// and that root opens it itself. Delegation is one level: asking only whether the
    /// child reaches the path would let a cycle, or a root naming itself, consent to anything.
    fn stands_aside(&self, root: &StateRoot, asked: Asked<'_>) -> bool {
        self.state
            .iter()
            .filter(|child| child.owner == root.owner)
            .filter(|child| root.declaration.delegates.contains(&child.declaration.root))
            .any(|child| below(&child.held, asked).is_some_and(|below| child.opens(&below, asked)))
    }
}

impl StateRoot {
    /// Whether this root opens a path with the components `below` under it to the finding.
    /// Rules that aren't first-party never get in, whatever a declaration lists.
    fn opens(&self, below: &[String], asked: Asked<'_>) -> bool {
        asked.integration != IntegrationId::GENERIC
            && asked.owners.status(self.owner) == OwnerStatus::Supported
            && self
                .declaration
                .open
                .iter()
                .any(|area| area.integrations.contains(&asked.integration) && area.at.holds(below))
            && !self
                .declaration
                .protected
                .iter()
                .any(|entry| entry.touches(below))
    }
}

/// The components of the asked path below `held`, by path or else by identity, or `None`
/// when it is above it, elsewhere, or anywhere its ancestry can't show.
fn below(held: &HeldPath, asked: Asked<'_>) -> Option<Vec<String>> {
    if !asked.lineage.is_known() {
        return None;
    }
    let names: Vec<Component<'_>> = asked.path.components().collect();
    let by_path = held
        .forms
        .iter()
        .find(|form| starts_with(asked.path, form))
        .and_then(|form| names.len().checked_sub(form.components().count()));
    let by_identity = || {
        let root = held.lineage.own()?;
        asked.lineage.0.iter().position(|id| *id == Some(root))
    };
    let depth = by_path.or_else(by_identity)?;
    let first = names.len().checked_sub(depth)?;
    names
        .get(first..)?
        .iter()
        .map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => None,
        })
        .collect()
}

/// Whether removing an item at `path` could remove the home directory: it is home or a
/// directory above it, by path or by identity, or that can't be ruled out.
fn holds_home(path: &Path, lineage: &Lineage, home: &HeldPath) -> bool {
    if home.forms.is_empty() || !home.lineage.is_known() {
        return true;
    }
    if home.forms.iter().any(|form| starts_with(form, path)) {
        return true;
    }
    lineage
        .own()
        .is_none_or(|own| home.lineage.0.contains(&Some(own)))
}
