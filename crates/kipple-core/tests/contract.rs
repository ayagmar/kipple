//! The mutation contract through its public types: frozen trees, revalidation that only
//! subtracts, outcomes measured against the reviewed tree, and method precedence
//! (`docs/04-safety-model.md` §3.5, §3.6, §7, §8; spec v2 pinned 3, 4, 7; v2.1 A–E).
#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a helper given a bad fixture fails the test"
)]

#[expect(
    dead_code,
    reason = "the trash fake and the freezer's table are for the apply loop's tests"
)]
#[path = "support/fakes.rs"]
mod fakes;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use fakes::{FakeExecutor, FakeJournal, Record, Script, TestCodec};
use kipple_core::{
    Activity, Allocated, Apparent, ApplyError, BadName, CancelToken, Completeness, Component,
    Consequence, Cost, DecodeError, Digest, Drift, EntryKind, EntrySnapshot, ErrorCode, Facts,
    FileIdentity, Fingerprint, Finish, FrozenTree, HeldPath, Intent, ItemId, ItemOutcome, Journal,
    Judgement, JudgementField, LeftReason, Level, Lineage, Loss, Mark, Marks, Method, MethodChoice,
    MethodRefusal, Modified, NameCodec as _, NodeId, Policy, Proposal, Receipt, ReceiptItem,
    Recovery, RelPath, RuleSettings, Scope, ShapeError, Skip, SpaceEstimate, Stamp, Status, Tally,
    Tracked, TrashDir, TrashSlot, Trashed, TreeBuilder, Unauthorized, Volume,
};

const VOLUME: Volume = Volume {
    device: 7,
    mount: Some(3),
};

fn name(text: &str) -> Component {
    Component::new(text).unwrap()
}

fn path(text: &str) -> RelPath {
    RelPath::of(Path::new(text)).unwrap()
}

/// One entry of a test tree, by its path below the target: a trailing `/` makes it a
/// directory.
#[derive(Debug, Clone, Copy)]
struct Spec<'text> {
    path: &'text str,
    file: u128,
    changed: i64,
    links: u64,
}

const fn spec(path: &str, file: u128) -> Spec<'_> {
    Spec {
        path,
        file,
        changed: 100,
        links: 1,
    }
}

fn snapshot(kind: EntryKind, file: u128, changed: i64, links: u64) -> EntrySnapshot {
    let stamp = |secs| Stamp::new(secs, 5).unwrap();
    EntrySnapshot {
        kind,
        identity: FileIdentity { volume: 7, file },
        fingerprint: Fingerprint {
            changed: stamp(changed),
            modified: stamp(50),
            born: Some(stamp(10)),
            size: 4096,
        },
        links,
    }
}

/// A tree whose top is directory `0`, holding `specs`, added in the order given.
fn build(scope: Scope, specs: &[Spec<'_>]) -> FrozenTree {
    let mut builder =
        TreeBuilder::new(scope, VOLUME, &snapshot(EntryKind::Dir, 1, 100, 1)).unwrap();
    let mut slots = BTreeMap::new();
    slots.insert(String::new(), builder.top());
    for spec in specs {
        let trimmed = spec.path.trim_end_matches('/');
        let kind = if spec.path.ends_with('/') {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let (parent, own) = trimmed.rsplit_once('/').unwrap_or(("", trimmed));
        let entry = snapshot(kind, spec.file, spec.changed, spec.links);
        let slot = builder.add(slots[parent], name(own), &entry).unwrap();
        slots.insert(trimmed.to_owned(), slot);
    }
    builder.finish().unwrap()
}

/// `target/` as Cargo leaves it: a profile directory with a binary hard-linked into
/// `deps/`, and incremental state.
fn target() -> Vec<Spec<'static>> {
    vec![
        spec("debug/", 2),
        Spec {
            links: 2,
            ..spec("debug/app", 3)
        },
        spec("debug/deps/", 4),
        Spec {
            links: 2,
            ..spec("debug/deps/app-1f2e", 3)
        },
        spec("debug/incremental/", 5),
        spec("debug/incremental/s-1", 6),
        spec(".rustc_info.json", 7),
    ]
}

fn find(tree: &FrozenTree, text: &str) -> NodeId {
    tree.find(&path(text)).unwrap()
}

#[test]
fn a_name_that_could_climb_out_or_name_two_levels_is_refused() {
    assert_eq!(Component::new(""), Err(BadName::Empty));
    assert_eq!(Component::new("."), Err(BadName::Dot));
    assert_eq!(Component::new(".."), Err(BadName::Dot));
    assert_eq!(Component::new("a/b"), Err(BadName::Separator));
    assert_eq!(Component::new("a\0b"), Err(BadName::Nul));
    assert!(
        Component::new("...").is_ok(),
        "three dots is an ordinary name"
    );
    assert_eq!(RelPath::of(Path::new("a/../b")), Err(BadName::Dot));
    assert!(
        RelPath::of(Path::new("/etc")).is_err(),
        "an absolute path isn't relative"
    );
}

/// Catches a lossy name: the entry removed would be a different one, or none.
#[cfg(unix)]
#[test]
fn a_non_utf8_name_survives_freezing_finding_and_storing() {
    use std::os::unix::ffi::OsStringExt as _;
    check_lossless(&OsString::from_vec(vec![b'f', 0xff, 0xfe, b'.', b'o']));
}

/// Catches a lossy name: Windows allows unpaired surrogates in names.
#[cfg(windows)]
#[test]
fn an_ill_formed_utf16_name_survives_freezing_finding_and_storing() {
    use std::os::windows::ffi::OsStringExt as _;
    check_lossless(&OsString::from_wide(&[0x66, 0xD800, 0x6F]));
}

fn check_lossless(odd: &OsString) {
    assert!(odd.to_str().is_none(), "the name isn't valid Unicode");
    let mut builder =
        TreeBuilder::new(Scope::Whole, VOLUME, &snapshot(EntryKind::Dir, 1, 100, 1)).unwrap();
    let top = builder.top();
    let component = Component::new(odd.clone()).unwrap();
    builder
        .add(
            top,
            component.clone(),
            &snapshot(EntryKind::File, 2, 100, 1),
        )
        .unwrap();
    let tree = builder.finish().unwrap();
    let id = tree.find(&RelPath::new(vec![component])).unwrap();
    assert_eq!(tree.entry(id).unwrap().name().unwrap().as_os_str(), odd);

    let stored = FrozenTree::decode(&tree.encode(&TestCodec), &TestCodec).unwrap();
    assert_eq!(stored, tree);
    assert_eq!(stored.digest(), tree.digest());
}

/// Catches a digest or stored form that depends on the order the freezer read entries
/// in, which would make an unchanged item look changed after a crash.
#[test]
fn the_same_entries_in_any_order_freeze_to_one_tree() {
    let forward = build(Scope::Whole, &target());
    let mut reversed = target();
    // Children may only follow their directory, so reverse within each level.
    reversed.sort_by_key(|spec| {
        let depth = spec.path.trim_end_matches('/').matches('/').count();
        (depth, std::cmp::Reverse(spec.path))
    });
    let backward = build(Scope::Whole, &reversed);
    assert_eq!(forward, backward);
    assert_eq!(forward.digest(), backward.digest());
    assert_eq!(forward.encode(&TestCodec), backward.encode(&TestCodec));

    let names: Vec<String> = forward
        .top()
        .children()
        .map(|child| {
            child
                .name()
                .unwrap()
                .as_os_str()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        names,
        [".rustc_info.json", "debug"],
        "children sorted by name"
    );
}

/// Catches a tree that holds an entry kipple can't account for: two of one name, one
/// on another file system, one inside a file, or a link as the target.
#[test]
fn a_tree_refuses_entries_it_cannot_stand_for() {
    let mut builder =
        TreeBuilder::new(Scope::Whole, VOLUME, &snapshot(EntryKind::Dir, 1, 100, 1)).unwrap();
    let top = builder.top();
    let file = builder
        .add(top, name("a"), &snapshot(EntryKind::File, 2, 100, 1))
        .unwrap();
    let inside = builder.add(file, name("b"), &snapshot(EntryKind::File, 3, 100, 1));
    assert_eq!(inside, Err(ShapeError::NotADirectory));
    let mut elsewhere = snapshot(EntryKind::File, 4, 100, 1);
    elsewhere.identity.volume = 8;
    assert_eq!(
        builder.add(top, name("c"), &elsewhere),
        Err(ShapeError::OtherVolume)
    );
    builder
        .add(top, name("a"), &snapshot(EntryKind::File, 5, 100, 1))
        .unwrap();
    assert_eq!(builder.finish(), Err(ShapeError::Duplicate("a".into())));

    let link = snapshot(EntryKind::Symlink, 1, 100, 1);
    assert_eq!(
        TreeBuilder::new(Scope::Whole, VOLUME, &link).err(),
        Some(ShapeError::TopIsLink)
    );
}

/// Catches Cargo's hard-linked outputs being kept as "changed" because kipple removed
/// their other name first, and an edit to them going unnoticed (v2.1 A).
#[test]
fn a_hard_linked_file_is_compared_by_contents_and_a_directory_by_everything() {
    let file = snapshot(EntryKind::File, 3, 100, 2);
    let sibling_unlinked = EntrySnapshot {
        links: 1,
        ..snapshot(EntryKind::File, 3, 200, 1)
    };
    assert_eq!(file.drift(&sibling_unlinked), None);
    let mut edited = sibling_unlinked;
    edited.fingerprint.size = 1;
    assert_eq!(file.drift(&edited), Some(Drift::Changed));

    let single = snapshot(EntryKind::File, 3, 100, 1);
    assert_eq!(
        single.drift(&snapshot(EntryKind::File, 3, 101, 1)),
        Some(Drift::Changed)
    );
    assert_eq!(
        single.drift(&snapshot(EntryKind::File, 9, 100, 1)),
        Some(Drift::Replaced)
    );
    assert_eq!(
        single.drift(&snapshot(EntryKind::Dir, 3, 100, 1)),
        Some(Drift::Replaced)
    );

    // A Linux directory's link count is two plus its subdirectories: never a hard link.
    let dir = snapshot(EntryKind::Dir, 4, 100, 3);
    assert_eq!(
        dir.drift(&snapshot(EntryKind::Dir, 4, 101, 3)),
        Some(Drift::Changed)
    );
}

/// Catches two different Windows times becoming one stamp, which would hide a change.
#[test]
fn windows_times_convert_exactly() {
    let epoch = Stamp::from_windows_ticks(116_444_736_000_000_000);
    assert_eq!((epoch.secs(), epoch.nanos()), (0, 0));
    let before = Stamp::from_windows_ticks(116_444_735_999_999_999);
    assert_eq!((before.secs(), before.nanos()), (-1, 999_999_900));
    for ticks in [i64::MIN, -1, 0, 1, i64::MAX - 1] {
        assert_ne!(
            Stamp::from_windows_ticks(ticks),
            Stamp::from_windows_ticks(ticks + 1)
        );
    }
    assert_eq!(Stamp::new(0, 1_000_000_000), None);
}

/// Catches a stored tree that comes back different, or a damaged one read as valid
/// with a meaning the bytes don't have: every accepted form is the canonical one.
#[test]
fn a_stored_tree_reads_back_exactly_and_damage_is_never_read_as_something_else() {
    let mut tree = build(Scope::Contents, &target());
    tree.exclude(find(&tree, "debug/incremental"));
    let stored = tree.encode(&TestCodec);
    assert_eq!(FrozenTree::decode(&stored, &TestCodec).unwrap(), tree);

    for cut in 0..stored.len() {
        assert!(
            FrozenTree::decode(&stored[..cut], &TestCodec).is_err(),
            "a tree cut at {cut} bytes is refused"
        );
    }
    let mut longer = stored.clone();
    longer.push(0);
    assert!(
        FrozenTree::decode(&longer, &TestCodec).is_err(),
        "trailing bytes are refused"
    );
    for at in 0..stored.len() {
        for flip in [0x01, 0x80, 0xff] {
            let mut damaged = stored.clone();
            damaged[at] ^= flip;
            if let Ok(read) = FrozenTree::decode(&damaged, &TestCodec) {
                assert_eq!(
                    read.encode(&TestCodec),
                    damaged,
                    "byte {at} ^ {flip:#x} was read as a tree that stores differently"
                );
                assert_eq!(
                    rebuilt(&read),
                    read,
                    "byte {at} ^ {flip:#x} was read as a tree no freeze makes"
                );
            }
        }
    }
}

/// `tree` made again the only way a freeze makes one: through a `TreeBuilder`, each entry
/// inside its directory, then the plan's exclusions. A decoded tree that differs from it
/// is one no freeze could have produced.
fn rebuilt(tree: &FrozenTree) -> FrozenTree {
    let top = tree.top().snapshot().unwrap();
    let mut builder = TreeBuilder::new(tree.scope(), tree.volume(), &top).unwrap();
    let mut slots = BTreeMap::from([(NodeId::TOP, builder.top())]);
    for entry in tree.entries().skip(1) {
        let parent = slots[&entry.parent().unwrap().id()];
        let snapshot = entry.snapshot().unwrap();
        let slot = builder
            .add(parent, entry.name().unwrap().clone(), &snapshot)
            .unwrap();
        slots.insert(entry.id(), slot);
    }
    let mut again = builder.finish().unwrap();
    for entry in tree.entries() {
        if entry.mark() == Mark::Unauthorized(Unauthorized::Excluded) {
            again.exclude(again.find(&entry.path()).unwrap());
        }
    }
    again
}

/// One node of a hand-written stored tree: how many entries it holds, its name's number
/// plus one (0 for the top), and its flags (kind in bits 0-1, mark in bits 2-3).
#[derive(Debug, Clone, Copy)]
struct Raw {
    children: u8,
    name: u8,
    flags: u8,
}

const DIR: u8 = 0;
const FILE: u8 = 1;
const EXCLUDED: u8 = 2 << 2;

/// A version 1 stored tree of `names` and `nodes`, in the documented layout: a whole
/// target on device 7, each node with one link, file ID its index plus one, size 0 and
/// times 0.
fn stored(names: &[&str], nodes: &[Raw]) -> Vec<u8> {
    let small = |count: usize| u8::try_from(count).unwrap();
    let mut bytes = b"kipple-tree\0".to_vec();
    bytes.extend([1, 0, 0, 7, 0, small(names.len()), small(nodes.len())]);
    for name in names {
        let encoded = TestCodec.encode(OsStr::new(name));
        bytes.push(small(encoded.len()));
        bytes.extend(encoded);
    }
    for (index, node) in nodes.iter().enumerate() {
        bytes.extend([node.children, node.name, node.flags, 1, small(index + 1)]);
        bytes.extend([0; 5]);
    }
    bytes
}

const fn raw(children: u8, name: u8, flags: u8) -> Raw {
    Raw {
        children,
        name,
        flags,
    }
}

/// Catches a stored tree that no freeze could make being read as a plan: an entry that is
/// its own directory or comes before it (walking up from it never ends), something kept
/// out of the plan holding entries marked for removal, and a name table in another order
/// or with names no entry uses (one tree, two digests).
#[test]
fn a_stored_tree_is_read_only_in_the_one_form_a_freeze_makes() {
    let two_files = [raw(2, 0, DIR), raw(0, 1, FILE), raw(0, 2, FILE)];
    let read = FrozenTree::decode(&stored(&["a", "b"], &two_files), &TestCodec).unwrap();
    assert_eq!(rebuilt(&read), read);
    let kept_dir = [
        raw(1, 0, DIR),
        raw(1, 1, DIR | EXCLUDED),
        raw(0, 2, FILE | EXCLUDED),
    ];
    let kept = FrozenTree::decode(&stored(&["d", "f"], &kept_dir), &TestCodec).unwrap();
    assert_eq!(rebuilt(&kept), kept);

    let refused: [(&str, &[&str], &[Raw]); 5] = [
        (
            "its own directory",
            &["a"],
            &[raw(0, 0, DIR), raw(1, 1, DIR)],
        ),
        (
            "a directory after its entry",
            &["a", "b"],
            &[raw(0, 0, DIR), raw(0, 1, FILE), raw(2, 2, DIR)],
        ),
        (
            "removal inside an excluded directory",
            &["d", "f"],
            &[raw(1, 0, DIR), raw(1, 1, DIR | EXCLUDED), raw(0, 2, FILE)],
        ),
        (
            "names out of first-use order",
            &["b", "a"],
            &[raw(2, 0, DIR), raw(0, 2, FILE), raw(0, 1, FILE)],
        ),
        (
            "an unused name",
            &["a", "z"],
            &[raw(1, 0, DIR), raw(0, 1, FILE)],
        ),
    ];
    for (case, names, nodes) in refused {
        assert!(
            matches!(
                FrozenTree::decode(&stored(names, nodes), &TestCodec),
                Err(DecodeError::Malformed(_))
            ),
            "{case} is refused"
        );
    }
}

/// A deterministic generator, so the property test needs no dependency.
struct SplitMix(u64);

impl SplitMix {
    const fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    const fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// A random tree of up to 40 entries, as `(path, spec)` pairs in an order a walk could
/// add them.
fn random_specs(rng: &mut SplitMix) -> Vec<(String, u128, i64)> {
    let mut dirs = vec![String::new()];
    let mut specs = Vec::new();
    for index in 0..rng.below(40) {
        let parent = dirs[usize::try_from(rng.below(dirs.len() as u64)).unwrap()].clone();
        let own = format!("{parent}n{}", rng.below(6));
        if specs
            .iter()
            .any(|(path, _, _): &(String, u128, i64)| path.trim_end_matches('/') == own)
        {
            continue;
        }
        let file = u128::from(index) + 10;
        if rng.below(3) == 0 {
            specs.push((format!("{own}/"), file, 100));
            dirs.push(format!("{own}/"));
        } else {
            specs.push((own, file, 100));
        }
    }
    specs
}

/// Catches revalidation that adds or substitutes a target, or keeps one the fresh plan
/// leaves out (04 §3.6): over generated trees, changes and fresh exclusions, every entry
/// it leaves is one the plan authorized, and one the fresh tree has unchanged at the same
/// place and still authorizes.
#[test]
fn revalidation_only_ever_subtracts() {
    let mut rng = SplitMix(0x6b69_7070);
    for _ in 0..500 {
        let frozen_specs = random_specs(&mut rng);
        let mut fresh_specs = frozen_specs.clone();
        for spec in &mut fresh_specs {
            match rng.below(8) {
                0 => spec.1 += 1000,
                1 => spec.2 += 1,
                _ => {}
            }
        }
        fresh_specs.retain(|_| rng.below(10) != 0);
        fresh_specs.push((format!("new{}", rng.below(4)), 5000, 100));
        let frozen = build(Scope::Whole, &as_specs(&frozen_specs));
        let mut fresh = build(Scope::Whole, &as_specs(&fresh_specs));
        let kept: Vec<NodeId> = fresh
            .entries()
            .map(|entry| entry.id())
            .filter(|_| rng.below(12) == 0)
            .collect();
        for id in kept {
            fresh.exclude(id);
        }
        check_subset(&frozen, &fresh, &frozen.recheck(&fresh));
    }
}

/// The specs of `entries`, in order, leaving out any whose directory isn't kept.
fn as_specs(entries: &[(String, u128, i64)]) -> Vec<Spec<'_>> {
    let mut kept = BTreeSet::from([String::new()]);
    let mut specs = Vec::new();
    for (path, file, changed) in entries {
        let trimmed = path.trim_end_matches('/');
        let parent = trimmed
            .rsplit_once('/')
            .map_or(String::new(), |(parent, _)| format!("{parent}/"));
        if !kept.contains(&parent) {
            continue;
        }
        if path.ends_with('/') {
            kept.insert(path.clone());
        }
        specs.push(Spec {
            changed: *changed,
            ..spec(path, *file)
        });
    }
    specs
}

fn check_subset(frozen: &FrozenTree, fresh: &FrozenTree, marks: &Marks) {
    for entry in frozen.entries() {
        let mark = marks.get(entry.id());
        let base = frozen.mark(entry.id());
        assert!(
            mark == base || (base == Mark::Remove && matches!(mark, Mark::Dropped(_))),
            "a mark only ever leaves remove: {base:?} became {mark:?}"
        );
        if mark != Mark::Remove {
            continue;
        }
        let now = fresh.find(&entry.path()).and_then(|id| fresh.entry(id));
        assert_eq!(
            now.map(|now| now.mark()),
            Some(Mark::Remove),
            "{} stays authorized only where the fresh plan authorizes it",
            entry.path()
        );
        let now = now.and_then(|now| now.snapshot());
        let then = entry.snapshot().unwrap();
        assert_eq!(
            now.map(|now| then.drift(&now)),
            Some(None),
            "{} stays authorized only unchanged in the fresh tree",
            entry.path()
        );
        if let Some(parent) = entry.parent() {
            assert_eq!(
                marks.get(parent.id()),
                Mark::Remove,
                "{} stays authorized only inside an authorized directory",
                entry.path()
            );
        }
    }
}

/// Catches entering a directory that changed since the freeze, which may hold a new
/// entry, and losing count of what was inside it (v2.1 B).
#[test]
fn a_changed_directory_is_dropped_with_everything_inside_it() {
    let frozen = build(Scope::Whole, &target());
    let mut changed = target();
    changed[4].changed += 1; // debug/incremental/
    let fresh = build(Scope::Whole, &changed);
    let marks = frozen.recheck(&fresh);
    assert_eq!(
        marks.get(find(&frozen, "debug/incremental")),
        Mark::Dropped(LeftReason::Changed)
    );
    assert_eq!(
        marks.get(find(&frozen, "debug/incremental/s-1")),
        Mark::Dropped(LeftReason::Changed)
    );
    assert_eq!(marks.get(find(&frozen, "debug/app")), Mark::Remove);

    let moved = Volume {
        mount: Some(4),
        ..VOLUME
    };
    let builder =
        TreeBuilder::new(Scope::Whole, moved, &snapshot(EntryKind::Dir, 1, 100, 1)).unwrap();
    let remounted = builder.finish().unwrap();
    assert_eq!(
        frozen.recheck(&remounted).authorized(),
        0,
        "another mount is replaced"
    );
}

/// Runs the fake executor over `tree` with `marks` and returns the outcome.
fn run(tree: &FrozenTree, marks: &Marks, script: Script) -> ItemOutcome {
    let executor = FakeExecutor {
        script,
        ..FakeExecutor::default()
    };
    let mut tally = Tally::new(tree, marks);
    let result = executor.run(tree, marks, &mut tally, &CancelToken::new());
    tally.finish(result)
}

/// Catches an item reported as done when the user's reviewed entries weren't all
/// removed, and hard-linked names counted as freeing their file twice (v2.1 C, E).
#[test]
fn an_item_is_done_only_when_every_reviewed_entry_went() {
    let tree = build(Scope::Whole, &target());
    let ItemOutcome::Done(effect) = run(&tree, &Marks::of(&tree), Script::default()) else {
        panic!("every entry removed is done");
    };
    assert_eq!(effect.removed.names, 8);
    assert_eq!(
        effect.removed.shared, 2,
        "both names of a two-name file free nothing"
    );
    assert_eq!(effect.removed.estimated, Allocated(6 * 4096));

    let mut marks = Marks::of(&tree);
    marks.drop_subtree(&tree, find(&tree, ".rustc_info.json"), LeftReason::Changed);
    let ItemOutcome::Partial(partial) = run(&tree, &marks, Script::default()) else {
        panic!("an entry dropped at revalidation makes the item partial");
    };
    let changed = &partial.left.0[&LeftReason::Changed];
    assert_eq!(changed.count, 1);
    assert_eq!(changed.sample, [path(".rustc_info.json")]);
}

/// Catches a directory kept because something appeared in it during the run being
/// reported as a failure, which pushes users to force it (pinned 7).
#[test]
fn a_directory_kept_for_new_entries_is_a_note_not_a_failure() {
    let tree = build(Scope::Whole, &target());
    let script = Script {
        appeared: BTreeSet::from([path("debug/deps")]),
        ..Script::default()
    };
    let ItemOutcome::Done(effect) = run(&tree, &Marks::of(&tree), script) else {
        panic!("still done");
    };
    assert_eq!(
        effect.left.0[&LeftReason::NotEmpty].count,
        3,
        "deps, debug and the top"
    );
}

/// Catches what a failed or cancelled item removed going missing from the totals (04 §7,
/// pinned 7), and an OS refusal of one entry stopping the rest.
#[test]
fn removals_inside_failed_and_cancelled_items_count_and_the_rest_is_not_reached() {
    let tree = build(Scope::Whole, &target());
    let marks = Marks::of(&tree);
    let denied = Script {
        denied: BTreeSet::from([path(".rustc_info.json")]),
        ..Script::default()
    };
    let ItemOutcome::Partial(partial) = run(&tree, &marks, denied) else {
        panic!("a refusal leaves the entry and goes on");
    };
    assert_eq!(partial.left.0[&LeftReason::Denied].count, 1);

    let failing = Script {
        fails_at: Some(path("debug/app")),
        ..Script::default()
    };
    let failed = run(&tree, &marks, failing);
    let ItemOutcome::Failed { error, effect } = &failed else {
        panic!("an error stops the item");
    };
    assert_eq!(error.code, ErrorCode::Io);
    assert!(
        effect.removed.names > 0,
        "what went before the error counts"
    );
    assert!(
        effect.left.0[&LeftReason::NotReached].count > 0,
        "what came after is not reached"
    );

    let cancelling = Script {
        cancel_after: Some(2),
        ..Script::default()
    };
    let cancelled = run(&tree, &marks, cancelling);
    assert_eq!(cancelled.status(), Status::Cancelled);

    let estimate = SpaceEstimate {
        apparent: Apparent(9000),
        allocated: Some(Allocated(12_288)),
        unique_reclaim: None,
        completeness: Completeness::Complete,
    };
    let item = |outcome: ItemOutcome, method| ReceiptItem {
        item: ItemId(0),
        path: PathBuf::from("/p/target"),
        method,
        estimate: Some(estimate),
        outcome,
    };
    let trashed = ItemOutcome::Done(kipple_core::Effect {
        trashed: Some(Trashed { slot: slot() }),
        ..kipple_core::Effect::default()
    });
    let receipt = Receipt {
        items: vec![
            item(failed.clone(), Method::Delete),
            item(cancelled.clone(), Method::Delete),
            item(ItemOutcome::Skipped(Skip::NotReviewed), Method::Delete),
            item(trashed, Method::Trash),
        ],
    };
    let totals = receipt.totals();
    let names = failed.effect().unwrap().removed.names + cancelled.effect().unwrap().removed.names;
    assert_eq!(totals.removed.names, names);
    assert_eq!(totals.trashed.items, 1);
    assert_eq!(totals.trashed.allocated, Some(Allocated(12_288)));
    assert_eq!(totals.items[&Status::Skipped], 1);
}

fn slot() -> TrashSlot {
    TrashSlot {
        trash: TrashDir {
            path: PathBuf::from("/home/u/.local/share/Trash/files"),
            identity: FileIdentity {
                volume: 7,
                file: 77,
            },
        },
        name: name("target"),
    }
}

/// Catches a receipt that lists every leaf of a huge tree, or a sample that changes
/// from run to run.
#[test]
fn a_receipt_keeps_a_bounded_sample_of_what_was_left_in_tree_order() {
    let specs: Vec<String> = (0..30).map(|i| format!("f{i:02}")).collect();
    let specs: Vec<Spec<'_>> = specs
        .iter()
        .enumerate()
        .map(|(i, path)| spec(path, 10 + i as u128))
        .collect();
    let tree = build(Scope::Contents, &specs);
    let mut marks = Marks::of(&tree);
    marks.drop_subtree(&tree, NodeId::TOP, LeftReason::Gone);
    let left = Tally::new(&tree, &marks).before_run().left;
    let gone = &left.0[&LeftReason::Gone];
    assert_eq!(
        gone.count, 30,
        "the top of a root's contents was never authorized"
    );
    assert_eq!(gone.sample.len(), kipple_core::SAMPLE);
    assert_eq!(gone.sample[0], path("f00"));
    assert_eq!(
        tree.mark(NodeId::TOP),
        Mark::Unauthorized(Unauthorized::Top)
    );
}

/// Catches an entry reported twice counting twice, and a report about an entry the
/// operation doesn't authorize counting at all.
#[test]
fn each_entry_counts_once_and_only_if_authorized() {
    use kipple_core::{EffectSink as _, Freed};
    let mut tree = build(Scope::Whole, &target());
    let excluded = find(&tree, ".rustc_info.json");
    tree.exclude(excluded);
    let marks = Marks::of(&tree);
    let mut tally = Tally::new(&tree, &marks);
    let app = find(&tree, "debug/app");
    tally.removed(app, Freed::Allocated(Allocated(10)));
    tally.removed(app, Freed::Allocated(Allocated(10)));
    tally.left(app, LeftReason::Busy);
    tally.removed(excluded, Freed::Allocated(Allocated(10)));
    let ItemOutcome::Cancelled(effect) = tally.finish(Ok(Finish::Cancelled)) else {
        panic!("cancelled");
    };
    assert_eq!(effect.removed.names, 1);
    assert_eq!(effect.excluded, 1);
    assert!(
        !effect.left.0.contains_key(&LeftReason::Busy),
        "a second report is ignored"
    );
}

/// Catches a method picked by the wrong precedence, or a trash item quietly planned as
/// a delete (03 §6).
#[test]
fn the_method_is_the_most_specific_choice_and_never_a_substitute() {
    let all = MethodChoice {
        command_line: Some(Method::Trash),
        rule: Some(Method::Delete),
        config: Some(Method::Delete),
    };
    assert_eq!(all.resolve(), Method::Trash);
    let rule = MethodChoice {
        command_line: None,
        ..all
    };
    assert_eq!(rule.resolve(), Method::Delete);
    assert_eq!(MethodChoice::default().resolve(), Method::Delete);

    let delete_only = Proposal::Remove(vec![Method::Delete]);
    assert_eq!(
        all.for_proposal(&delete_only),
        Err(MethodRefusal::NotOffered(Method::Trash))
    );
    assert_eq!(
        all.for_proposal(&Proposal::Report),
        Err(MethodRefusal::ReportOnly)
    );
}

fn rule() -> kipple_core::Rule {
    use kipple_core::{Category, IntegrationId, Operation, Platform, RuleId, RuleText, Selector};
    kipple_core::Rule {
        id: RuleId("builtin.cargo.target".to_owned()),
        integration: IntegrationId("cargo"),
        text: RuleText {
            title: "Rust build output".to_owned(),
            description: String::new(),
            why: String::new(),
            loss: String::new(),
            references: Vec::new(),
        },
        category: Category::BuildArtifact,
        platforms: Platform::ALL.to_vec(),
        root: kipple_core::SymbolicRoot::ProjectRoots,
        selector: Selector::Integration("cargo.target"),
        requirements: Vec::new(),
        eligibility: Vec::new(),
        operation: Operation::RemoveFiles(vec![Method::Delete, Method::Trash]),
        recent_days: None,
    }
}

fn facts() -> Facts {
    Facts {
        proposal: Proposal::Remove(vec![Method::Delete]),
        layout_verified: true,
        consequence: Some(Consequence {
            recovery: Recovery::RebuildLocal,
            loss: &[Loss::CacheWarmth],
            cost: Cost::Low,
        }),
        tracked: Tracked::NoRepository,
        activity: Activity::Idle,
        modified: Modified::Ago(Duration::from_hours(24 * 40)),
        recent_days: None,
        lineage: Lineage::default(),
    }
}

/// Catches a plan applied after the config changed what it was judged under: a kept
/// path, a rule's level or the rule itself (v2.1 H; 11 M3's profile regressions).
#[test]
fn a_judgement_changes_with_whatever_the_item_was_judged_under() {
    let policy = Policy::default();
    let then = Judgement::new(&policy, &rule(), &facts(), Method::Delete);
    assert_eq!(then.first_difference(&then.clone()), None);

    let mut kept = policy.clone();
    kept.keep.push(HeldPath {
        forms: vec![PathBuf::from("/home/u/keep")],
        ..HeldPath::default()
    });
    let kept = Judgement::new(&kept, &rule(), &facts(), Method::Delete);
    assert_eq!(then.first_difference(&kept), Some(JudgementField::Held));

    let mut level = policy.clone();
    level.rules.insert(
        rule().id,
        RuleSettings {
            level: Some(Level::Thorough),
            ..RuleSettings::default()
        },
    );
    let level = Judgement::new(&level, &rule(), &facts(), Method::Delete);
    assert_eq!(then.first_difference(&level), Some(JudgementField::Level));

    let mut wider = rule();
    wider.recent_days = Some(30);
    let wider = Judgement::new(&policy, &wider, &facts(), Method::Delete);
    assert_eq!(then.first_difference(&wider), Some(JudgementField::Rule));
}

/// The fake journal holds the apply loop to the documented order; this checks the fake
/// itself catches what it must, since the apply loop's tests rely on it.
#[test]
fn the_fake_journal_catches_an_outcome_without_an_intent_and_a_failing_store() {
    let intent = Intent {
        plan: Digest(1),
        item: ItemId(0),
        method: Method::Delete,
        reviewed: Digest(2),
        authorized: Digest(3),
        entries: 4,
        trash: None,
    };
    let mut journal = FakeJournal {
        records: vec![Record::Plan(Digest(1))],
        ..FakeJournal::default()
    };
    journal
        .outcome(ItemId(0), &ItemOutcome::Skipped(Skip::NotReviewed))
        .unwrap();
    assert!(
        journal.order_problem().is_some(),
        "an outcome needs an intent"
    );

    let mut failing = FakeJournal {
        records: vec![Record::Plan(Digest(1))],
        fails_at: Some(1),
        ..FakeJournal::default()
    };
    failing.intent(&intent).unwrap();
    assert_eq!(
        failing.intent(&intent),
        Err(ApplyError::new(ErrorCode::Journal))
    );
    assert_eq!(failing.order_problem(), None);
}
