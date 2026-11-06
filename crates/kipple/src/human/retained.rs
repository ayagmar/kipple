//! The words for what retention decided about a release.

use kipple_core::{GroupGap, Retained, Settlement};

/// Why a store's group of releases can't be settled.
const fn gap(gap: GroupGap) -> &'static str {
    match gap {
        GroupGap::Selection => "its selected release can't be read",
        GroupGap::HistoryMissing => "it has no selection history",
        GroupGap::HistoryMalformed => "its selection history can't be parsed",
        GroupGap::HistoryStale => "its selection history is out of date",
        GroupGap::InstallerBusy => "an install is in progress",
        GroupGap::ActivityUnknown => "a program may be using one of its releases",
        GroupGap::RetiredRunning => "a program runs a release set aside for removal",
    }
}

/// What retention decided, for a detail view.
pub(crate) fn settlement(settlement: Settlement) -> String {
    match settlement {
        Settlement::Unsettled => "not settled yet, so it is kept".to_owned(),
        Settlement::Candidate => "no longer needed by its store".to_owned(),
        Settlement::Retained(Retained::Active) => "kept: the selected release".to_owned(),
        Settlement::Retained(Retained::Previous) => "kept: the rollback".to_owned(),
        Settlement::Retained(Retained::NewestLeft) => "kept: the newest left".to_owned(),
        Settlement::Retained(Retained::Running) => "kept: a program runs it".to_owned(),
        Settlement::Retained(Retained::Pending) => "kept: an install needs it".to_owned(),
        Settlement::Retained(Retained::GroupBlocked(why)) => format!("kept: {}", gap(why)),
    }
}
