//! Rules: declarative policy over published selectors, facts and operations
//! (`docs/06-adapters-and-rules.md` §3). The adapters crate validates rule packs into
//! these types, and the engine evaluates them.

use std::fmt;
use std::time::Duration;

use crate::integration::IntegrationId;
use crate::native::NativeKind;
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

/// An operation this engine publishes (`docs/06-adapters-and-rules.md` §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationSpec {
    /// The name rules use for it.
    pub name: &'static str,
    /// Whether only built-in packs may name it. The table says so, never an ID prefix.
    pub first_party: bool,
    /// Whether a rule using it must carry a `[rule.retention]` table.
    pub retention: bool,
    /// The methods a rule using it may offer, none for one that chooses no method.
    pub methods: &'static [Method],
    /// What it does, for the generated catalog.
    pub meaning: &'static str,
}

/// The operations this engine publishes.
pub const OPERATIONS: &[OperationSpec] = &[
    OperationSpec {
        name: "report",
        first_party: false,
        retention: false,
        methods: &[],
        meaning: "shows the finding and never acts on it",
    },
    OperationSpec {
        name: "remove-files",
        first_party: false,
        retention: false,
        methods: Method::ALL,
        meaning: "removes a frozen set of files",
    },
    OperationSpec {
        name: "remove-verified-release",
        first_party: true,
        retention: true,
        methods: Method::ALL,
        meaning: "removes a frozen release that retention doesn't keep",
    },
    OperationSpec {
        name: NativeKind::GitWorktreeRemove.name(),
        first_party: true,
        retention: false,
        methods: &[],
        meaning: "runs `git worktree remove` on a registered linked worktree",
    },
    OperationSpec {
        name: NativeKind::NpmCacheClean.name(),
        first_party: true,
        retention: false,
        methods: &[],
        meaning: "runs `npm cache clean` on npm's download cache",
    },
];

named! {
    /// What a release's retention group is keyed on.
    GroupKey {
        /// The store it is in, such as Codex's `standalone`.
        Component = "component",
        /// The platform it was built for.
        Target = "target",
    }
}

/// How a release's group is formed. What is kept in each group is fixed: the host keeps
/// everything the tool's own prune would (`docs/06-adapters-and-rules.md` §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retention {
    /// What groups its releases.
    pub group_by: Vec<GroupKey>,
}

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
    /// Remove a frozen release that retention doesn't keep, by one of these methods.
    RemoveVerifiedRelease {
        /// How it may be removed.
        methods: Vec<Method>,
        /// How its releases are grouped.
        retention: Retention,
    },
    /// Run a tool's own command.
    Native(NativeKind),
}

impl Operation {
    /// The name rules use for this operation.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::RemoveFiles(_) => "remove-files",
            Self::RemoveVerifiedRelease { .. } => "remove-verified-release",
            Self::Native(kind) => kind.name(),
        }
    }

    /// Whether it removes a frozen set of files: what a profile can hold an entry for. A
    /// native command removes what it finds when it runs, so it can't.
    #[must_use]
    pub const fn removes_frozen_files(&self) -> bool {
        match self {
            Self::RemoveFiles(_) | Self::RemoveVerifiedRelease { .. } => true,
            Self::Report | Self::Native(_) => false,
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
