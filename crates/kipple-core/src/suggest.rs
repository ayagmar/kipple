//! Suggested project roots (D-053, `docs/03-functional-spec.md` §6): which directories to
//! offer as project roots, from the projects a search of the home directory found. Pure:
//! the platform walks, this only decides.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::policy::{HeldPath, starts_with};

/// A directory to offer as a project root, with the projects found in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// The directory.
    pub path: PathBuf,
    /// The projects found in it, sorted.
    pub projects: Vec<PathBuf>,
}

/// The project roots to suggest for the `projects` found below `home`.
///
/// A project's parent is the candidate, or the project itself when its parent is home. A
/// candidate inside another is folded into it. Home itself, a filesystem root, an existing
/// project root (any of `roots`, which holds every form a root is known by) or a path
/// inside one, and an excluded path or one inside it are never suggested. A project not
/// below home is ignored. Sorted by path.
#[must_use]
pub fn suggest(
    home: &Path,
    projects: &[PathBuf],
    roots: &[PathBuf],
    exclude: &[HeldPath],
) -> Vec<Suggestion> {
    let mut candidates: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for project in projects {
        let Some(candidate) = candidate(home, project) else {
            continue;
        };
        // Strictly inside home by construction, so never home or a file system root.
        let refused = roots.iter().any(|root| is_within(&candidate, root))
            || exclude.iter().any(|excluded| excluded.covers(&candidate));
        if !refused {
            candidates
                .entry(candidate)
                .or_default()
                .push(project.clone());
        }
    }
    fold(candidates)
}

/// Whether `path` is `base` or inside it, by components and ignoring case, as config
/// paths compare: a match only ever leaves out more.
#[must_use]
pub fn is_within(path: &Path, base: &Path) -> bool {
    starts_with(path, base)
}

/// The directory to suggest for `project`: its parent, or itself when its parent is
/// `home`. `None` when it isn't below home.
fn candidate(home: &Path, project: &Path) -> Option<PathBuf> {
    if !is_within(project, home) || is_within(home, project) {
        return None;
    }
    let parent = project.parent()?;
    Some(if is_within(home, parent) {
        project.to_owned()
    } else {
        parent.to_owned()
    })
}

/// Folds every candidate inside another into the outermost one, shallowest first so an
/// outer candidate is placed before everything inside it, whatever its case.
fn fold(candidates: BTreeMap<PathBuf, Vec<PathBuf>>) -> Vec<Suggestion> {
    let mut shallowest: Vec<_> = candidates.into_iter().collect();
    shallowest.sort_by_key(|(path, _)| path.components().count());
    let mut folded: Vec<Suggestion> = Vec::new();
    for (path, projects) in shallowest {
        match folded
            .iter_mut()
            .find(|outer| is_within(&path, &outer.path))
        {
            Some(outer) => outer.projects.extend(projects),
            None => folded.push(Suggestion { path, projects }),
        }
    }
    for suggestion in &mut folded {
        suggestion.projects.sort();
    }
    folded.sort_by(|a, b| a.path.cmp(&b.path));
    folded
}
