//! The trash on the real file system (spike S4): moving a reviewed item whole into a
//! trash and back, and every refusal 04 §5 and spec pinned 9 and 10 ask for. Each test
//! names what it catches. The trash is a disposable directory laid out as the OS's own,
//! except in the one test that uses the account's real trash on a disposable host.
#![cfg_attr(
    unix,
    expect(
        clippy::unwrap_used,
        reason = "a fixture that fails to build fails the test"
    )
)]

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use kipple_core::{
        Anchor, ApplyError, CancelToken, Clock, ErrorCode, FrozenTree, LeftReason, Marks, NodeId,
        RelPath, RestoreRequest, SlotState, TrashBackend as _, TrashDir, TrashOp, TrashSlot,
    };
    use kipple_platform::ConfinedTrash;
    use tempfile::TempDir;

    use crate::executor::support::{
        Site, anchor_at, freeze, identity, link, listing, move_to, present, write,
    };

    /// A clock stopped at 2023-11-14T22:13:20Z.
    #[derive(Debug)]
    struct Stopped;

    impl Clock for Stopped {
        fn now(&self) -> SystemTime {
            UNIX_EPOCH + Duration::from_secs(1_700_000_000)
        }
    }

    /// A home trash in its own temp directory, laid out as the OS's: freedesktop's
    /// `Trash/files` and `Trash/info` on Linux, `.Trash` on macOS.
    struct Home {
        _temp: TempDir,
        files: PathBuf,
        backend: ConfinedTrash,
    }

    impl Home {
        fn new() -> Self {
            let temp = TempDir::new().unwrap();
            let files = if cfg!(target_os = "linux") {
                fs::create_dir_all(temp.path().join("Trash/info")).unwrap();
                temp.path().join("Trash/files")
            } else {
                temp.path().join(".Trash")
            };
            fs::create_dir_all(&files).unwrap();
            Self {
                backend: backend(Some(files.clone())),
                files,
                _temp: temp,
            }
        }
    }

    /// Where `slot` holds its item.
    fn held(slot: &TrashSlot) -> PathBuf {
        slot.trash.path.join(slot.name.as_os_str())
    }

    fn backend(home: Option<PathBuf>) -> ConfinedTrash {
        ConfinedTrash::new(home, Arc::new(Stopped))
    }

    /// The `.trashinfo` describing `slot` on Linux; `None` on macOS, which has none.
    fn info(slot: &TrashSlot) -> Option<PathBuf> {
        cfg!(target_os = "linux").then(|| {
            let mut name = slot.name.as_os_str().to_owned();
            name.push(".trashinfo");
            slot.trash.path.parent().unwrap().join("info").join(name)
        })
    }

    /// An authorized move of everything frozen at `anchor` now.
    fn whole(anchor: &Anchor) -> TrashOp {
        let tree = freeze(anchor).tree;
        let marks = Marks::of(&tree);
        TrashOp::for_platform_tests(anchor.clone(), tree, marks)
    }

    /// What restores the item `op` moved to `slot`.
    fn request(op: &TrashOp, slot: &TrashSlot) -> RestoreRequest {
        let target = op.target();
        let top = target.tree().top().snapshot().unwrap();
        RestoreRequest::new(slot.clone(), target.anchor().clone(), top).unwrap()
    }

    /// Moves `op`'s item into `trash`, which must take it.
    fn trashed(trash: &ConfinedTrash, op: &TrashOp) -> TrashSlot {
        let slot = trash.destination(op).unwrap();
        trash.move_whole(op, &slot, &CancelToken::new()).unwrap();
        slot
    }

    /// `target/` holding files at two depths.
    fn nested(site: &Site, relative: &str) {
        write(&site.at(&format!("{relative}/a")), b"a");
        write(&site.at(&format!("{relative}/sub/b")), b"bb");
    }

    /// Removes `path` and everything in it, as a user emptying the trash would.
    #[expect(
        clippy::disallowed_methods,
        reason = "the test plays a user who empties the trash"
    )]
    fn empty(path: &Path) {
        fs::remove_dir_all(path).unwrap();
    }

    /// Catches a trash move that leaves part of the item behind, loses an entry, or
    /// can't be undone: the item moves whole under the slot named before the move, and
    /// comes back whole, as itself.
    #[test]
    fn an_item_moves_whole_to_the_trash_and_back() {
        let site = Site::new();
        let home = Home::new();
        nested(&site, "target");
        write(&site.at("sibling"), b"stays");
        let before = listing(&site.at("target")).len();
        let was = identity(&site.at("target"));
        let anchor = site.anchor("target");
        let op = whole(&anchor);

        let slot = trashed(&home.backend, &op);

        assert!(!present(&site.at("target")), "the target left");
        assert_eq!(slot.trash.path, home.files);
        assert_eq!(identity(&held(&slot)), was, "the trash holds the item");
        assert_eq!(listing(&held(&slot)).len(), before);
        let top = op.target().tree().top().snapshot().unwrap();
        assert_eq!(home.backend.find(&slot, &top), Ok(SlotState::Holds));

        home.backend
            .restore(&request(&op, &slot), &CancelToken::new())
            .unwrap();

        assert_eq!(identity(&site.at("target")), was, "the item is back");
        assert_eq!(listing(&site.at("target")).len(), before);
        assert!(!present(&held(&slot)), "the slot is empty");
        assert_eq!(home.backend.find(&slot, &top), Ok(SlotState::Empty));
        if let Some(info) = info(&slot) {
            assert!(!present(&info), "its .trashinfo went with it");
        }
        assert!(present(&site.at("sibling")), "its sibling stays");
        site.assert_outside_intact();
    }

    /// Catches a freedesktop trash entry a file manager can't list or restore: the
    /// `.trashinfo` names the item's absolute path and when it was trashed.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_trashed_item_is_described_for_file_managers() {
        let site = Site::new();
        let home = Home::new();
        write(&site.at("a dir/target/x"), b"x");
        let op = whole(&site.anchor("a dir/target"));

        let slot = trashed(&home.backend, &op);

        let text = fs::read_to_string(info(&slot).unwrap()).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("[Trash Info]"));
        let path = lines.next().unwrap().strip_prefix("Path=/").unwrap();
        assert!(path.ends_with("/root/a%20dir/target"), "{path}");
        let date = lines.next().unwrap().strip_prefix("DeletionDate=").unwrap();
        assert_eq!(date.len(), "2023-11-14T22:13:20".len(), "{date}");
        assert!(
            date.starts_with("2023-11-1"),
            "local time of the clock: {date}"
        );
    }

    /// Catches an item that can't be found or restored once its trash lost the `info`
    /// directory: only a move needs it.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_item_stays_restorable_without_its_trashinfo() {
        let site = Site::new();
        let home = Home::new();
        nested(&site, "target");
        let op = whole(&site.anchor("target"));
        let top = op.target().tree().top().snapshot().unwrap();
        let slot = trashed(&home.backend, &op);
        empty(info(&slot).unwrap().parent().unwrap());

        assert_eq!(home.backend.find(&slot, &top), Ok(SlotState::Holds));
        home.backend
            .restore(&request(&op, &slot), &CancelToken::new())
            .unwrap();
        assert!(present(&site.at("target/sub/b")), "the item is back");
    }

    /// Catches a restore that overwrites or merges into what now has the item's name.
    #[test]
    fn a_restore_never_replaces_what_now_has_the_name() {
        let site = Site::new();
        let home = Home::new();
        nested(&site, "target");
        let op = whole(&site.anchor("target"));
        let slot = trashed(&home.backend, &op);
        write(&site.at("target/new"), b"made after the move");

        let refused = home
            .backend
            .restore(&request(&op, &slot), &CancelToken::new())
            .unwrap_err();

        assert_eq!(refused.code, ErrorCode::DestinationExists, "{refused:?}");
        assert_eq!(listing(&site.at("target")).len(), 1, "the new entry alone");
        assert!(present(&held(&slot)), "the item stays in the trash");
    }

    /// Restores `nested/target` after `change` did something to the directory holding
    /// it, and returns why the restore was refused. Fails if anything moved.
    fn restore_after(change: impl FnOnce(&Site)) -> ApplyError {
        let site = Site::new();
        let home = Home::new();
        nested(&site, "nested/target");
        let op = whole(&site.anchor("nested/target"));
        let slot = trashed(&home.backend, &op);
        change(&site);

        let refused = home
            .backend
            .restore(&request(&op, &slot), &CancelToken::new())
            .unwrap_err();

        assert!(present(&held(&slot)), "the item stays in the trash");
        assert!(
            !present(&site.outside.join("target")),
            "nothing went outside"
        );
        site.assert_outside_intact();
        refused
    }

    /// Catches a restore that follows a link put in place of the item's parent, that
    /// moves the item into another directory with the parent's name, or that recreates
    /// a parent that is gone (spec pinned 10).
    #[test]
    fn a_restore_needs_the_parent_it_came_from() {
        let through_link = restore_after(|site| {
            site.replace(&site.at("nested"), |path| link(&site.outside, path));
        });
        assert_eq!(
            through_link.code,
            ErrorCode::ThroughLink,
            "{through_link:?}"
        );

        let replaced = restore_after(|site| {
            site.replace(&site.at("nested"), |path| fs::create_dir(path).unwrap());
        });
        assert_eq!(replaced.code, ErrorCode::ParentChanged, "{replaced:?}");

        let missing = restore_after(|site| move_to(&site.at("nested"), &site.at("moved")));
        assert_eq!(missing.code, ErrorCode::ParentMissing, "{missing:?}");
    }

    /// Catches a restore that reports success, or moves something else back, when the
    /// trash entry was emptied or replaced (04 §5).
    #[test]
    fn an_emptied_or_replaced_trash_entry_is_reported() {
        let site = Site::new();
        let home = Home::new();
        nested(&site, "target");
        let op = whole(&site.anchor("target"));
        let top = op.target().tree().top().snapshot().unwrap();
        let slot = trashed(&home.backend, &op);
        empty(&held(&slot));

        assert_eq!(home.backend.find(&slot, &top), Ok(SlotState::Empty));
        let missing = home
            .backend
            .restore(&request(&op, &slot), &CancelToken::new())
            .unwrap_err();
        assert_eq!(missing.code, ErrorCode::TrashMissing, "{missing:?}");

        write(&held(&slot), b"something else");
        assert_eq!(home.backend.find(&slot, &top), Ok(SlotState::Other));
        let other = home
            .backend
            .restore(&request(&op, &slot), &CancelToken::new())
            .unwrap_err();
        assert_eq!(other.code, ErrorCode::TrashChanged, "{other:?}");
        assert!(!present(&site.at("target")), "nothing came back");

        empty(&home.files);
        assert_eq!(home.backend.find(&slot, &top), Ok(SlotState::Empty));
        let emptied = home
            .backend
            .restore(&request(&op, &slot), &CancelToken::new())
            .unwrap_err();
        assert_eq!(emptied.code, ErrorCode::TrashMissing, "{emptied:?}");
    }

    /// Moves `op` into a fresh home trash and returns why it was refused, after checking
    /// nothing moved and no `.trashinfo` was left.
    fn refused_move(site: &Site, op: &TrashOp) -> ApplyError {
        let home = Home::new();
        let slot = home.backend.destination(op).unwrap();
        let refused = home
            .backend
            .move_whole(op, &slot, &CancelToken::new())
            .unwrap_err();
        assert!(present(&site.at("target")), "the target stays");
        assert!(!present(&held(&slot)), "nothing is in the trash");
        if let Some(info) = info(&slot) {
            assert!(!present(&info), "no .trashinfo is left");
        }
        refused
    }

    /// Catches a move of an item with a file edited in place after its freeze: the
    /// whole move is refused before anything moves.
    #[test]
    fn an_item_edited_after_its_freeze_never_moves() {
        let site = Site::new();
        nested(&site, "target");
        let op = whole(&site.anchor("target"));
        write(&site.at("target/sub/b"), b"edited");

        let refused = refused_move(&site, &op);

        assert_eq!(refused.code, ErrorCode::TargetChanged, "{refused:?}");
        assert_eq!(refused.at, Some(RelPath::of(Path::new("sub/b")).unwrap()));
    }

    /// Catches a move of an item that gained an entry after its freeze: its directory
    /// changed, which refuses the move as the executor leaves such a directory.
    #[test]
    fn an_item_grown_after_its_freeze_never_moves() {
        let site = Site::new();
        nested(&site, "target");
        let op = whole(&site.anchor("target"));
        write(&site.at("target/sub/new"), b"new");

        let refused = refused_move(&site, &op);

        assert_eq!(refused.code, ErrorCode::TargetChanged, "{refused:?}");
    }

    /// `target/sub/b` in `tree`.
    fn sub_b(tree: &FrozenTree) -> NodeId {
        tree.find(&RelPath::of(Path::new("sub/b")).unwrap())
            .unwrap()
    }

    /// Catches a trash move carrying a child the plan left out (a file git tracks) into
    /// the trash with the rest (amendment J).
    #[test]
    fn an_item_with_an_excluded_child_never_moves() {
        let site = Site::new();
        nested(&site, "target");
        let anchor = site.anchor("target");
        let mut tree = freeze(&anchor).tree;
        tree.exclude(sub_b(&tree));
        let marks = Marks::of(&tree);

        let refused = refused_move(&site, &TrashOp::for_platform_tests(anchor, tree, marks));

        assert_eq!(refused.code, ErrorCode::Unauthorized, "{refused:?}");
    }

    /// Catches a trash move carrying a child revalidation dropped into the trash with
    /// the rest (amendment J).
    #[test]
    fn an_item_with_a_dropped_child_never_moves() {
        let site = Site::new();
        nested(&site, "target");
        let anchor = site.anchor("target");
        let tree = freeze(&anchor).tree;
        let mut marks = Marks::of(&tree);
        marks.drop_subtree(&tree, sub_b(&tree), LeftReason::Changed);

        let refused = refused_move(&site, &TrashOp::for_platform_tests(anchor, tree, marks));

        assert_eq!(refused.code, ErrorCode::Unauthorized, "{refused:?}");
    }

    /// Catches a root moved to the trash: a root's contents are never one item.
    #[test]
    fn a_root_never_moves() {
        let site = Site::new();
        let home = Home::new();
        write(&site.at("a"), b"a");
        let op = whole(&site.anchor(""));

        let refused = home.backend.destination(&op).unwrap_err();

        assert_eq!(refused.code, ErrorCode::Unauthorized, "{refused:?}");
        assert!(present(&site.at("a")), "the root's contents stay");
    }

    /// Catches a cancelled apply that still moves the item.
    #[test]
    fn a_cancelled_move_moves_nothing() {
        let site = Site::new();
        let home = Home::new();
        nested(&site, "target");
        let op = whole(&site.anchor("target"));
        let slot = home.backend.destination(&op).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();

        let refused = home.backend.move_whole(&op, &slot, &cancel).unwrap_err();

        assert_eq!(refused.code, ErrorCode::Cancelled, "{refused:?}");
        assert!(present(&site.at("target")), "the target stays");
    }

    /// Catches a trash reached through a link: what it leads to isn't the user's trash
    /// (spec pinned 9: not a link).
    #[test]
    fn a_trash_that_is_a_link_is_refused() {
        let site = Site::new();
        let home = Home::new();
        nested(&site, "target");
        let elsewhere = TempDir::new().unwrap();
        let linked = elsewhere.path().join("linked");
        // On Linux the trash is `…/Trash/files`, and its `Trash` is the link.
        let files = if cfg!(target_os = "linux") {
            link(home.files.parent().unwrap(), &linked);
            linked.join("files")
        } else {
            link(&home.files, &linked);
            linked
        };
        let slot = TrashSlot {
            trash: TrashDir {
                path: files.clone(),
                identity: identity(&home.files),
            },
            name: kipple_core::Component::new("target").unwrap(),
        };

        let refused = backend(Some(files))
            .move_whole(&whole(&site.anchor("target")), &slot, &CancelToken::new())
            .unwrap_err();

        assert_eq!(refused.code, ErrorCode::ThroughLink, "{refused:?}");
        assert!(present(&site.at("target/a")), "the target stays");
    }

    /// The directory the variable `name` names, or `None` to skip where CI doesn't
    /// require it.
    fn prepared(name: &str) -> Option<PathBuf> {
        let path = std::env::var_os(name).map(PathBuf::from);
        assert!(
            path.is_some() || std::env::var_os("KIPPLE_TEST_MOUNTS_REQUIRED").is_none(),
            "{name} must name a prepared mount"
        );
        path
    }

    /// An item `target/` with a file in it, in a fresh directory on `volume`, and its
    /// anchor there.
    fn item_on(volume: &Path) -> (TempDir, Anchor) {
        let temp = TempDir::new_in(volume).unwrap();
        write(&temp.path().join("target/x"), b"on the other volume");
        let anchor = anchor_at(temp.path(), "target");
        (temp, anchor)
    }

    /// Catches a move into a trash on another file system, which the OS can only do by
    /// copying and deleting (spec pinned 9: refused, never copied).
    #[test]
    fn a_move_to_another_file_system_is_refused() {
        let Some(holder) = prepared("KIPPLE_TEST_OTHER_MOUNT") else {
            return;
        };
        let home = Home::new();
        let (temp, anchor) = item_on(&holder.join("other"));
        let op = whole(&anchor);
        let slot = TrashSlot {
            trash: TrashDir {
                path: home.files.clone(),
                identity: identity(&home.files),
            },
            name: kipple_core::Component::new("target").unwrap(),
        };

        let refused = home
            .backend
            .move_whole(&op, &slot, &CancelToken::new())
            .unwrap_err();

        assert_eq!(refused.code, ErrorCode::CrossFileSystem, "{refused:?}");
        assert!(present(&temp.path().join("target/x")), "the item stays");
        assert!(!present(&held(&slot)), "nothing is in the trash");
    }

    /// Catches an item on another mount sent to the home trash, or refused when that
    /// mount has a freedesktop trash at its top (`$topdir/.Trash-$uid`).
    #[cfg(target_os = "linux")]
    #[test]
    fn an_item_on_another_mount_goes_to_that_mounts_trash() {
        let Some(holder) = prepared("KIPPLE_TEST_OTHER_MOUNT") else {
            return;
        };
        let top = holder.join("other");
        let home = Home::new();
        let (temp, anchor) = item_on(&top);
        let uid = {
            use std::os::unix::fs::MetadataExt as _;
            fs::metadata(temp.path()).unwrap().uid()
        };
        for dir in ["files", "info"] {
            fs::create_dir_all(top.join(format!(".Trash-{uid}")).join(dir)).unwrap();
        }
        let op = whole(&anchor);

        let slot = trashed(&home.backend, &op);

        assert!(
            slot.trash.path.ends_with(format!(".Trash-{uid}/files")),
            "the mount's own trash takes it: {slot:?}"
        );
        assert!(!present(&temp.path().join("target")), "the target left");
        assert!(present(&info(&slot).unwrap()), "its .trashinfo is written");
        home.backend
            .restore(&request(&op, &slot), &CancelToken::new())
            .unwrap();
        assert!(present(&temp.path().join("target/x")), "the item is back");
    }

    /// Catches a trash move on FAT, which can't hold what kipple checks (spec pinned 6).
    #[test]
    fn nothing_moves_on_fat() {
        let Some(volume) = prepared("KIPPLE_TEST_FAT") else {
            return;
        };
        let home = Home::new();
        let (temp, anchor) = item_on(&volume);

        let refused = home.backend.available(&anchor).unwrap_err();

        assert_eq!(
            refused.code,
            ErrorCode::UnsupportedFileSystem,
            "{refused:?}"
        );
        assert!(present(&temp.path().join("target/x")), "the item stays");
    }

    /// Catches the real trash refusing kipple's move, or the move not being restorable
    /// there: on Linux the account's freedesktop home trash, on macOS `~/.Trash`, which
    /// macOS may guard. Only on a disposable host: it writes to the account's trash.
    #[test]
    fn an_item_moves_through_the_accounts_own_trash() {
        if std::env::var_os("KIPPLE_TEST_DISPOSABLE_HOST").is_none() {
            return;
        }
        let vars: Vec<_> = std::env::vars_os().collect();
        let roots =
            kipple_platform::resolve_roots(&kipple_platform::Environment::from_vars(vars), &[]);
        let Some(Ok(trash)) = roots.get(kipple_core::SymbolicRoot::Trash) else {
            panic!("the trash did not resolve");
        };
        let Some(Ok(home)) = roots.get(kipple_core::SymbolicRoot::Home) else {
            panic!("home did not resolve");
        };
        fs::create_dir_all(&trash.path).unwrap();
        if cfg!(target_os = "linux") {
            fs::create_dir_all(trash.path.parent().unwrap().join("info")).unwrap();
        }
        let backend = backend(Some(trash.path.clone()));
        let (temp, anchor) = item_on(&home.path);
        let op = whole(&anchor);

        let slot = trashed(&backend, &op);

        assert!(!present(&temp.path().join("target")), "the target left");
        backend
            .restore(&request(&op, &slot), &CancelToken::new())
            .unwrap();
        assert!(present(&temp.path().join("target/x")), "the item is back");
    }
}

/// Catches trash offered on Windows, where the Recycle Bin can delete permanently
/// instead (spec pinned 9): every operation is refused and nothing moves.
#[cfg(windows)]
#[test]
fn the_recycle_bin_is_never_used() {
    use std::sync::Arc;

    use kipple_core::{ErrorCode, Marks, TrashBackend as _, TrashOp};
    use kipple_platform::{ConfinedTrash, SystemClock};

    use crate::executor::support::{Site, freeze, present, write};

    let site = Site::new();
    write(&site.at("target/x"), b"x");
    let anchor = site.anchor("target");
    let backend = ConfinedTrash::new(None, Arc::new(SystemClock));

    let refused = backend.available(&anchor).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Unsupported);
    let tree = freeze(&anchor).tree;
    let marks = Marks::of(&tree);
    let op = TrashOp::for_platform_tests(anchor, tree, marks);
    assert_eq!(
        backend.destination(&op).unwrap_err().code,
        ErrorCode::Unsupported
    );
    assert!(present(&site.at("target/x")), "nothing moved");
}
