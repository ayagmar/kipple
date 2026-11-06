//! The facts the host establishes for each finding once it is measured: whether a git
//! repository could track files inside it (`docs/04-safety-model.md` §2a), and in
//! [`activity`](crate::activity) whether a running program uses it (§4). Integrations
//! never supply these. Here too every predicate of a rule is evaluated: a fact that isn't
//! known never satisfies one.
//!
//! Each fact has one source. The host answers every fact of [`GENERIC_FACTS`], and nothing
//! an integration supplies can change that answer, known or not. The one exception is
//! [`GIT_STATUS`] on a subject that isn't plain, which is git's to answer. An integration
//! answers the facts it publishes, which are the names that aren't generic.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::integration::Evidence;
use crate::policy::{Activity, Clause, FactState, Facts, Modified, Tracked, Unmet};
use crate::probe::{FsProbe, ProbeError};
use crate::rule::{Bounds, FactKind, FactSpec, GENERIC_FACTS, Predicate, Rule, Test};
use crate::size::{Completeness, SpaceEstimate};
use crate::supplied::{FactValue, Subject};

/// What only git can say about a subject that isn't a plain directory.
const GIT_STATUS: &[&str] = &["git.tracked_changes", "git.untracked_files"];

/// What one directory holds, as far as git goes.
#[derive(Debug, Clone)]
enum GitEntry {
    Absent,
    Present,
    Unchecked(ProbeError),
}

/// What is above an item, as far as git goes.
#[derive(Debug, Clone)]
pub(crate) enum Above {
    /// No `.git` in it or any directory above it, each checked.
    Nothing,
    /// A `.git` in this directory, the nearest.
    Repository(PathBuf),
    /// A directory couldn't be checked.
    Unchecked(ProbeError),
}

/// The repository checks of one scan. Findings share most of their ancestors, so each
/// directory is checked once.
#[derive(Debug, Default)]
pub(crate) struct Repositories(Mutex<HashMap<PathBuf, GitEntry>>);

impl Repositories {
    /// Looks for `.git` in `item` and in every directory above it, up to the file system's
    /// root, crossing mounts. Only when each one could be checked and none has it is
    /// nothing inside tracked.
    pub(crate) fn above(&self, fs: &dyn FsProbe, item: &Path) -> Above {
        for dir in item.ancestors() {
            match self.entry(fs, dir) {
                GitEntry::Absent => {}
                GitEntry::Present => return Above::Repository(dir.to_owned()),
                GitEntry::Unchecked(error) => return Above::Unchecked(error),
            }
        }
        Above::Nothing
    }

    fn entry(&self, fs: &dyn FsProbe, dir: &Path) -> GitEntry {
        let known = |cache: &Self| {
            let cache = cache.0.lock().unwrap_or_else(PoisonError::into_inner);
            cache.get(dir).cloned()
        };
        if let Some(entry) = known(self) {
            return entry;
        }
        let entry = match fs.metadata(&dir.join(".git")) {
            // A `.git` file, link or directory all mean a repository.
            Ok(_) => GitEntry::Present,
            // Below something that isn't a directory, nothing can be there either. Each
            // directory above is still asked.
            Err(error)
                if matches!(
                    error.io_kind(),
                    Some(io::ErrorKind::NotFound | io::ErrorKind::NotADirectory)
                ) =>
            {
                GitEntry::Absent
            }
            Err(error) => GitEntry::Unchecked(error),
        };
        let mut cache = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        cache.insert(dir.to_owned(), entry.clone());
        entry
    }
}

/// What a measured finding's predicates are evaluated over.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Evaluated<'finding> {
    /// Why its integration thinks it is what its rule says.
    pub(crate) evidence: &'finding [Evidence],
    /// What measuring it found, when it was measured.
    pub(crate) size: Option<SpaceEstimate>,
    /// What the host and its integration established.
    pub(crate) facts: &'finding Facts,
}

/// Every predicate of `rule` the finding doesn't satisfy, where `specs` are the facts its
/// integration publishes.
pub(crate) fn unmet(rule: &Rule, specs: &[FactSpec], finding: Evaluated<'_>) -> Vec<Unmet> {
    let clauses = [
        (Clause::Requirement, &rule.requirements),
        (Clause::Eligibility, &rule.eligibility),
    ];
    clauses
        .into_iter()
        .flat_map(|(clause, predicates)| predicates.iter().map(move |p| (clause, p)))
        .filter_map(|(clause, predicate)| {
            let state = state(predicate, value(predicate.fact, specs, finding))?;
            Some(Unmet {
                fact: predicate.fact,
                clause,
                state,
            })
        })
        .collect()
}

/// How `predicate` falls short on `value`, if it does. A test of another type than the
/// value is unknown: the validator refuses one, and nothing here assumes it did.
fn state(predicate: &Predicate, value: FactValue) -> Option<FactState> {
    let met = match (&predicate.test, value) {
        (Test::OneOf(values), FactValue::Enum(value)) => values.contains(&value),
        (Test::Is(wanted), FactValue::Bool(value)) => *wanted == value,
        (Test::Duration(bounds), FactValue::Duration(value)) => within(bounds, value),
        (Test::Bytes(bounds), FactValue::Bytes(value))
        | (Test::Count(bounds), FactValue::Count(value)) => within(bounds, value),
        _ => return Some(FactState::Unknown),
    };
    (!met).then_some(FactState::Failed)
}

fn within<T: PartialOrd + Copy>(bounds: &Bounds<T>, value: T) -> bool {
    bounds.min.is_none_or(|min| value >= min) && bounds.max.is_none_or(|max| value <= max)
}

/// The value of the fact `name`, from its one source. Unknown when that source has no
/// answer, when nothing publishes the fact, or when the value isn't of the fact's type.
fn value(name: &str, specs: &[FactSpec], finding: Evaluated<'_>) -> FactValue {
    let spec = GENERIC_FACTS
        .iter()
        .chain(specs)
        .find(|spec| spec.name == name);
    let supplied = &finding.facts.supplied;
    let value = if integration_answers(name, &supplied.subject) {
        let value = supplied.values.get(name).copied();
        value.unwrap_or(FactValue::Unknown)
    } else {
        generic(name, finding)
    };
    match spec {
        Some(spec) if fits(spec.kind, value) => value,
        Some(_) | None => FactValue::Unknown,
    }
}

/// Whether the integration is the source of the fact `name` about `subject`, not the host.
fn integration_answers(name: &str, subject: &Subject) -> bool {
    let generic = GENERIC_FACTS.iter().any(|spec| spec.name == name);
    !generic || (*subject != Subject::Plain && GIT_STATUS.contains(&name))
}

/// Whether `value` is one `kind` allows.
fn fits(kind: FactKind, value: FactValue) -> bool {
    match (kind, value) {
        (FactKind::Enum(values), FactValue::Enum(value)) => values.contains(&value),
        (FactKind::Bool, FactValue::Bool(_))
        | (FactKind::Duration, FactValue::Duration(_))
        | (FactKind::Bytes, FactValue::Bytes(_))
        | (FactKind::Count, FactValue::Count(_)) => true,
        _ => false,
    }
}

/// What the host established about the generic fact `name`. Unknown for a fact it has no
/// mapping for, and where the mapping doesn't cover what it found.
fn generic(name: &str, finding: Evaluated<'_>) -> FactValue {
    let facts = finding.facts;
    match name {
        "activity" => match facts.activity {
            Activity::Idle | Activity::IdleExceptSystem { .. } => FactValue::Enum("idle"),
            Activity::InUse { .. } => FactValue::Enum("busy"),
            Activity::Unknown(_) => FactValue::Unknown,
        },
        "modified_age" => match facts.modified {
            Modified::Ago(age) => FactValue::Duration(age),
            Modified::Unknown(_) => FactValue::Unknown,
        },
        "size" => finding.size.map_or(FactValue::Unknown, size),
        "marker.present" => FactValue::Bool(
            finding
                .evidence
                .iter()
                .any(|evidence| matches!(evidence, Evidence::Marker(_))),
        ),
        // The host speaks for plain directories. A release or a worktree is more than that:
        // around a worktree "no repository" says nothing about the checkout itself.
        "git.tracked_files_in_item" | "git.tracked_changes" | "git.untracked_files"
            if facts.supplied.subject != Subject::Plain =>
        {
            FactValue::Unknown
        }
        "git.tracked_files_in_item" => match facts.tracked {
            Tracked::NoRepository => FactValue::Count(0),
            Tracked::Asked { tracked, .. } => FactValue::Count(tracked),
            Tracked::InsideRepository { .. }
            | Tracked::Unchecked(_)
            | Tracked::HoldsRepository { .. }
            | Tracked::Unwalked(_) => FactValue::Unknown,
        },
        // With no repository there is no work tree to have changes in. Inside one the host
        // doesn't know: only git's integration does.
        "git.tracked_changes" | "git.untracked_files" => match facts.tracked {
            Tracked::NoRepository => FactValue::Count(0),
            Tracked::Asked { .. }
            | Tracked::InsideRepository { .. }
            | Tracked::Unchecked(_)
            | Tracked::HoldsRepository { .. }
            | Tracked::Unwalked(_) => FactValue::Unknown,
        },
        _ => FactValue::Unknown,
    }
}

/// `estimate`'s size, unless something inside wasn't measured: a lower bound can pass a
/// `min` it wouldn't, or a `max` it shouldn't.
fn size(estimate: SpaceEstimate) -> FactValue {
    match estimate.completeness {
        Completeness::Complete => {
            let bytes = estimate
                .allocated
                .map_or(estimate.apparent.0, |bytes| bytes.0);
            FactValue::Bytes(bytes)
        }
        Completeness::Incomplete { .. } | Completeness::Cancelled => FactValue::Unknown,
    }
}
