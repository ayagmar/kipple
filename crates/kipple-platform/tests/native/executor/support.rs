//! A disposable site for executor tests: a verified root, a target below it and an
//! outside area of sentinels, all inside one temp directory.
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]

use std::fs;
use std::path::{Path, PathBuf};

use kipple_core::{
    Anchor, CancelToken, DeleteOp, EffectSink, Executor as _, FileIdentity, Freed, Freeze,
    Freezer as _, FrozenTree, FsProbe as _, ItemOutcome, LeftReason, Marks, NodeId, RelPath,
    Removed, SymbolicRoot, Tally, VerifiedRoot,
};
use kipple_platform::{ConfinedFs, NoFollowFs};
use tempfile::TempDir;

/// What every site's outside area holds, by path below it, and what each file says.
const SENTINELS: [(&str, &[u8]); 3] = [
    ("keep", b"outside the root"),
    ("dir/keep", b"in a directory outside"),
    ("dir/deeper/keep", b"further down outside"),
];

/// A temp directory holding `root/`, the verified root, and `outside/`, sentinels that
/// nothing may touch.
#[derive(Debug)]
pub(crate) struct Site {
    temp: TempDir,
    pub(crate) root: PathBuf,
    pub(crate) outside: PathBuf,
}

impl Site {
    pub(crate) fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        fs::create_dir(&root).unwrap();
        for (path, contents) in SENTINELS {
            write(&outside.join(path), contents);
        }
        Self {
            temp,
            root,
            outside,
        }
    }

    /// Swaps whatever is at `path` for what `with` makes there, as a racing process
    /// would between freeze and apply. The original is moved aside first, so the new
    /// entry can't reuse its inode.
    pub(crate) fn replace(&self, path: &Path, with: impl FnOnce(&Path)) {
        let aside = self.temp.path().join("displaced");
        fs::create_dir_all(&aside).unwrap();
        let count = fs::read_dir(&aside).unwrap().count();
        move_to(path, &aside.join(count.to_string()));
        with(path);
    }

    /// `relative` below the root.
    pub(crate) fn at(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// The anchor of `relative` below the root, `""` for the root's contents.
    pub(crate) fn anchor(&self, relative: &str) -> Anchor {
        anchor_at(&self.root, relative)
    }

    /// Fails unless every sentinel outside is still there, unchanged.
    pub(crate) fn assert_outside_intact(&self) {
        for (path, contents) in SENTINELS {
            assert_eq!(
                fs::read(self.outside.join(path)).ok().as_deref(),
                Some(contents),
                "the sentinel {path} outside the root is intact"
            );
        }
    }
}

/// The identity a scan records for `path`.
pub(crate) fn identity(path: &Path) -> FileIdentity {
    NoFollowFs.metadata(path).unwrap().identity.unwrap()
}

/// The anchor of `relative` below the root `root`, with the identities a scan and a
/// planner record.
pub(crate) fn anchor_at(root: &Path, relative: &str) -> Anchor {
    let relative = RelPath::of(Path::new(relative)).unwrap();
    let parent = relative
        .components()
        .split_last()
        .map(|(_, way)| identity(&RelPath::new(way.to_vec()).below(root)));
    Anchor {
        root: VerifiedRoot {
            root: SymbolicRoot::Home,
            path: root.to_owned(),
            identity: identity(root),
        },
        relative,
        parent,
    }
}

pub(crate) fn freeze(anchor: &Anchor) -> Freeze {
    ConfinedFs.freeze(anchor, &CancelToken::new()).unwrap()
}

/// Deletes every entry of `tree` at `anchor`.
pub(crate) fn delete(anchor: &Anchor, tree: FrozenTree) -> ItemOutcome {
    let marks = Marks::of(&tree);
    run(&DeleteOp::for_platform_tests(anchor.clone(), tree, marks))
}

/// Runs `op` the way the apply loop does, into a [`Tally`].
pub(crate) fn run(op: &DeleteOp) -> ItemOutcome {
    let target = op.target();
    let mut tally = Tally::new(target.tree(), target.marks());
    let ran = ConfinedFs.delete(op, &mut tally, &CancelToken::new());
    tally.finish(ran)
}

/// Runs `op` into a [`Tally`] through `sink`, which sees each report first.
pub(crate) fn run_through(
    op: &DeleteOp,
    cancel: &CancelToken,
    sink: impl FnMut(Report, &CancelToken),
) -> ItemOutcome {
    let target = op.target();
    let mut tally = Tally::new(target.tree(), target.marks());
    let mut watched = Watched {
        tally: &mut tally,
        cancel,
        sink,
    };
    let ran = ConfinedFs.delete(op, &mut watched, cancel);
    tally.finish(ran)
}

/// One report the executor made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Report {
    Removed(NodeId, Freed),
    Left(NodeId, LeftReason),
}

struct Watched<'tally, 'tree, F> {
    tally: &'tally mut Tally<'tree>,
    cancel: &'tally CancelToken,
    sink: F,
}

impl<F: FnMut(Report, &CancelToken)> EffectSink for Watched<'_, '_, F> {
    fn removed(&mut self, id: NodeId, freed: Freed) {
        (self.sink)(Report::Removed(id, freed), self.cancel);
        self.tally.removed(id, freed);
    }

    fn left(&mut self, id: NodeId, reason: LeftReason) {
        (self.sink)(Report::Left(id, reason), self.cancel);
        self.tally.left(id, reason);
    }
}

/// How many reviewed entries `outcome` left for `reason`.
pub(crate) fn left(outcome: &ItemOutcome, reason: LeftReason) -> u64 {
    outcome
        .effect()
        .and_then(|effect| effect.left.0.get(&reason))
        .map_or(0, |left| left.count)
}

/// What `outcome` removed.
pub(crate) fn removed(outcome: &ItemOutcome) -> Removed {
    outcome
        .effect()
        .map(|effect| effect.removed)
        .unwrap_or_default()
}

pub(crate) fn write(path: &Path, contents: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Whether anything, even a dangling link, has the name `path`.
pub(crate) fn present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Whether `path` is a link: a symbolic link, or on Windows a junction too.
pub(crate) fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
            meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        }
        #[cfg(unix)]
        meta.file_type().is_symlink()
    })
}

/// A link at `link` to `target`: a symbolic link on Unix; on Windows a junction for a
/// directory, which any user can make, and a symbolic link for a file.
pub(crate) fn link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    if target.is_dir() {
        junction(target, link);
    } else {
        std::os::windows::fs::symlink_file(target, link).unwrap();
    }
}

/// Moves `from` to `to`, as a racing process would.
#[expect(
    clippy::disallowed_methods,
    reason = "the test plays a process that changes the disk under kipple"
)]
pub(crate) fn move_to(from: &Path, to: &Path) {
    fs::rename(from, to).unwrap();
}

/// Lets the temp directory be cleaned up after a test that took permissions away.
#[cfg(unix)]
pub(crate) fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// A name the OS holds that isn't valid Unicode: invalid UTF-8 on Linux, an unpaired
/// surrogate on Windows. macOS file systems refuse such names.
#[cfg(any(target_os = "linux", windows))]
pub(crate) fn ill_formed(prefix: &str) -> std::ffi::OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let mut bytes = prefix.as_bytes().to_vec();
        bytes.extend_from_slice(b"-\xff\xfe");
        std::ffi::OsStr::from_bytes(&bytes).to_owned()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
        let mut wide: Vec<u16> = std::ffi::OsStr::new(prefix).encode_wide().collect();
        wide.extend([u16::from(b'-'), 0xD800]);
        std::ffi::OsString::from_wide(&wide)
    }
}

/// One entry a [`listing`] found.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Listed {
    path: PathBuf,
    /// What it holds, for a file.
    contents: Option<Vec<u8>>,
}

/// Every path below `dir`, without following links, with what each file holds.
pub(crate) fn listing(dir: &Path) -> Vec<Listed> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = fs::read_dir(&next) else {
            continue;
        };
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            let contents = kind.is_file().then(|| fs::read(&path).unwrap());
            if kind.is_dir() {
                stack.push(path.clone());
            }
            found.push(Listed { path, contents });
        }
    }
    found.sort();
    found
}

/// A junction at `link` to the directory `target`, made with `FSCTL_SET_REPARSE_POINT`
/// as `mklink /J` does.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "windows-sys exposes DeviceIoControl as a raw call"
)]
fn junction(target: &Path, link: &Path) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
    use windows_sys::Win32::System::SystemServices::IO_REPARSE_TAG_MOUNT_POINT;

    fs::create_dir(link).unwrap();
    let dir = fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(link)
        .unwrap();
    let wide = |text: &OsStr| -> Vec<u8> {
        text.encode_wide()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    };
    let mut substitute = OsStr::new(r"\??\").to_owned();
    substitute.push(target.as_os_str());
    let (substitute, print) = (wide(&substitute), wide(target.as_os_str()));
    let length = |bytes: &[u8]| u16::try_from(bytes.len() - 2).unwrap();
    let mut names = Vec::new();
    for value in [
        0,
        length(&substitute),
        length(&substitute) + 2,
        length(&print),
    ] {
        names.extend_from_slice(&value.to_le_bytes());
    }
    names.extend_from_slice(&substitute);
    names.extend_from_slice(&print);
    let mut buffer = IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes().to_vec();
    buffer.extend_from_slice(&u16::try_from(names.len()).unwrap().to_le_bytes());
    buffer.extend_from_slice(&[0, 0]);
    buffer.extend_from_slice(&names);
    let mut returned = 0;
    // SAFETY: the handle is open for writing and the buffer is a mount-point reparse
    // buffer of its own length.
    let ok = unsafe {
        DeviceIoControl(
            dir.as_raw_handle(),
            FSCTL_SET_REPARSE_POINT,
            buffer.as_ptr().cast(),
            u32::try_from(buffer.len()).unwrap(),
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
}
