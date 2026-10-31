//! Profiles from freezing to a run (03 J6, D-068, spec M3c pinned 3, 10, 12; the profile
//! regressions of `docs/11-delivery-plan.md` M3): each change since a profile was made
//! suspends what it touches or skips the item, and nothing a profile runs was outside
//! what it froze.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, UNIX_EPOCH};

use kipple_core::format::{IntegrationDigests, encode_profile};
use kipple_core::profile::{
    self, Profile, ProfileChange, ProfileCheck, ProfileError, ProfileName, ProfileScene,
    ProfileStore, ProfileStoreError,
};
use kipple_core::{
    ActivityEvidence, ApplyPorts, Authorization, Bucket, CancelToken, CleanRequest, Confirmed,
    Consent, Ending, FindingSink, HeldPath, Integration, IntegrationDescriptor, IntegrationError,
    IntegrationId, Lens, Level, Lineage, Method, Modified, OpenedRoot, PlanMode, Platform,
    Prepared, ResolvedRoot, RootSource, Roots, RuleId, RuleSettings, ScanContext, ScanReport,
    Selector, Session, Skip, Status, SymbolicRoot, WalkGap, prepare_clean, run_clean, run_prepared,
};

use super::codec::TestCodec;
use super::world::Target;
use super::{
    CARGO, Config, HOME, HOME_ID, Item, OTHER, SplitMix, apply_as, home, id, plan_all, ports,
    project, rescanned, rule, rule_id, scan_of, world_of,
};

const CARGO_RULE: &str = "builtin.cargo.target";
const OTHER_RULE: &str = "builtin.other.build";

/// Where profiles are kept, in memory.
#[derive(Debug, Default)]
struct Shelf(Mutex<BTreeMap<String, Vec<u8>>>);

impl Shelf {
    fn files(&self) -> MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        self.0.lock().unwrap()
    }
}

impl ProfileStore for Shelf {
    fn read(&self, name: &ProfileName) -> Result<Option<Vec<u8>>, ProfileStoreError> {
        Ok(self.files().get(name.as_str()).cloned())
    }

    fn create_new(&self, name: &ProfileName, bytes: &[u8]) -> Result<(), ProfileStoreError> {
        let mut files = self.files();
        if files.contains_key(name.as_str()) {
            return Err(ProfileStoreError::Exists);
        }
        files.insert(name.as_str().to_owned(), bytes.to_vec());
        Ok(())
    }

    fn replace(&self, name: &ProfileName, bytes: &[u8]) -> Result<(), ProfileStoreError> {
        let mut files = self.files();
        let file = files
            .get_mut(name.as_str())
            .ok_or(ProfileStoreError::Missing)?;
        bytes.clone_into(file);
        Ok(())
    }

    fn remove(&self, name: &ProfileName) -> Result<(), ProfileStoreError> {
        self.files()
            .remove(name.as_str())
            .map(drop)
            .ok_or(ProfileStoreError::Missing)
    }

    fn list(&self) -> Result<Vec<ProfileName>, ProfileStoreError> {
        Ok(self
            .files()
            .keys()
            .filter_map(|name| ProfileName::parse(name))
            .collect())
    }
}

/// An integration that declares itself and finds nothing: what its digest is made of.
#[derive(Debug)]
struct Declared {
    id: IntegrationId,
    revision: u32,
}

impl Integration for Declared {
    fn descriptor(&self) -> IntegrationDescriptor {
        IntegrationDescriptor {
            id: self.id,
            lens: Lens::Dev,
            platforms: Platform::ALL,
            roots: &[SymbolicRoot::ProjectRoots],
            selectors: &[],
            artifacts: &[],
            owner_files: &[],
            verified: &[],
            consequences: &[],
            revision: self.revision,
        }
    }

    fn activity_evidence(&self) -> ActivityEvidence {
        ActivityEvidence::BuiltExecutables
    }

    fn discover(
        &self,
        _: &ScanContext<'_>,
        _: &mut FindingSink<'_>,
    ) -> Result<(), IntegrationError> {
        Ok(())
    }
}

fn digests(cargo_revision: u32) -> IntegrationDigests {
    let integrations: Vec<Box<dyn Integration>> = vec![
        Box::new(Declared {
            id: CARGO,
            revision: cargo_revision,
        }),
        Box::new(Declared {
            id: OTHER,
            revision: 1,
        }),
    ];
    IntegrationDigests::of(&integrations)
}

/// A build directory of the other integration's rule at `/home/u/p<n>/out`.
fn other(n: u128, base: u128) -> (Item, Target) {
    let (mut item, target) = project(n, base, &["o"]);
    item.path = home(&format!("p{n}/out"));
    item.rule = rule_id(OTHER_RULE);
    item.integration = OTHER;
    (item, target)
}

fn name(text: &str) -> ProfileName {
    ProfileName::parse(text).unwrap()
}

/// One machine a profile is made and run on: the config, the disk, a scan of it, the
/// roots it resolved and each integration's digest.
#[derive(Debug, Clone)]
struct Desk {
    config: Config,
    targets: Vec<(Item, Target)>,
    scan: ScanReport,
    roots: Roots,
    digests: IntegrationDigests,
}

/// Two cargo projects and one build directory of the other rule, all Safe at Balanced,
/// deleted.
fn desk() -> Desk {
    let targets = vec![
        project(0, 100, &["a", "debug/", "debug/b"]),
        project(1, 200, &["c"]),
        other(2, 300),
    ];
    let items: Vec<Item> = targets.iter().map(|(one, _)| one.clone()).collect();
    let mut roots = Roots::default();
    roots.insert(
        SymbolicRoot::ProjectRoots,
        Ok(ResolvedRoot::new(
            SymbolicRoot::ProjectRoots,
            PathBuf::from(HOME),
            RootSource::CommandLine,
        )),
    );
    Desk {
        config: Config::new(),
        scan: scan_of(&items),
        targets,
        roots,
        digests: digests(1),
    }
}

impl Desk {
    fn scene(&self) -> ProfileScene<'_> {
        ProfileScene {
            judge: self.config.judge(),
            scan: &self.scan,
            roots: &self.roots,
            integrations: &self.digests,
        }
    }

    /// The profile `nightly` of the rules `only` names, or of every rule, made now,
    /// stored and read back; `None` when it can't be made.
    fn try_make(&self, only: Option<&[&str]>) -> Option<Profile> {
        let only: Option<BTreeSet<RuleId>> =
            only.map(|only| only.iter().map(|rule| rule_id(rule)).collect());
        let created = UNIX_EPOCH + Duration::from_secs(1_793_000_000);
        let draft = profile::freeze(name("nightly"), &self.scene(), created, only.as_ref());
        draft
            .ok()
            .map(|draft| profile::create(&Shelf::default(), &draft, &TestCodec).unwrap())
    }

    /// [`Desk::try_make`], made.
    fn make(&self, only: Option<&[&str]>) -> Profile {
        self.try_make(only).unwrap()
    }

    fn check(&self, profile: &Profile) -> ProfileCheck {
        profile::check(profile, &self.scene())
    }

    /// The finding at `path` in this desk's scan.
    fn rule_at(&self, path: &std::path::Path) -> RuleId {
        let finding = self.scan.findings.iter().find(|f| f.path == path).unwrap();
        finding.rule.clone()
    }

    /// The findings Safe now of `rules`, by path.
    fn safe_of(&self, rules: &[RuleId]) -> BTreeSet<PathBuf> {
        self.scan
            .findings
            .iter()
            .filter(|finding| rules.contains(&finding.rule))
            .filter(|finding| {
                finding.verdict(&self.config.policy).map(|v| v.bucket()) == Some(Bucket::Safe)
            })
            .map(|finding| finding.path.clone())
            .collect()
    }

    /// Prepares the clean `check` selects, on a fresh copy of the disk, applying with
    /// `rescan` as what the disk holds at apply.
    fn prepare(&self, check: &ProfileCheck) -> Prepared {
        let world = world_of(&self.targets);
        let mutation = ports(&world);
        let rescan = rescanned(&self.scan);
        let session = Session {
            judge: self.config.judge(),
            store: &world,
            ports: ApplyPorts {
                rescan: &rescan,
                mutation: &mutation,
            },
            codec: &TestCodec,
        };
        let request = CleanRequest {
            scan: &self.scan,
            selection: check.selection(),
        };
        prepare_clean(&request, &session, PlanMode::Safe, &CancelToken::new())
    }

    /// Runs `prepared` as `check`'s grant allows, on a fresh copy of the disk, with the
    /// disk scanned again as `rescan` at apply: the paths of the items done.
    fn run(&self, prepared: &Prepared, check: &ProfileCheck, rescan: &ScanReport) -> Ran {
        let world = world_of(&self.targets);
        let mutation = ports(&world);
        let rescan = rescanned(rescan);
        let session = Session {
            judge: self.config.judge(),
            store: &world,
            ports: ApplyPorts {
                rescan: &rescan,
                mutation: &mutation,
            },
            codec: &TestCodec,
        };
        let grant = check.grant().unwrap().clone();
        let finished = run_prepared(
            prepared,
            Confirmed::Profile(grant),
            &session,
            &mut (),
            &CancelToken::new(),
        );
        let mut ran = Ran::default();
        for item in finished.receipt.items {
            if item.outcome.status() == Status::Done {
                ran.done.insert(item.path);
            } else {
                ran.skipped
                    .insert(item.path.clone(), skip_of(&item.outcome));
            }
        }
        ran
    }

    /// Checks `profile` and runs what it selects against the disk as scanned: `None`
    /// when the check grants nothing.
    fn run_now(&self, profile: &Profile) -> Option<Ran> {
        let check = self.check(profile);
        check.grant()?;
        Some(self.run(&self.prepare(&check), &check, &self.scan))
    }

    fn settings(&mut self, rule: &str) -> &mut RuleSettings {
        self.config.policy.rules.entry(rule_id(rule)).or_default()
    }
}

/// A kept or excluded path at `/home/u/<path>`.
fn held(path: &str) -> HeldPath {
    HeldPath {
        forms: vec![home(path)],
        lineage: Lineage(vec![Some(id(999)), Some(HOME_ID)]),
        note: None,
    }
}

fn skip_of(outcome: &kipple_core::ItemOutcome) -> Option<Skip> {
    if let kipple_core::ItemOutcome::Skipped(skip) = outcome {
        Some(skip.clone())
    } else {
        None
    }
}

/// What a profile run did.
#[derive(Debug, Default)]
struct Ran {
    done: BTreeSet<PathBuf>,
    skipped: BTreeMap<PathBuf, Option<Skip>>,
}

/// The profile made on `desk()` of every rule, then the change `change` made: `rule` is
/// suspended for exactly `expected`, and the run that follows removes only what the
/// profile froze of its other, unchanged rules.
fn suspends(rule: &str, expected: &[ProfileChange], change: impl FnOnce(&mut Desk)) {
    let mut desk = desk();
    let profile = desk.make(None);
    let before = desk.safe_of(&[rule_id(CARGO_RULE), rule_id(OTHER_RULE)]);
    change(&mut desk);
    let check = desk.check(&profile);
    let suspended = check
        .suspended()
        .iter()
        .find(|one| one.rule == rule_id(rule));
    assert!(suspended.is_some(), "{rule} is suspended");
    let suspended = suspended.unwrap();
    assert_eq!(suspended.changes, expected);
    assert!(
        !check.active().contains(&rule_id(rule)),
        "{rule} isn't active"
    );
    let Some(ran) = desk.run_now(&profile) else {
        return;
    };
    assert!(
        ran.done.is_subset(&before),
        "nothing beyond what was frozen ran"
    );
    for path in &ran.done {
        assert_ne!(
            desk.rule_at(path),
            rule_id(rule),
            "{} was suspended",
            path.display()
        );
    }
}

/// Catches a profile made at Careful running at Balanced, which would take what
/// Balanced adds to Safe without anyone confirming it (docs/11 M3).
#[test]
fn careful_to_balanced_suspends_every_entry() {
    let mut desk = desk();
    desk.config.policy.level = Level::Careful;
    let profile = desk.make(None);
    desk.config.policy.level = Level::Balanced;
    let check = desk.check(&profile);
    assert_eq!(check.active(), &[] as &[RuleId]);
    for one in check.suspended() {
        assert_eq!(
            one.changes,
            [ProfileChange::Level, ProfileChange::RecentWindow]
        );
    }
    assert_eq!(check.grant(), None);
    assert!(check.selection().0.is_empty(), "nothing is selected");
}

/// Catches Balanced to Thorough widening a profile (docs/11 M3).
#[test]
fn balanced_to_thorough_suspends_every_entry() {
    suspends(
        CARGO_RULE,
        &[ProfileChange::Level, ProfileChange::RecentWindow],
        |desk| {
            desk.config.policy.level = Level::Thorough;
        },
    );
}

/// Catches one rule's own level changing without its entry noticing (docs/11 M3).
#[test]
fn a_rules_own_level_suspends_that_rule_only() {
    let mut desk = desk();
    let profile = desk.make(None);
    desk.settings(CARGO_RULE).level = Some(Level::Thorough);
    let check = desk.check(&profile);
    assert_eq!(check.active(), [rule_id(OTHER_RULE)]);
    let ran = desk.run_now(&profile).unwrap();
    assert_eq!(ran.done, BTreeSet::from([home("p2/out")]));
}

/// Catches a rule's recent window changing without its entry noticing (docs/11 M3).
#[test]
fn a_rules_recent_window_suspends_it() {
    suspends(CARGO_RULE, &[ProfileChange::RecentWindow], |desk| {
        desk.settings(CARGO_RULE).recent_days = Some(365);
    });
}

/// Catches a rule turned off when the profile was made running once it is turned on
/// (docs/11 M3: re-enabling a rule).
#[test]
fn a_rule_turned_back_on_is_suspended() {
    let mut desk = desk();
    desk.settings(CARGO_RULE).disabled = true;
    let profile = desk.make(None);
    desk.settings(CARGO_RULE).disabled = false;
    let check = desk.check(&profile);
    let suspended: Vec<_> = check.suspended().iter().map(|one| &one.rule).collect();
    assert_eq!(suspended, [&rule_id(CARGO_RULE)]);
    assert_eq!(check.suspended()[0].changes, [ProfileChange::Enabled]);
    let ran = desk.run_now(&profile).unwrap();
    assert_eq!(ran.done, BTreeSet::from([home("p2/out")]));
}

/// Catches an upgrade that keeps a rule's ID and widens what it selects (D-021).
#[test]
fn the_same_rule_id_selecting_more_is_suspended() {
    suspends(CARGO_RULE, &[ProfileChange::Rule], |desk| {
        desk.config.rules[0].selector = Selector::Integration("cargo.target-and-deps");
    });
}

/// Catches a root replaced by another directory at the same path (docs/11 M3).
#[test]
fn a_replaced_root_is_suspended() {
    suspends(CARGO_RULE, &[ProfileChange::Roots], |desk| {
        let cargo = desk
            .scan
            .integrations
            .iter_mut()
            .find(|i| i.id == CARGO)
            .unwrap();
        cargo.opened[0].identity = Some(id(77));
    });
}

/// Catches a changed method running as the new one (docs/11 M3).
#[test]
fn a_changed_method_is_suspended() {
    suspends(CARGO_RULE, &[ProfileChange::Method], |desk| {
        desk.config
            .methods
            .rules
            .insert(rule_id(CARGO_RULE), Method::Trash);
    });
}

/// Catches a kept path the profile was made with being removed from the config, which
/// would let the profile take what it held (docs/11 M3: a removed grant).
#[test]
fn a_kept_path_removed_suspends_every_entry() {
    let mut desk = desk();
    desk.config.policy.keep = vec![held("p9")];
    let profile = desk.make(None);
    desk.config.policy.keep.clear();
    let check = desk.check(&profile);
    assert_eq!(check.active(), &[] as &[RuleId]);
    for one in check.suspended() {
        assert_eq!(one.changes, [ProfileChange::Held]);
    }
}

/// Catches a kept or excluded path added since being taken for a change: it only holds
/// more, so nothing is suspended (founder question 2).
#[test]
fn a_held_path_added_suspends_nothing() {
    let mut desk = desk();
    let profile = desk.make(None);
    desk.config.policy.exclude = vec![held("p0")];
    let check = desk.check(&profile);
    assert_eq!(check.suspended(), &[]);
    let ran = desk.run_now(&profile).unwrap();
    assert!(
        !ran.done.contains(&home("p0/target")),
        "what is excluded now stays"
    );
}

/// Catches a rule new since the profile was made running under it (docs/11 M3).
#[test]
fn a_new_rule_is_pending_and_never_runs() {
    let mut desk = desk();
    let profile = desk.make(None);
    desk.config.rules.push(rule("builtin.cargo.new", CARGO));
    let (mut item, target) = project(3, 400, &["n"]);
    item.rule = rule_id("builtin.cargo.new");
    desk.targets.push((item.clone(), target));
    let items: Vec<Item> = desk.targets.iter().map(|(one, _)| one.clone()).collect();
    desk.scan = scan_of(&items);
    let check = desk.check(&profile);
    assert_eq!(check.pending(), [rule_id("builtin.cargo.new")]);
    assert!(
        !check.active().contains(&rule_id("builtin.cargo.new")),
        "a rule new since create is never active"
    );
    let ran = desk.run_now(&profile).unwrap();
    assert!(
        !ran.done.contains(&item.path),
        "a pending rule's item stays"
    );
}

/// Catches an item that was Safe when the profile was made and is Review or unknown now
/// being removed (docs/11 M3): not selected when it is so at the check, and skipped when
/// it turns so between the check and its item.
#[test]
fn an_item_once_safe_and_now_review_or_unknown_is_left() {
    for (modified, skip) in [
        (
            Modified::Ago(Duration::from_hours(1)),
            Skip::Riskier(vec![kipple_core::Caution::Recent]),
        ),
        (
            Modified::Unknown(WalkGap::Unread),
            Skip::Protected(vec![kipple_core::Hold::UnknownModified]),
        ),
    ] {
        let mut desk = desk();
        let profile = desk.make(None);
        let mut changed = desk.scan.clone();
        changed.findings[0].facts.as_mut().unwrap().modified = modified;

        let check = desk.check(&profile);
        let prepared = desk.prepare(&check);
        let ran = desk.run(&prepared, &check, &changed);
        assert_eq!(
            ran.skipped.get(&home("p0/target")),
            Some(&Some(skip.clone()))
        );

        desk.scan = changed;
        let checked_now = desk.run_now(&profile).unwrap();
        assert!(
            !checked_now.done.contains(&home("p0/target")),
            "{skip:?} isn't selected"
        );
    }
}

/// Catches an integration whose code changed what it finds keeping the profile's entries
/// active (pinned 10: its revision is bumped).
#[test]
fn an_integration_revised_suspends_its_rules() {
    suspends(CARGO_RULE, &[ProfileChange::Integration], |desk| {
        desk.digests = digests(2);
    });
}

/// Catches a trash move under `--yes` without a profile, and a profile that can't move
/// what it froze to the trash (pinned 3): Safe-only skips the trash item as a whole move
/// nobody reviewed; the profile, which recorded trash, runs it.
#[test]
fn a_profile_moves_to_the_trash_where_safe_only_does_not() {
    let mut desk = desk();
    desk.config.methods.config = Some(Method::Trash);
    let profile = desk.make(Some(&[CARGO_RULE]));
    let ran = desk.run_now(&profile).unwrap();
    assert_eq!(
        ran.done,
        BTreeSet::from([home("p0/target"), home("p1/target")])
    );

    let world = world_of(&desk.targets);
    let planned = plan_all(&desk.scan, &desk.config, &world, PlanMode::Safe);
    let receipt = apply_as(
        &planned.plan,
        Authorization::SafeOnly,
        &rescanned(&desk.scan),
        &desk.config,
        &world,
    );
    for item in &receipt.items {
        assert_eq!(skip_of(&item.outcome), Some(Skip::WholeNotReviewed));
    }
}

/// Catches a grant that authorizes what its check didn't select: a plan holding a
/// left-out or suspended rule's Safe item, an item below a root the entry didn't freeze,
/// or one removed by another method is refused item by item as not reviewed (pinned 3).
#[test]
fn a_grant_admits_only_its_rules_roots_and_method() {
    let desk = desk();
    let profile = desk.make(Some(&[CARGO_RULE]));
    let check = desk.check(&profile);
    let grant = check.grant().unwrap().clone();
    assert_eq!(check.left_out(), [rule_id(OTHER_RULE)]);

    // A second project root the profile never froze, holding a cargo project.
    let mut wider = desk.clone();
    let (mut item, target) = project(5, 500, &["w"]);
    item.path = PathBuf::from(HOME).with_file_name("work").join("p5/target");
    wider.targets.push((item.clone(), target));
    let items: Vec<Item> = wider.targets.iter().map(|(one, _)| one.clone()).collect();
    wider.scan = scan_of(&items);
    for report in &mut wider.scan.integrations {
        report.opened.push(OpenedRoot {
            root: SymbolicRoot::ProjectRoots,
            path: item.path.parent().unwrap().parent().unwrap().to_owned(),
            identity: Some(id(498)),
        });
    }
    let world = world_of(&wider.targets);
    let planned = plan_all(&wider.scan, &wider.config, &world, PlanMode::Safe);
    let receipt = apply_as(
        &planned.plan,
        Authorization::Profile(grant.clone()),
        &rescanned(&wider.scan),
        &wider.config,
        &world,
    );
    let skipped: BTreeMap<PathBuf, Option<Skip>> = receipt
        .items
        .iter()
        .map(|done| (done.path.clone(), skip_of(&done.outcome)))
        .collect();
    assert_eq!(skipped.get(&home("p0/target")), Some(&None), "granted");
    assert_eq!(
        skipped.get(&home("p2/out")),
        Some(&Some(Skip::NotReviewed)),
        "left out"
    );
    assert_eq!(
        skipped.get(&item.path),
        Some(&Some(Skip::NotReviewed)),
        "another root"
    );

    // The same plan made after the rule's method changed, and after it was suspended.
    for change in [
        |later: &mut Desk| {
            later
                .config
                .methods
                .rules
                .insert(rule_id(CARGO_RULE), Method::Trash);
        },
        |later: &mut Desk| later.settings(CARGO_RULE).level = Some(Level::Thorough),
    ] {
        let mut later = desk.clone();
        change(&mut later);
        let world_later = world_of(&later.targets);
        let made = plan_all(&later.scan, &later.config, &world_later, PlanMode::Safe);
        let receipt_later = apply_as(
            &made.plan,
            Authorization::Profile(grant.clone()),
            &rescanned(&later.scan),
            &later.config,
            &world_later,
        );
        assert!(!receipt_later.items.is_empty(), "something was planned");
        for one in &receipt_later.items {
            assert_eq!(skip_of(&one.outcome), Some(Skip::NotReviewed));
        }
        assert!(
            world_later.state().removals.is_empty(),
            "nothing was removed"
        );
    }
}

/// Catches `--only` that leaves rules out by omitting them, which would make them
/// pending, and a rule added later becoming active by any route but `--add` (pinned 12).
#[test]
fn only_leaves_the_rest_out_and_a_later_rule_stays_pending() {
    let mut desk = desk();
    let profile = desk.make(Some(&[CARGO_RULE]));
    let check = desk.check(&profile);
    assert_eq!(check.pending(), &[] as &[RuleId]);
    assert_eq!(check.left_out(), [rule_id(OTHER_RULE)]);

    desk.config.rules.push(rule("builtin.cargo.new", CARGO));
    let later = desk.check(&profile);
    assert_eq!(later.pending(), [rule_id("builtin.cargo.new")]);
    let review = profile::review(&profile, &desk.scene(), &BTreeSet::new()).unwrap();
    assert_eq!(review.draft, None, "nothing to review without --add");
    assert_eq!(review.not_added, [rule_id("builtin.cargo.new")]);

    let add = BTreeSet::from([rule_id("builtin.cargo.new")]);
    let adding = profile::review(&profile, &desk.scene(), &add).unwrap();
    assert_eq!(adding.added, [rule_id("builtin.cargo.new")]);
    let shelf = Shelf::default();
    let bytes = encode_profile(profile.contents(), &TestCodec);
    shelf.create_new(&name("nightly"), &bytes).unwrap();
    let reviewed = profile::replace(&shelf, &adding.draft.unwrap(), &TestCodec).unwrap();
    let after = desk.check(&reviewed);
    assert!(
        after.active().contains(&rule_id("builtin.cargo.new")),
        "added by name"
    );
    assert_eq!(after.left_out(), [rule_id(OTHER_RULE)], "still left out");
}

/// Catches a stored profile read leniently: a field this version doesn't know, a newer
/// version, or a profile including no rule would each be taken for one it can run.
#[test]
fn a_stored_profile_kipple_cant_vouch_for_is_refused() {
    let desk = desk();
    let draft = profile::freeze(name("nightly"), &desk.scene(), UNIX_EPOCH, None).unwrap();
    let bytes = encode_profile(draft.contents(), &TestCodec);
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let edits: [fn(&mut serde_json::Value); 4] = [
        |doc| doc["extra"] = serde_json::json!(1),
        |doc| doc["schema"] = serde_json::json!("kipple.profile/v2"),
        |doc| {
            for entry in doc["entries"].as_array_mut().unwrap() {
                entry["scope"] = serde_json::json!("left-out");
            }
        },
        |doc| doc["name"] = serde_json::json!("other"),
    ];
    let mut refused = Vec::new();
    for edit in edits {
        let mut edited = json.clone();
        edit(&mut edited);
        let shelf = Shelf::default();
        shelf
            .create_new(&name("nightly"), &serde_json::to_vec(&edited).unwrap())
            .unwrap();
        refused.push(profile::read(&shelf, &name("nightly"), &TestCodec).unwrap_err());
    }
    assert!(
        matches!(
            refused[0],
            ProfileError::Format(kipple_core::format::FormatError::Shape { .. })
        ),
        "an unknown field is refused as the wrong shape: {:?}",
        refused[0]
    );
    assert!(
        matches!(
            refused[1],
            ProfileError::Format(kipple_core::format::FormatError::Newer { .. })
        ),
        "a v2 profile reads as made by a newer kipple: {:?}",
        refused[1]
    );
    assert!(
        matches!(
            refused[2],
            ProfileError::Format(kipple_core::format::FormatError::Invalid { .. })
        ),
        "a profile with no included entry is refused as invalid: {:?}",
        refused[2]
    );
    assert_eq!(refused[3], ProfileError::Misnamed);
}

/// Catches a clean planned under one config and run under another as if nothing
/// changed: a `Prepared` run after the config was reloaded with another level skips each
/// item as judged under something else (spec M3c edge case: a `Prepared` run under a
/// reloaded config).
#[test]
fn a_prepared_clean_run_under_a_reloaded_config_skips_what_changed() {
    let mut desk = desk();
    let profile = desk.make(None);
    let check = desk.check(&profile);
    let prepared = desk.prepare(&check);
    desk.config.policy.level = Level::Thorough;
    let ran = desk.run(&prepared, &check, &desk.scan.clone());
    assert!(ran.done.is_empty(), "nothing ran under the reloaded config");
    for skip in ran.skipped.values() {
        assert!(
            matches!(skip, Some(Skip::Rejudged(_))),
            "skipped as judged under something else: {skip:?}"
        );
    }
}

/// Catches the clean flow losing the profile's grant on the way: `run_clean` with the
/// profile's consent runs exactly the profile's selection.
#[test]
fn clean_with_a_profiles_consent_runs_its_selection() {
    let desk = desk();
    let profile = desk.make(Some(&[OTHER_RULE]));
    let check = desk.check(&profile);
    let world = world_of(&desk.targets);
    let mutation = ports(&world);
    let rescan = rescanned(&desk.scan);
    let session = Session {
        judge: desk.config.judge(),
        store: &world,
        ports: ApplyPorts {
            rescan: &rescan,
            mutation: &mutation,
        },
        codec: &TestCodec,
    };
    let request = CleanRequest {
        scan: &desk.scan,
        selection: check.selection(),
    };
    let consent = Consent::Profile(check.grant().unwrap().clone());
    let finished = run_clean(&request, &session, consent, &mut (), &CancelToken::new());
    assert_eq!(finished.ending, Ending::Done);
    let done: Vec<_> = finished
        .receipt
        .items
        .iter()
        .map(|item| &item.path)
        .collect();
    assert_eq!(done, [&home("p2/out")]);
}

/// Catches a prepared clean that can't go to the worker thread that applies it
/// (pinned 1, 4).
#[test]
fn a_prepared_clean_can_go_to_a_worker() {
    fn send<T: Send>() {}
    send::<Prepared>();
}

/// One of `from`, as `rng` picks.
fn pick<T: Copy>(rng: &mut SplitMix, from: &[T]) -> T {
    from[rng.pick(from.len())]
}

/// One thing that can change between making a profile and running it.
fn mutate(desk: &mut Desk, profile: &mut Profile, rng: &mut SplitMix) {
    let rule = pick(rng, &[CARGO_RULE, OTHER_RULE]);
    let level = pick(rng, &[Level::Careful, Level::Balanced, Level::Thorough]);
    match rng.below(12) {
        0 => desk.config.policy.level = level,
        1 => desk.settings(rule).level = Some(level),
        2 => desk.settings(rule).recent_days = Some(pick(rng, &[1, 30, 400])),
        3 => desk.settings(rule).disabled = rng.below(2) == 0,
        4 => desk.config.methods.config = Some(pick(rng, &[Method::Delete, Method::Trash])),
        5 => desk.config.rules[0].recent_days = Some(pick(rng, &[1, 7, 90])),
        6 => {
            let opened = &mut desk.scan.integrations[0].opened[0];
            opened.identity = pick(rng, &[None, Some(id(77)), Some(HOME_ID)]);
        }
        7 => desk.config.policy.keep = vec![held(pick(rng, &["p0", "p9"]))],
        8 => desk.config.policy.keep.clear(),
        9 => {
            let finding = usize::try_from(rng.below(3)).unwrap();
            let facts = desk.scan.findings[finding].facts.as_mut().unwrap();
            facts.modified = pick(
                rng,
                &[
                    Modified::Ago(Duration::from_hours(1)),
                    Modified::Unknown(WalkGap::Unread),
                    Modified::Ago(Duration::from_hours(24 * 40)),
                ],
            );
        }
        10 => desk.digests = digests(pick(rng, &[1, 2])),
        // A restart, or a kipple upgrade that changed the classification: the profile is
        // read again from its file, as the older kipple wrote it.
        _ => {
            let mut json: serde_json::Value =
                serde_json::from_slice(&encode_profile(profile.contents(), &TestCodec)).unwrap();
            if rng.below(2) == 0 {
                json["policy_version"] = serde_json::json!(0);
            }
            let shelf = Shelf::default();
            shelf
                .create_new(&name("nightly"), &serde_json::to_vec(&json).unwrap())
                .unwrap();
            *profile = profile::read(&shelf, &name("nightly"), &TestCodec).unwrap();
        }
    }
}

/// Catches a profile run that widens, over generated sequences of config changes, roots
/// that fail or are replaced, re-reads, upgrades and items that change: every item a
/// profile run removes is of an active rule whose frozen level, window and method are
/// the current ones, below a root it froze, and Safe now (pinned 12).
#[test]
fn a_profile_run_never_widens_whatever_changes() {
    let mut rng = SplitMix(0x7072_6f66);
    let mut ran_some = 0;
    for _ in 0..150 {
        let mut desk = desk();
        for _ in 0..rng.below(2) {
            let mut unused = desk.make(None);
            mutate(&mut desk, &mut unused, &mut rng);
        }
        let only: &[&str] = pick(
            &mut rng,
            &[&[CARGO_RULE][..], &[OTHER_RULE], &[CARGO_RULE, OTHER_RULE]],
        );
        let Some(mut profile) = desk.try_make(Some(only)) else {
            continue;
        };
        for _ in 0..4 {
            mutate(&mut desk, &mut profile, &mut rng);
            let check = desk.check(&profile);
            for rule in check.pending().iter().chain(check.left_out()) {
                assert!(!check.active().contains(rule), "{rule:?} is never active");
            }
            let Some(ran) = desk.run_now(&profile) else {
                continue;
            };
            ran_some += usize::from(!ran.done.is_empty());
            for path in &ran.done {
                holds_for(&desk, &profile, &check, path);
            }
        }
    }
    assert!(
        ran_some > 20,
        "the sequences ran something often enough to mean something"
    );
}

/// What every item a profile run removed must hold.
fn holds_for(desk: &Desk, profile: &Profile, check: &ProfileCheck, path: &std::path::Path) {
    let finding = desk.scan.findings.iter().find(|f| f.path == path).unwrap();
    let rule = &finding.rule;
    assert!(check.active().contains(rule), "{rule:?} is active");
    let entry = profile
        .contents()
        .entries()
        .iter()
        .find(|entry| entry.rule() == rule)
        .unwrap();
    let policy = &desk.config.policy;
    assert_eq!(entry.level(), policy.level_of(rule));
    assert_eq!(entry.recent_days(), policy.configured_window(rule));
    assert_eq!(entry.method(), desk.config.methods.choice(rule).resolve());
    assert_eq!(
        finding.verdict(policy).map(|v| v.bucket()),
        Some(Bucket::Safe)
    );
    assert!(
        entry
            .roots()
            .iter()
            .any(|root| root.identity().is_some() && path.starts_with(root.path())),
        "below a root it froze"
    );
}

/// A profile as stored, made with a kept path that has a note holding `SECRET`: what the
/// free-text test reads (04 §8a).
pub(super) fn stored_with_a_note() -> Vec<u8> {
    let mut desk = desk();
    desk.config.policy.keep = vec![HeldPath {
        note: Some("SECRET".to_owned()),
        ..held("p9")
    }];
    let draft = profile::freeze(name("nightly"), &desk.scene(), UNIX_EPOCH, None).unwrap();
    encode_profile(draft.contents(), &TestCodec)
}
