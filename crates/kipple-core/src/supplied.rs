//! What an integration supplies about a finding (`docs/06-adapters-and-rules.md` §3, spec
//! M4 pinned 1): typed facts its rules may test, and the subject they describe. It only
//! supplies them: the host evaluates every predicate, so a fact missing, of the wrong type
//! or unknown never satisfies one.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::integration::{ActivityEvidence, Evidence};
use crate::policy::Consequence;

/// A fact's value. A runtime string never enters it, so a version is an enum fact, such as
/// `supported`, and the version itself stays in the finding's [`Subject`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactValue {
    /// One of the values its [`FactSpec`](crate::FactSpec) lists.
    Enum(&'static str),
    /// True or false.
    Bool(bool),
    /// A number of things.
    Count(u64),
    /// A length of time.
    Duration(Duration),
    /// A number of bytes.
    Bytes(u64),
    /// It couldn't be established, which never satisfies a predicate.
    Unknown,
}

/// An entry of the advice an integration gave in this scan, as
/// [`FindingSink::advise`](crate::FindingSink::advise) returns it: its place in
/// [`IntegrationReport::advice`](crate::IntegrationReport::advice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdviceId(pub usize);

/// What a finding is, past its path: which operations it can go with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// Files and directories, and nothing more to say about them.
    Plain,
}

/// Everything an integration supplies about one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supplied {
    /// Its facts, by name.
    pub values: BTreeMap<&'static str, FactValue>,
    /// What it is.
    pub subject: Subject,
    /// What can show it in use: one of the evidences its integration declares.
    pub activity: ActivityEvidence,
    /// The advice that applies to it, which makes a report rule point to its tool's own
    /// cleanup.
    pub advice: Option<AdviceId>,
}

impl Default for Supplied {
    /// A plain finding with nothing supplied, judged by the strictest evidence.
    fn default() -> Self {
        Self {
            values: BTreeMap::new(),
            subject: Subject::Plain,
            activity: ActivityEvidence::Everything,
            advice: None,
        }
    }
}

/// What an integration claims about a finding it adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// Why it thinks the finding is what its rule says.
    pub evidence: Vec<Evidence>,
    /// What removing it costs, when the integration verified that.
    pub consequence: Option<Consequence>,
}
