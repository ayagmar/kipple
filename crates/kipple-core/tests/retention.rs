//! How release versions order: `settle` keeps the newest left when a store names no
//! rollback, so a wrong order removes the only way back.

fn version(text: &str) -> ReleaseVersion {
    ReleaseVersion(text.to_owned())
}

fn newest<'text>(versions: &[&'text str]) -> Option<&'text str> {
    versions.iter().copied().max_by_key(|text| version(text))
}

/// Catches a prerelease outranking its release, which would keep the alpha and let the
/// release go.
#[test]
fn a_prerelease_orders_below_its_release() {
    assert_eq!(newest(&["0.166.0", "0.166.0-alpha.2"]), Some("0.166.0"));
    assert_eq!(
        newest(&["0.165.0", "0.166.0-alpha.2"]),
        Some("0.166.0-alpha.2")
    );
}

/// Catches comparing versions as text, where 0.9.0 outranks 0.166.0.
#[test]
fn versions_order_by_number_not_by_text() {
    assert_eq!(newest(&["0.9.0", "0.166.0"]), Some("0.166.0"));
    assert_eq!(
        newest(&["1.0.0-alpha.9", "1.0.0-alpha.10"]),
        Some("1.0.0-alpha.10")
    );
    assert_eq!(
        newest(&["1.0.0-alpha", "1.0.0-alpha.1"]),
        Some("1.0.0-alpha.1")
    );
    assert_eq!(newest(&["1.0.0-1", "1.0.0-rc"]), Some("1.0.0-rc"));
}

/// Catches a name that isn't a version beating a real one.
#[test]
fn text_that_is_no_version_orders_below_every_version() {
    assert_eq!(newest(&["zzz", "0.0.1"]), Some("0.0.1"));
    assert!(
        version("local-build") != version("local-test"),
        "two names stay distinct when neither is a version"
    );
}

use std::path::PathBuf;

use kipple_core::{
    Activity, ActivityGap, Completeness, GroupGap, ReleaseFacts, ReleaseGroup, ReleaseRole,
    ReleaseState, ReleaseVersion, ReleaseView, Retained, Seen, Settlement, settle,
};

const TARGET: &str = "x86_64-unknown-linux-musl";

/// One release as a test sets it up.
#[derive(Debug, Clone)]
struct Member {
    facts: ReleaseFacts,
    measured: Option<Completeness>,
    activity: Activity,
}

fn member(component: &str, target: &str, version: &str, role: ReleaseRole) -> Member {
    Member {
        facts: ReleaseFacts {
            group: ReleaseGroup {
                component: component.to_owned(),
                target: target.to_owned(),
            },
            version: ReleaseVersion(version.to_owned()),
            role,
            files: Vec::new(),
            locks: Vec::new(),
            state: ReleaseState::Sound,
        },
        measured: Some(Completeness::Complete),
        activity: Activity::Idle,
    }
}

fn standalone(version: &str, role: ReleaseRole) -> Member {
    member("standalone", TARGET, version, role)
}

fn running() -> Activity {
    Activity::InUse {
        pid: 42,
        seen: Seen::Executable(PathBuf::from("bin/codex")),
    }
}

fn settled(members: &[Member]) -> Vec<Settlement> {
    let views: Vec<ReleaseView<'_>> = members
        .iter()
        .map(|member| ReleaseView {
            facts: &member.facts,
            measured: member.measured,
            activity: &member.activity,
        })
        .collect();
    settle(&views)
}

const fn kept(why: Retained) -> Settlement {
    Settlement::Retained(why)
}

/// Catches a group decided while one of its releases is unmeasured: that release could be
/// the newest left, so no other in its group may go, while another group still settles.
#[test]
fn unmeasured_member_leaves_its_group_unsettled() {
    for measured in [None, Some(Completeness::Cancelled)] {
        let mut members = vec![
            standalone("0.166.0", ReleaseRole::Active),
            standalone("0.164.0", ReleaseRole::Installed),
            standalone("0.165.0", ReleaseRole::Installed),
            member("daemon", TARGET, "0.164.0", ReleaseRole::Installed),
            member("daemon", TARGET, "0.165.0", ReleaseRole::Previous),
        ];
        if let Some(cut) = members.get_mut(2) {
            cut.measured = measured;
        }

        let settled = settled(&members);

        assert_eq!(
            settled,
            [
                Settlement::Unsettled,
                Settlement::Unsettled,
                Settlement::Unsettled,
                Settlement::Candidate,
                kept(Retained::Previous),
            ],
            "{measured:?}"
        );
    }
}

/// Catches the selected release going when what its component records can't be relied
/// on: the record that names it is the one in doubt.
#[test]
fn a_blocked_component_keeps_its_active_release() {
    let mut members = vec![
        standalone("0.166.0", ReleaseRole::Active),
        standalone("0.165.0", ReleaseRole::Installed),
        member(
            "standalone",
            "aarch64-unknown-linux-musl",
            "0.164.0",
            ReleaseRole::Installed,
        ),
    ];
    for member in &mut members {
        member.facts.state = ReleaseState::Blocked(GroupGap::HistoryStale);
    }

    let blocked = kept(Retained::GroupBlocked(GroupGap::HistoryStale));
    assert_eq!(settled(&members), [blocked; 3]);
}

/// Catches a release set aside answering for its target alone, which it may not name:
/// a process under it keeps every release of its component, and only that component.
#[test]
fn a_process_under_a_retired_release_keeps_its_component() {
    let mut retired = member("standalone", "", "", ReleaseRole::Retired);
    retired.activity = running();
    let members = vec![
        standalone("0.166.0", ReleaseRole::Active),
        standalone("0.164.0", ReleaseRole::Installed),
        retired,
        member("daemon", TARGET, "0.165.0", ReleaseRole::Active),
        member("daemon", TARGET, "0.164.0", ReleaseRole::Installed),
    ];

    let blocked = kept(Retained::GroupBlocked(GroupGap::RetiredRunning));
    assert_eq!(
        settled(&members),
        [
            blocked,
            blocked,
            blocked,
            kept(Retained::Active),
            kept(Retained::NewestLeft),
        ]
    );
}

/// A deterministic generator, so a failing case can be replayed from its seed.
struct SplitMix(u64);

impl SplitMix {
    const fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn pick<T: Copy, const N: usize>(&mut self, from: &[T; N]) -> T {
        let at = usize::try_from(self.next()).unwrap_or(0) % N;
        from.get(at).copied().unwrap_or(from[0])
    }

    const fn one_in(&mut self, odds: u64) -> bool {
        self.next().is_multiple_of(odds)
    }
}

const COMPONENTS: [&str; 2] = ["standalone", "app-server-daemon"];
const TARGETS: [&str; 2] = [TARGET, "aarch64-unknown-linux-musl"];
const VERSIONS: [&str; 8] = [
    "0.162.1",
    "0.163.0",
    "0.164.0",
    "0.165.0",
    "0.166.0-alpha.5.1",
    "0.166.0",
    "0.167.0",
    "local-build",
];

fn activity(rng: &mut SplitMix) -> Activity {
    match rng.next() % 8 {
        0 => running(),
        1 => Activity::Unknown(ActivityGap::Uninspected(1)),
        2 => Activity::Unknown(ActivityGap::Partial("hidepid".to_owned())),
        3 => Activity::IdleExceptSystem {
            unchecked: 3,
            by_command_line: 0,
        },
        _ => Activity::Idle,
    }
}

const fn measured(rng: &mut SplitMix) -> Option<Completeness> {
    match rng.next() % 12 {
        0 => None,
        1 => Some(Completeness::Cancelled),
        2 => Some(Completeness::Incomplete {
            unreadable: 1,
            skipped_mounts: 0,
        }),
        _ => Some(Completeness::Complete),
    }
}

/// A store as a scan could find it: each component with its own records, an active and a
/// rollback at most once each, and anything a probe can fail at failing now and then.
fn generated(rng: &mut SplitMix) -> Vec<Member> {
    let mut members = Vec::new();
    for component in COMPONENTS {
        let state = if rng.one_in(6) {
            ReleaseState::Blocked(rng.pick(&[
                GroupGap::HistoryStale,
                GroupGap::InstallerBusy,
                GroupGap::Unreadable,
            ]))
        } else {
            ReleaseState::Sound
        };
        let mut roles = vec![ReleaseRole::Active, ReleaseRole::Previous];
        let count = rng.next() % 7;
        for _ in 0..count {
            let target = rng.pick(&TARGETS);
            let version = rng.pick(&VERSIONS);
            let role = match roles.pop().filter(|_| rng.one_in(2)) {
                Some(role) => role,
                None if rng.one_in(10) => ReleaseRole::PendingInstall,
                None if rng.one_in(10) => ReleaseRole::Retired,
                None => ReleaseRole::Installed,
            };
            let mut generated = member(component, target, version, role);
            generated.facts.state = state;
            generated.activity = activity(rng);
            generated.measured = measured(rng);
            members.push(generated);
        }
    }
    members
}

/// What upstream's prune keeps (S5 codex §4), written from that report: it skips a whole
/// store when its records can't be relied on, a process can't be inspected, an install is
/// under way or something runs from a release set aside, and otherwise keeps the selected
/// release, the rollback and whatever runs.
fn upstream_keeps(members: &[Member]) -> Vec<bool> {
    let skips = |component: &str| {
        members
            .iter()
            .filter(|member| member.facts.group.component == component)
            .any(|member| {
                member.facts.state != ReleaseState::Sound
                    || matches!(member.activity, Activity::Unknown(_))
                    || member.facts.role == ReleaseRole::PendingInstall
                    || (member.facts.role == ReleaseRole::Retired
                        && matches!(member.activity, Activity::InUse { .. }))
            })
    };
    members
        .iter()
        .map(|member| {
            skips(&member.facts.group.component)
                || matches!(
                    member.facts.role,
                    ReleaseRole::Active | ReleaseRole::Previous
                )
                || matches!(member.activity, Activity::InUse { .. })
        })
        .collect()
}

/// Catches retention keeping less than upstream's own prune, letting an active, rollback,
/// running or pending release go, deciding a group with an unmeasured member, or deciding
/// differently when the same store is read again in another order, as a restart reads it.
#[test]
fn settle_never_keeps_less_than_upstream() {
    let mut rng = SplitMix(0x7265_6c65_6173_6573);
    for case in 0..4000 {
        let members = generated(&mut rng);

        let decided = settled(&members);

        let upstream = upstream_keeps(&members);
        for ((member, settlement), keeps) in members.iter().zip(&decided).zip(&upstream) {
            let candidate = *settlement == Settlement::Candidate;
            let context = || format!("case {case}: {member:?} in {members:#?}");
            assert!(!(candidate && *keeps), "upstream keeps it: {}", context());
            let protected = matches!(
                member.facts.role,
                ReleaseRole::Active | ReleaseRole::Previous | ReleaseRole::PendingInstall
            ) || matches!(member.activity, Activity::InUse { .. });
            assert!(!(candidate && protected), "a kept role went: {}", context());
            let unmeasured_peer = members.iter().any(|other| {
                other.facts.group == member.facts.group
                    && other.facts.role != ReleaseRole::Retired
                    && !matches!(
                        other.measured,
                        Some(Completeness::Complete | Completeness::Incomplete { .. })
                    )
            });
            assert!(
                !(candidate && unmeasured_peer),
                "decided beside an unmeasured release: {}",
                context()
            );
            assert!(
                !(candidate && member.facts.role == ReleaseRole::Retired),
                "a retired release is never a candidate: {}",
                context()
            );
        }

        let mut again = members.clone();
        again.reverse();
        let mut reread = settled(&again);
        reread.reverse();
        assert_eq!(reread, decided, "case {case}: read again in another order");
    }
}

/// Catches the generator drifting into stores where nothing is ever a candidate, which
/// would let the property above pass on nothing.
#[test]
fn the_generated_stores_reach_every_outcome() {
    let mut rng = SplitMix(0x7265_6c65_6173_6573);
    let mut seen = Vec::new();
    for _ in 0..4000 {
        seen.extend(settled(&generated(&mut rng)));
    }
    for outcome in [
        Settlement::Candidate,
        Settlement::Unsettled,
        kept(Retained::Active),
        kept(Retained::Previous),
        kept(Retained::NewestLeft),
        kept(Retained::Running),
        kept(Retained::Pending),
        kept(Retained::GroupBlocked(GroupGap::ActivityUnknown)),
        kept(Retained::GroupBlocked(GroupGap::UnorderedVersion)),
        kept(Retained::GroupBlocked(GroupGap::RetiredRunning)),
    ] {
        assert!(seen.contains(&outcome), "expected {outcome:?} generated");
    }
}
