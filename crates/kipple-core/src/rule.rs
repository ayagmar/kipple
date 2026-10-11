//! Rules: declarative policy over published selectors, facts and operations
//! (`docs/06-adapters-and-rules.md` §3). The adapters crate validates rule packs into
//! these types, and the engine evaluates them.

use std::fmt;
use std::time::Duration;

use crate::integration::IntegrationId;
use crate::root::SymbolicRoot;

/// A rule pack's ID, such as `builtin.cargo`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackId(pub String);

/// A rule's ID, such as `builtin.cargo.target`. Always prefixed by its pack's ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleId(pub String);

impl fmt::Display for PackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0)
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0)
    }
}

named! {
    /// What a finding is (`docs/03-functional-spec.md` §1). For display and grouping
    /// only: a category never authorizes anything.
    Category {
        /// A cache the tool regenerates.
        Cache = "cache",
        /// Build output.
        BuildArtifact = "build-artifact",
        /// Installed dependencies, such as `node_modules`.
        DependencyInstall = "dependency-install",
        /// An installed release of a tool.
        ReleaseInstall = "release-install",
        /// Logs.
        Log = "log",
        /// History, such as sessions.
        History = "history",
        /// Models.
        Model = "model",
        /// A source checkout, such as a worktree.
        SourceWorktree = "source-worktree",
        /// A package manager's download cache.
        PackageCache = "package-cache",
        /// The OS trash.
        Trash = "trash",
    }
}

named! {
    /// An operating system a pack or rule can apply to.
    Platform {
        /// Linux.
        Linux = "linux",
        /// macOS.
        Macos = "macos",
        /// Windows.
        Windows = "windows",
    }
}

named! {
    /// The home screen's secondary grouping (`docs/03-functional-spec.md` §2 J1).
    Lens {
        /// What AI coding agents leave behind.
        Agents = "agents",
        /// Developer tools and projects.
        Dev = "dev",
        /// The operating system and its package managers.
        System = "system",
    }
}

named! {
    /// How an allowed `remove-files` operation would run.
    Method {
        /// Permanent deletion.
        Delete = "delete",
        /// The OS trash.
        Trash = "trash",
    }
}

/// A name inside a directory: a literal pattern component, such as `node_modules`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name(pub String);

/// A relative pattern of one to eight components. `*` and `?` match within a single
/// component; there is no recursive wildcard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern(pub Vec<String>);

/// How a rule finds candidates under its root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// Directories whose path relative to the root matches the pattern.
    DirsMatching {
        /// The pattern.
        pattern: Pattern,
        /// More matches than this are dropped and reported as an omission.
        max_matches: u32,
    },
    /// Regular files whose path relative to the root matches the pattern.
    FilesMatching {
        /// The pattern.
        pattern: Pattern,
        /// More matches than this are dropped and reported as an omission.
        max_matches: u32,
    },
    /// A directory named `artifact` whose parent holds one of the `markers`, found by a
    /// walk of the root that never descends into a candidate.
    ProjectArtifact {
        /// File names that mark a project.
        markers: Vec<Name>,
        /// The artifact directory's name.
        artifact: Name,
        /// How many directories below the root a candidate may be.
        max_depth: u32,
    },
    /// A selector the rule's own integration publishes.
    Integration(&'static str),
}

impl Selector {
    /// The name rules use for this selector.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::DirsMatching { .. } => "dirs-matching",
            Self::FilesMatching { .. } => "files-matching",
            Self::ProjectArtifact { .. } => "project-artifact",
            Self::Integration(name) => name,
        }
    }
}

/// The type of a fact, which decides which predicate forms it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactKind {
    /// One of a fixed set of values.
    Enum(&'static [&'static str]),
    /// True or false.
    Bool,
    /// A length of time.
    Duration,
    /// A number of bytes.
    Bytes,
    /// A number of things.
    Count,
}

/// A fact rules may test, as the engine publishes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactSpec {
    /// Its name, such as `modified_age`.
    pub name: &'static str,
    /// Its type.
    pub kind: FactKind,
    /// The one selector it applies to, or `None` for every selector.
    pub selector: Option<&'static str>,
    /// What it means, for the generated catalog.
    pub meaning: &'static str,
}

/// The generic facts of schema v1, which every pack may use
/// (`docs/06-adapters-and-rules.md` §3).
pub const GENERIC_FACTS: &[FactSpec] = &[
    FactSpec {
        name: "activity",
        kind: FactKind::Enum(&["idle", "busy"]),
        selector: None,
        meaning: "whether the owning tool is using the item, supplied by the host",
    },
    FactSpec {
        name: "modified_age",
        kind: FactKind::Duration,
        selector: None,
        meaning: "time since the newest modification inside the item",
    },
    FactSpec {
        name: "size",
        kind: FactKind::Bytes,
        selector: None,
        meaning: "allocated size, or apparent size where allocation isn't reported",
    },
    FactSpec {
        name: "marker.present",
        kind: FactKind::Bool,
        selector: Some("project-artifact"),
        meaning: "whether a project marker sits next to the artifact",
    },
    FactSpec {
        name: "git.tracked_files_in_item",
        kind: FactKind::Count,
        selector: None,
        meaning: "files inside the item that a git repository tracks",
    },
    FactSpec {
        name: "git.tracked_changes",
        kind: FactKind::Count,
        selector: None,
        meaning: "modified tracked files in the enclosing work tree",
    },
    FactSpec {
        name: "git.untracked_files",
        kind: FactKind::Count,
        selector: None,
        meaning: "untracked, non-ignored files in the enclosing work tree",
    },
];

/// The generic selectors of schema v1, with their parameters.
pub const GENERIC_SELECTORS: &[(&str, &[&str])] = &[
    ("dirs-matching", &["pattern", "max_matches"]),
    ("files-matching", &["pattern", "max_matches"]),
    ("project-artifact", &["marker", "artifact", "max_depth"]),
];

/// The operations this engine publishes. More arrive with the integrations that can
/// carry them out.
pub const OPERATIONS: &[&str] = &["report", "remove-files"];

/// A range test. At least one bound is set, and `min <= max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds<T> {
    /// The smallest accepted value.
    pub min: Option<T>,
    /// The largest accepted value.
    pub max: Option<T>,
}

/// What a predicate accepts. `unknown` never satisfies any of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Test {
    /// One of these values of an enum fact.
    OneOf(Vec<&'static str>),
    /// This value of a bool fact.
    Is(bool),
    /// A duration in this range.
    Duration(Bounds<Duration>),
    /// A byte count in this range.
    Bytes(Bounds<u64>),
    /// A count in this range. `absent` is 0 to 0, `present` is at least 1.
    Count(Bounds<u64>),
}

/// A test of one fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predicate {
    /// The fact's name.
    pub fact: &'static str,
    /// What it must be.
    pub test: Test,
}

/// What a rule's operation is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Report the finding; never act on it.
    Report,
    /// Remove a frozen set of files, by one of these methods.
    RemoveFiles(Vec<Method>),
}

impl Operation {
    /// The name rules use for this operation.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::RemoveFiles(_) => "remove-files",
        }
    }
}

/// The text a rule shows people. Display only: none of it is input to a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleText {
    /// A short title.
    pub title: String,
    /// A longer description, possibly empty.
    pub description: String,
    /// Why this exists on the machine.
    pub why: String,
    /// What removing it loses.
    pub loss: String,
    /// Upstream documentation backing the rule (HTTPS URLs).
    pub references: Vec<String>,
}

/// A validated rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Its ID.
    pub id: RuleId,
    /// The integration that owns it: `generic`, or a built-in integration.
    pub integration: IntegrationId,
    /// What it shows people.
    pub text: RuleText,
    /// What its findings are.
    pub category: Category,
    /// Where it applies.
    pub platforms: Vec<Platform>,
    /// The root it selects under.
    pub root: SymbolicRoot,
    /// How it finds candidates.
    pub selector: Selector,
    /// Facts that must hold for the rule to apply on this machine at all.
    pub requirements: Vec<Predicate>,
    /// Facts each item must satisfy to be eligible.
    pub eligibility: Vec<Predicate>,
    /// What may be done with its findings.
    pub operation: Operation,
    /// A recent window longer than the carefulness level's, in days.
    pub recent_days: Option<u16>,
}

/// A validated rule pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    /// Its ID.
    pub id: PackId,
    /// Its version, a semantic version.
    pub version: String,
    /// Who publishes it.
    pub publisher: String,
    /// What it is for.
    pub description: String,
    /// Its license.
    pub license: String,
    /// Where it comes from, when it was fetched.
    pub source: Option<String>,
    /// The semantic-version requirement on the engine.
    pub kipple: String,
    /// The platforms it supports.
    pub platforms: Vec<Platform>,
    /// Its rules, in document order.
    pub rules: Vec<Rule>,
}
