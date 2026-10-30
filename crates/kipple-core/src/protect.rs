//! The host's protected sets: what no plan touches, whatever any rule says
//! (`docs/04-safety-model.md` §3.2, §3.9).
//!
//! The planner checks every selected item against them before it merges or freezes
//! anything, and apply checks again before it runs anything. They hold whether the
//! integration that declared a subresource ran, failed or is absent.

use std::fmt;
use std::path::Path;

use crate::policy::{HeldPath, Lineage, starts_with};

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

/// The paths the host protects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Protections {
    /// The home directory, matched by path and by identity. `None` when it is unknown:
    /// then no item can be shown not to hold it, and every one is protected.
    pub home: Option<HeldPath>,
    /// Every integration's protected subresources, whether it runs here or not.
    pub subresources: Vec<HeldPath>,
}

impl Protections {
    /// What protects an item at `path`, with the identities `lineage` of it and the
    /// directories above it, if anything does.
    #[must_use]
    pub fn check(&self, path: &Path, lineage: &Lineage) -> Option<Protection> {
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
        self.subresources
            .iter()
            .any(|held| held.holds(path, lineage))
            .then_some(Protection::Subresource)
    }
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
