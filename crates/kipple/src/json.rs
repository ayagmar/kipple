//! The versioned JSON output (`docs/03-functional-spec.md` §4). These types are the
//! contract: `--json`, `--events jsonl` and the schemas `cargo xtask gen-docs` publishes
//! are all generated from them.

use std::collections::BTreeMap;
use std::path::Path;

use kipple_core::{
    Advice, Completeness, Consequence, Evidence, Finding, IntegrationId, IntegrationReport, Lens,
    Level, Omission, ScanOutcome, ScanReport, SpaceEstimate, SymbolicRoot, UnresolvedRoot, Verdict,
};
use kipple_platform::{PLATFORM, raw_path};
use schemars::JsonSchema;
use serde::Serialize;

/// The schema of `kipple scan --json`.
pub const SCAN: &str = "kipple.scan/v1";
/// The schema of each `kipple scan --events jsonl` record.
pub const EVENT: &str = "kipple.event/v1";
/// The schema of `kipple rules --json`.
pub const RULES: &str = "kipple.rules/v1";
/// The schema of `kipple doctor --json`.
pub const DOCTOR: &str = "kipple.doctor/v1";
/// The schema of the generated support matrix.
pub const SUPPORT_MATRIX: &str = "kipple.support-matrix/v1";

/// kipple's version, in every envelope.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A path: its display form, plus its exact bytes when it isn't valid Unicode.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PathOut {
    /// The path, with anything that isn't valid Unicode replaced.
    pub display: String,
    /// Base64 of the exact bytes (Unix) or UTF-16LE code units (Windows), present only
    /// when `display` lost something.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

impl PathOut {
    /// `path` in both forms.
    #[must_use]
    pub fn new(path: &Path) -> Self {
        Self {
            display: path.to_string_lossy().into_owned(),
            raw: raw_path(path).map(|bytes| base64(&bytes)),
        }
    }
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                let index = (n >> (18 - 6 * i)) & 63;
                out.push(char::from(
                    ALPHABET.get(index as usize).copied().unwrap_or(b'='),
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Whether a scan ran to the end.
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OutcomeOut {
    /// Every integration finished and every finding was sized.
    Completed,
    /// The scan was cancelled; the report holds what was found until then.
    Cancelled,
}

impl From<ScanOutcome> for OutcomeOut {
    fn from(outcome: ScanOutcome) -> Self {
        match outcome {
            ScanOutcome::Completed => Self::Completed,
            ScanOutcome::Cancelled => Self::Cancelled,
        }
    }
}

/// `kipple scan --json`: one envelope once the scan finished.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScanOut {
    /// `kipple.scan/v1`.
    pub schema: &'static str,
    /// kipple's version.
    pub kipple: &'static str,
    /// The platform it ran on.
    pub platform: &'static str,
    /// Whether the scan ran to the end.
    pub outcome: OutcomeOut,
    /// The carefulness level each finding's bucket was derived at.
    pub level: &'static str,
    /// Every finding, sorted by lens, rule, then path.
    pub findings: Vec<FindingOut>,
    /// Every integration that ran, sorted by lens, then ID.
    pub integrations: Vec<IntegrationOut>,
}

impl ScanOut {
    /// The envelope of `report`, each integration with its lens from `lenses`.
    #[must_use]
    pub fn new(report: &ScanReport, lenses: &BTreeMap<IntegrationId, Lens>) -> Self {
        let lens = |id| lenses.get(&id).copied().unwrap_or(Lens::System);
        let mut findings: Vec<FindingOut> = report
            .findings
            .iter()
            .map(|finding| FindingOut::new(finding, lens(finding.integration)))
            .collect();
        findings.sort_by(|a, b| a.order().cmp(&b.order()));
        let mut integrations: Vec<IntegrationOut> = report
            .integrations
            .iter()
            .map(|report| IntegrationOut::new(report, lens(report.id)))
            .collect();
        integrations.sort_by(|a, b| (a.lens, &a.id).cmp(&(b.lens, &b.id)));
        Self {
            schema: SCAN,
            kipple: VERSION,
            platform: PLATFORM.name(),
            outcome: report.outcome.into(),
            level: LEVEL.name(),
            findings,
            integrations,
        }
    }
}

/// A lens, as `--lens` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum LensOut {
    /// What AI coding agents leave behind.
    Agents,
    /// Developer tools and projects.
    Dev,
    /// The operating system and its package managers.
    System,
}

impl From<Lens> for LensOut {
    fn from(lens: Lens) -> Self {
        match lens {
            Lens::Agents => Self::Agents,
            Lens::Dev => Self::Dev,
            Lens::System => Self::System,
        }
    }
}

/// The carefulness level buckets are derived at, until it can be configured.
pub(crate) const LEVEL: Level = Level::Balanced;

/// A bucket of the home screen (`docs/04-safety-model.md` §2a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BucketOut {
    /// Passes every check at the level.
    Safe,
    /// Eligible, but loses something beyond the level or changed recently.
    Review,
    /// Blocked, unknown, report-only or advice only.
    Protected,
}

/// A finding's bucket and every reason for it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct VerdictOut {
    /// The bucket.
    pub bucket: BucketOut,
    /// Why it isn't Safe: for Review, what falls short of the level (`recent`, `recovery`,
    /// `loss`, `high-cost`); for Protected, what holds it (`report-only`, `advice`,
    /// `unknown-layout`, `unknown-consequence`, `unknown-git`, `in-use`,
    /// `unknown-activity`, `unknown-modified`). Empty when Safe.
    pub reasons: Vec<&'static str>,
}

impl From<&Verdict> for VerdictOut {
    fn from(verdict: &Verdict) -> Self {
        match verdict {
            Verdict::Safe => Self {
                bucket: BucketOut::Safe,
                reasons: Vec::new(),
            },
            Verdict::Review(cautions) => Self {
                bucket: BucketOut::Review,
                reasons: cautions.iter().map(|c| c.name()).collect(),
            },
            Verdict::Protected(holds) => Self {
                bucket: BucketOut::Protected,
                reasons: holds.iter().map(|h| h.name()).collect(),
            },
        }
    }
}

/// What removing a finding costs (`docs/04-safety-model.md` §2a).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConsequenceOut {
    /// How it comes back: `automatic`, `rebuild-local`, `reinstall`, `redownload` or
    /// `none`.
    pub recovery: &'static str,
    /// What is lost besides the bytes: `cache-warmth`, `rollback-ability`,
    /// `offline-ability`, `history`.
    pub loss: Vec<&'static str>,
    /// How expensive coming back is: `low`, `high` or `unknown`.
    pub cost: &'static str,
}

impl From<&Consequence> for ConsequenceOut {
    fn from(consequence: &Consequence) -> Self {
        Self {
            recovery: consequence.recovery.name(),
            loss: consequence.loss.iter().map(|loss| loss.name()).collect(),
            cost: consequence.cost.name(),
        }
    }
}

/// Something an integration found.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FindingOut {
    /// The integration that found it.
    pub integration: String,
    /// Its integration's lens.
    pub lens: LensOut,
    /// The rule it was found by.
    pub rule: String,
    /// What it is. Display only.
    pub category: &'static str,
    /// Where it is.
    pub path: PathOut,
    /// Why the integration thinks it is what the rule says.
    pub evidence: Vec<EvidenceOut>,
    /// What removing it would free, once sized.
    pub size: Option<SizeOut>,
    /// Its bucket at the report's level, once measured.
    pub verdict: Option<VerdictOut>,
    /// What removing it costs, once measured, when its integration verified that.
    pub consequence: Option<ConsequenceOut>,
}

impl FindingOut {
    /// `finding`, whose integration has `lens`.
    #[must_use]
    pub fn new(finding: &Finding, lens: Lens) -> Self {
        Self {
            integration: finding.integration.0.to_owned(),
            lens: lens.into(),
            rule: finding.rule.0.clone(),
            category: finding.category.name(),
            path: PathOut::new(&finding.path),
            evidence: finding.evidence.iter().map(EvidenceOut::from).collect(),
            size: finding.size.as_ref().map(SizeOut::from),
            verdict: finding.verdict(LEVEL).as_ref().map(VerdictOut::from),
            consequence: finding
                .facts
                .as_ref()
                .and_then(|facts| facts.consequence.as_ref())
                .map(ConsequenceOut::from),
        }
    }

    fn order(&self) -> (LensOut, &str, &str) {
        (self.lens, &self.rule, &self.path.display)
    }
}

/// One piece of evidence.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EvidenceOut {
    /// A file beside it marks a project.
    Marker {
        /// The file's name.
        file: String,
    },
    /// An entry inside it is one its tool writes.
    Signature {
        /// The entry's name.
        entry: String,
    },
    /// None of the entries its tool writes is inside: its layout is unknown.
    NoSignature,
    /// It is the root itself.
    Root {
        /// The symbolic root.
        root: &'static str,
    },
}

impl From<&Evidence> for EvidenceOut {
    fn from(evidence: &Evidence) -> Self {
        match evidence {
            Evidence::Marker(file) => Self::Marker { file: file.clone() },
            Evidence::Signature(entry) => Self::Signature {
                entry: entry.clone(),
            },
            Evidence::NoSignature => Self::NoSignature,
            Evidence::Root(root) => Self::Root { root: root.name() },
        }
    }
}

/// How much of an item was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CompletenessOut {
    /// Every entry was read.
    Complete,
    /// Some entries couldn't be read, so the sizes are lower bounds.
    Incomplete,
    /// Sizing stopped because the scan was cancelled.
    Cancelled,
}

/// What removing an item would free, in bytes.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SizeOut {
    /// The sum of file lengths.
    pub apparent: u64,
    /// Allocated storage, where the platform reports it.
    pub allocated: Option<u64>,
    /// Allocated storage that only this item holds, where the platform reports it.
    pub unique_reclaim: Option<u64>,
    /// Whether every entry was measured.
    pub completeness: CompletenessOut,
    /// How many entries couldn't be read, when incomplete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreadable: Option<u64>,
}

impl From<&SpaceEstimate> for SizeOut {
    fn from(size: &SpaceEstimate) -> Self {
        let (completeness, unreadable) = match size.completeness {
            Completeness::Complete => (CompletenessOut::Complete, None),
            Completeness::Incomplete { unreadable } => {
                (CompletenessOut::Incomplete, Some(unreadable))
            }
            Completeness::Cancelled => (CompletenessOut::Cancelled, None),
        };
        Self {
            apparent: size.apparent.0,
            allocated: size.allocated.map(|a| a.0),
            unique_reclaim: size.unique_reclaim.map(|a| a.0),
            completeness,
            unreadable,
        }
    }
}

/// One integration's part of a scan.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IntegrationOut {
    /// Its ID.
    pub id: String,
    /// Its lens.
    pub lens: LensOut,
    /// What it could not cover. Omissions make a report partial, never anything safe.
    pub omissions: Vec<OmissionOut>,
    /// Native settings and commands it suggests. kipple never runs them.
    pub advice: Vec<AdviceOut>,
}

impl IntegrationOut {
    fn new(report: &IntegrationReport, lens: Lens) -> Self {
        Self {
            id: report.id.0.to_owned(),
            lens: lens.into(),
            omissions: report.omissions.iter().map(OmissionOut::from).collect(),
            advice: report.advice.iter().map(AdviceOut::from).collect(),
        }
    }
}

/// Something a scan couldn't cover, and why.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OmissionOut {
    /// What kind of gap it is.
    pub kind: &'static str,
    /// The symbolic root it concerns, if any.
    pub root: Option<&'static str>,
    /// The path it concerns, if any.
    pub path: Option<PathOut>,
    /// What happened, with the upstream error. For people; not a stable value.
    pub message: String,
}

impl From<&Omission> for OmissionOut {
    fn from(omission: &Omission) -> Self {
        let (kind, root, path): (_, _, Option<&Path>) = match omission {
            Omission::RootUnresolved { root, reason } => {
                let path = match reason {
                    UnresolvedRoot::Link { path, .. }
                    | UnresolvedRoot::Unresolvable { path, .. } => Some(path.as_path()),
                    UnresolvedRoot::NotSet { .. }
                    | UnresolvedRoot::NotAbsolute { .. }
                    | UnresolvedRoot::BaseUnresolved { .. }
                    | UnresolvedRoot::Setting { .. }
                    | UnresolvedRoot::Os { .. } => None,
                };
                ("root-unresolved", Some(*root), path)
            }
            Omission::RootUnreadable { root, error } => {
                ("root-unreadable", Some(*root), Some(error.path()))
            }
            Omission::RootNotDirectory { root, path, .. } => {
                ("root-not-directory", Some(*root), Some(path))
            }
            Omission::RootChanged { root, path } => ("root-changed", Some(*root), Some(path)),
            Omission::Unlisted { error } => ("unlisted", None, Some(error.path())),
            Omission::DiscoveryFailed { error } => ("discovery-failed", None, Some(error.path())),
            Omission::SizingFailed { error, .. } => ("sizing-failed", None, Some(error.path())),
            Omission::Crashed => ("crashed", None, None),
        };
        Self {
            kind,
            root: root.map(SymbolicRoot::name),
            path: path.map(PathOut::new),
            message: omission.to_string(),
        }
    }
}

/// A native setting or command, shown and never run.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AdviceOut {
    /// A short title.
    pub title: String,
    /// What it does and what to watch out for.
    pub text: String,
    /// Exact commands or settings to copy.
    pub snippets: Vec<String>,
    /// Upstream documentation.
    pub references: Vec<String>,
}

impl From<&Advice> for AdviceOut {
    fn from(advice: &Advice) -> Self {
        Self {
            title: advice.title.clone(),
            text: advice.text.clone(),
            snippets: advice.snippets.clone(),
            references: advice.references.clone(),
        }
    }
}

/// One `kipple scan --events jsonl` record. Sequence numbers follow emission order.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EventOut {
    /// `kipple.event/v1`.
    pub schema: &'static str,
    /// Its position in the stream, from 0.
    pub seq: u64,
    /// What happened.
    #[serde(flatten)]
    pub event: EventBody,
}

/// What a record says happened.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum EventBody {
    /// The scan began.
    ScanStarted {
        /// How many integrations will run.
        integrations: usize,
    },
    /// An integration's root was found and will be read.
    RootOpened {
        /// The integration.
        integration: String,
        /// The symbolic root.
        root: &'static str,
        /// Where it is.
        path: PathOut,
    },
    /// An integration found something, not sized yet.
    FindingAdded {
        /// Its ID within this scan, for `finding-updated`.
        id: u64,
        /// What was found.
        finding: FindingOut,
    },
    /// A finding was measured, and its bucket derived.
    FindingUpdated {
        /// The finding's ID within this scan.
        id: u64,
        /// What removing it would free, unless it couldn't be sized.
        size: Option<SizeOut>,
        /// Its bucket at the report's level.
        verdict: VerdictOut,
    },
    /// An integration finished.
    IntegrationCompleted {
        /// The integration.
        integration: String,
        /// What it couldn't cover.
        omissions: Vec<OmissionOut>,
    },
    /// The engine finished.
    ScanCompleted {
        /// Whether it ran to the end.
        outcome: OutcomeOut,
    },
    /// The last record, written while stdout stays writable: the whole sorted report.
    Completed {
        /// The same report `--json` prints.
        report: Box<ScanOut>,
    },
}

/// Every published schema, by the file name the docs give it.
#[must_use]
pub fn schemas() -> Vec<(&'static str, schemars::Schema)> {
    vec![
        ("doctor", schemars::schema_for!(crate::doctor::DoctorOut)),
        ("event", schemars::schema_for!(EventOut)),
        ("rules", schemars::schema_for!(crate::rules::RulesOut)),
        ("scan", schemars::schema_for!(ScanOut)),
        (
            "support-matrix",
            schemars::schema_for!(crate::support::SupportMatrixOut),
        ),
    ]
}
