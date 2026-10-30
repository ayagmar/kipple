//! The words for what applying a plan did: item outcomes, skips, error codes, what
//! reconciliation found, restores, stored runs and how a command ended (spec M3b pinned
//! 17). The CLI's apply commands render these; every variant is worded here, so a new one
//! fails to build until it is.

use kipple_core::{
    ApplyError, Dropped, Ending, ErrorCode, Found, GitGap, JudgementField, LeftReason, Method,
    RestoreResult, Restoring, RunState, Skip, Status, Stop, StoreRefusal, Unjudged, Unreadable,
    Unsupported,
};

use super::{caution, hold};

/// An item's status, as a word.
pub(crate) const fn status(status: Status) -> &'static str {
    match status {
        Status::Done => "done",
        Status::Partial => "partly done",
        Status::Skipped => "skipped",
        Status::Failed => "failed",
        Status::Cancelled => "cancelled",
        Status::Indeterminate => "can't tell",
    }
}

/// Why an item was skipped, as a sentence fragment.
pub(crate) fn skip(skip: &Skip) -> String {
    match skip {
        Skip::NotReviewed => "it isn't in the plan that was reviewed".to_owned(),
        Skip::Rejudged(field) => format!("{} changed since review", judgement_field(*field)),
        Skip::Protected(holds) => format!(
            "it is Protected now: {}",
            list(holds.iter().map(|h| hold(*h)))
        ),
        Skip::Riskier(cautions) => format!(
            "it needs review in the TUI: {}",
            list(cautions.iter().map(|c| caution(*c)))
        ),
        Skip::MethodNotOffered(method) => {
            format!("its rule doesn't offer {}", method_name(*method))
        }
        Skip::Freeze(error) => format!("it couldn't be read again: {}", error_text(error)),
        Skip::Refused(error) => format!(
            "it was refused before anything changed: {}",
            error_text(error)
        ),
        Skip::FileSystem(why) => format!("it is on {}", unsupported(why)),
        Skip::AllDropped(_) => "everything reviewed in it changed since review".to_owned(),
        Skip::WholeNotReviewed => {
            "a trash move takes the directory whole, which needs a review".to_owned()
        }
        Skip::NotWhole { .. } => {
            "some of it changed since review, and a trash move takes it whole".to_owned()
        }
        Skip::NotStarted(why) => stop(*why).to_owned(),
        Skip::RootChanged => "the folder it is in isn't the one scanned".to_owned(),
        Skip::Unjudged(why) => unjudged(*why).to_owned(),
        Skip::HostProtected(_) => "your system protects it".to_owned(),
        Skip::HoldsProtected { .. } => "it holds something Protected".to_owned(),
        Skip::NotGranted => "its rule comes from a pack that isn't granted".to_owned(),
        Skip::Conflict(_) => "it overlaps another item".to_owned(),
        Skip::TrashUnavailable(error) => format!("the trash can't take it: {}", error_text(error)),
        Skip::Git(gap) => format!("git couldn't say what it tracks in it: {}", git_gap(*gap)),
    }
}

/// Why an apply stopped before an item.
pub(crate) const fn stop(stop: Stop) -> &'static str {
    match stop {
        Stop::Cancelled => "cancelled before it started",
        Stop::JournalFailed => "the run's record couldn't be written, so nothing more ran",
        Stop::Interrupted => "the run ended before it got there",
    }
}

const fn judgement_field(field: JudgementField) -> &'static str {
    match field {
        JudgementField::Rule => "its rule",
        JudgementField::Level => "the level",
        JudgementField::RecentWindow => "the recent window",
        JudgementField::Method => "the method",
        JudgementField::Held => "the kept and excluded paths",
        JudgementField::Policy => "the policy",
    }
}

const fn unjudged(why: Unjudged) -> &'static str {
    match why {
        Unjudged::NotFound => "the scan doesn't find it any more",
        Unjudged::Unmeasured => "it couldn't be measured",
        Unjudged::Unanchored => "where it is couldn't be pinned down",
    }
}

const fn method_name(method: Method) -> &'static str {
    match method {
        Method::Delete => "deleting",
        Method::Trash => "moving to the trash",
    }
}

fn unsupported(why: &Unsupported) -> String {
    match why {
        Unsupported::Network => "a network file system".to_owned(),
        Unsupported::UserSpace => "a user-space file system".to_owned(),
        Unsupported::NoPosixDelete => {
            "a file system like FAT, which kipple doesn't change".to_owned()
        }
        Unsupported::Unrecognised(name) => {
            format!(
                "a file system kipple doesn't know ({})",
                super::escaped(name)
            )
        }
    }
}

/// An error, with the entry it is about.
pub(crate) fn error_text(error: &ApplyError) -> String {
    let what = error_code(error.code);
    match &error.at {
        Some(at) if !at.is_empty() => {
            format!(
                "{what} at {}",
                super::escaped(&at.below(std::path::Path::new("")).display().to_string())
            )
        }
        Some(_) | None => what.to_owned(),
    }
}

/// kipple's error code, as a sentence fragment.
pub(crate) const fn error_code(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::RootChanged => "the folder it is in changed",
        ErrorCode::RootUnreadable => "the folder it is in can't be opened",
        ErrorCode::TargetMissing => "it isn't there",
        ErrorCode::TargetChanged => "something else is there now",
        ErrorCode::ParentChanged => "the folder holding it changed",
        ErrorCode::ParentMissing => "the folder holding it isn't there",
        ErrorCode::ThroughLink => "the way to it goes through a link",
        ErrorCode::MountBoundary => "it crosses into another drive",
        ErrorCode::UnsupportedFileSystem => "kipple doesn't change that file system",
        ErrorCode::UnknownEntry => {
            "it holds something kipple can't judge, such as a cloud placeholder"
        }
        ErrorCode::Unreadable => "a folder in it can't be listed",
        ErrorCode::TooLarge => "it holds too many entries",
        ErrorCode::Unsupported => "this system can't do that",
        ErrorCode::CrossFileSystem => "the trash is on another drive",
        ErrorCode::DestinationExists => "something is already where it would go",
        ErrorCode::TrashMissing => "it isn't in the trash any more",
        ErrorCode::TrashChanged => "something else is in the trash under its name",
        ErrorCode::Stranded => "something swapped in for it was moved and couldn't be moved back",
        ErrorCode::Unauthorized => "the run reached something it wasn't allowed to touch",
        ErrorCode::Journal => "the run's record couldn't be written",
        ErrorCode::Unsynced => {
            "the change couldn't be saved to disk, so a power loss could undo it"
        }
        ErrorCode::Cancelled => "cancelled",
        ErrorCode::Io => "the system refused",
    }
}

/// Why an entry the plan reviewed stays.
pub(crate) const fn left_reason(reason: LeftReason) -> &'static str {
    match reason {
        LeftReason::Gone => "already gone",
        LeftReason::Replaced => "replaced since review",
        LeftReason::Changed => "changed since review",
        LeftReason::Excluded => "left out of the plan",
        LeftReason::NotEmpty => "still holds something",
        LeftReason::Denied => "the system refused",
        LeftReason::Busy => "in use",
        LeftReason::Unsupported => "can't be removed here",
        LeftReason::Failed => "removing it failed",
        LeftReason::NotReached => "not reached",
    }
}

/// What reconciliation found of an item a crash interrupted. Never what kipple did.
pub(crate) fn found(found: &Found) -> String {
    match found {
        Found::Unchanged => "nothing of it was removed".to_owned(),
        Found::Gone => "it is gone".to_owned(),
        Found::Replaced => "something else is there now".to_owned(),
        Found::PartlyGone { gone, of } => format!("{gone} of its {of} entries are gone"),
        Found::Trashed(_) => "it is in the trash, and can be restored".to_owned(),
        Found::Unreadable(error) => format!("it couldn't be checked: {}", error_text(error)),
        Found::Unsynced(error) => format!("it can't be told: {}", error_text(error)),
        Found::Pending => "not checked yet".to_owned(),
    }
}

/// Why git couldn't say in full which files in an item it tracks.
pub(crate) const fn git_gap(gap: GitGap) -> &'static str {
    match gap {
        GitGap::Missing => "git isn't installed",
        GitGap::TooOld => "git is older than 2.32",
        GitGap::Failed => "git failed",
        GitGap::Truncated => "git's answer was cut short",
        GitGap::OtherWorkTree => "git names another work tree",
        GitGap::Unplaced => "git named a file kipple couldn't find",
        GitGap::Unpassable => "its path can't be passed to git",
        GitGap::NestedRepository => "another repository lives inside it",
        GitGap::Unsupported => "this build doesn't ask git",
    }
}

/// How a restore ended.
pub(crate) fn restore_result(result: &RestoreResult) -> String {
    match result {
        RestoreResult::Restored => "restored".to_owned(),
        RestoreResult::Failed(error) => format!("not restored: {}", error_text(error)),
        RestoreResult::Indeterminate(seen) => format!("interrupted: {}", found(seen)),
    }
}

/// What `restore` did with one item.
pub(crate) fn restoring(restoring: &Restoring) -> String {
    match restoring {
        Restoring::Ran(result) => restore_result(result),
        Restoring::AlreadyRestored => "already restored".to_owned(),
        Restoring::NotTrashed => "this run didn't move it to the trash".to_owned(),
        Restoring::NoSuchItem => "this run has no such item".to_owned(),
        Restoring::Cancelled => "cancelled before it started".to_owned(),
    }
}

/// Why an item of a plan file isn't in the fresh plan.
pub(crate) fn dropped(dropped: &Dropped) -> String {
    match dropped {
        Dropped::NotFound => "it isn't found there any more".to_owned(),
        Dropped::RootChanged => "the folder it is in changed".to_owned(),
        Dropped::MethodWidened { file, now } => format!(
            "the file says {}, and your config says {}",
            method_name(*file),
            method_name(*now)
        ),
        Dropped::MethodsDiffer => "the file names its rule with two methods".to_owned(),
        Dropped::Refused(why) => skip(why),
        Dropped::NothingInCommon => "nothing in it matches the file any more".to_owned(),
        Dropped::Duplicate => "the file names it twice".to_owned(),
    }
}

/// Where a stored run stands.
pub(crate) const fn run_state(state: RunState) -> &'static str {
    match state {
        RunState::Complete => "complete",
        RunState::Running => "running",
        RunState::Interrupted => "interrupted",
        RunState::NeverStarted => "never started",
        RunState::Unreadable => "unreadable",
    }
}

/// Why a stored run can't be read.
pub(crate) const fn unreadable(why: &Unreadable) -> &'static str {
    match why {
        Unreadable::Plan(_) => "its plan is damaged",
        Unreadable::Journal(_) => "its record is damaged",
        Unreadable::Order => "its record is out of order",
        Unreadable::Store(_) => "its files can't be read",
    }
}

/// Why the run store refused.
pub(crate) fn store_refusal(refusal: &StoreRefusal) -> String {
    match refusal {
        StoreRefusal::Busy => "another kipple apply is running".to_owned(),
        StoreRefusal::Lock(_) => "the run store couldn't be locked".to_owned(),
        StoreRefusal::NoDataDir => "kipple's data folder isn't known".to_owned(),
        StoreRefusal::FileSystem(why) => format!("kipple's data folder is on {}", unsupported(why)),
        StoreRefusal::Space { needed, .. } => {
            format!("free {} for the plan record", super::bytes(*needed))
        }
        StoreRefusal::Write(error) => format!("the run couldn't be stored: {}", error_text(error)),
        StoreRefusal::Unsupported => "this build can't store runs yet".to_owned(),
    }
}

/// How a command ended.
pub(crate) fn ending(ending: &Ending) -> String {
    match ending {
        Ending::Done => "done".to_owned(),
        Ending::NothingToClean => "nothing to clean".to_owned(),
        Ending::NoneRan => "nothing ran".to_owned(),
        Ending::Declined => "nothing changed".to_owned(),
        Ending::Stale => "the plan is stale, make a new one".to_owned(),
        Ending::Failed => "something didn't finish".to_owned(),
        Ending::Refused(why) => store_refusal(why),
        Ending::Cancelled => "cancelled".to_owned(),
    }
}

fn list<'a>(words: impl Iterator<Item = &'a str>) -> String {
    words.collect::<Vec<_>>().join(", ")
}
