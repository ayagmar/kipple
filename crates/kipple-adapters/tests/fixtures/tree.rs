//! `FixtureTree`: builds an upstream layout in a directory from test code
//! (`docs/06-adapters-and-rules.md` §7). Shared by the integration tests and
//! `cargo xtask demo-home`.

use std::fs;
use std::io;
use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};
use std::time::SystemTime;

// Only the adapters' own tests call these so far, and the other crates that include this
// file would fail `dead_code` on them. Naming them here keeps the lint off them in one
// place; drop a name once `demo_home` reaches it.
const _: () = {
    let _ = FixtureTree::link;
    let _ = Holds::AsWritten;
    let _ = FixtureTree::bad_link;
    let _ = FixtureTree::modified_dir;
    let _ = FixtureTree::link_stamp;
};

/// Directories, files and links to create, relative to the directory the tree is built
/// in.
#[derive(Debug, Default)]
pub(crate) struct FixtureTree {
    dirs: Vec<String>,
    files: Vec<(String, Content)>,
    links: Vec<(String, String, Holds)>,
    modified: Vec<(String, SystemTime)>,
    modified_dirs: Vec<(String, SystemTime)>,
}

#[derive(Debug)]
enum Content {
    Bytes(Vec<u8>),
    /// A sparse file of this length, so large sizes cost no disk.
    Sized(u64),
}

/// What a link holds of the target it's given, which is the upstream layout's to say: an
/// integration can tell the forms apart, so a fixture has to state the one upstream writes.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Holds {
    /// The target is a path relative to the directory the tree is built in, like a link's
    /// own path, and the link holds it as an absolute path, as Codex's `current` does.
    AbsoluteInTree,
    /// The link holds the target as written, relative to the link (`bin/codex`,
    /// `../../.pi/agent/bin/pi`) or absolute, in the tree or not. `/` separates its parts
    /// on every system.
    AsWritten,
}

impl FixtureTree {
    /// An empty tree.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A directory, with its parents.
    pub(crate) fn dir(&mut self, path: &str) -> &mut Self {
        self.dirs.push(path.to_owned());
        self
    }

    /// A file holding `contents`, with its parent directories.
    pub(crate) fn file(&mut self, path: &str, contents: &[u8]) -> &mut Self {
        let content = Content::Bytes(contents.to_vec());
        self.files.push((path.to_owned(), content));
        self
    }

    /// A sparse file of `len` bytes, with its parent directories.
    pub(crate) fn sized(&mut self, path: &str, len: u64) -> &mut Self {
        self.files.push((path.to_owned(), Content::Sized(len)));
        self
    }

    /// A symlink at `path`, with its parent directories, to `target`, which it holds as
    /// `holds` says. Links are made after every directory and file, in the order added, so
    /// a link to a directory through another link comes after it.
    pub(crate) fn link(&mut self, path: &str, target: &str, holds: Holds) -> &mut Self {
        self.links.push((path.to_owned(), target.to_owned(), holds));
        self
    }

    /// A symlink at `path`, with its parent directories, whose target doesn't exist.
    pub(crate) fn bad_link(&mut self, path: &str) -> &mut Self {
        self.link(path, &format!("{path}.missing"), Holds::AbsoluteInTree)
    }

    /// Every file below `dir` was last modified `at`, which overrides earlier calls for the
    /// same files. Directories keep the time they were created, unless [`Self::modified_dir`]
    /// sets one.
    pub(crate) fn modified(&mut self, dir: &str, at: SystemTime) -> &mut Self {
        self.modified.push((dir.to_owned(), at));
        self
    }

    /// The directory `path`, which the tree must hold, was last modified `at`. Set after
    /// everything else is built, so writing its children doesn't overwrite it.
    pub(crate) fn modified_dir(&mut self, path: &str, at: SystemTime) -> &mut Self {
        self.modified_dirs.push((path.to_owned(), at));
        self
    }

    /// What `lstat` reports for the built link at `path`, as Codex's `selection-history`
    /// records it: `<st_dev> <st_ino> <ctime_sec>.<ctime_nsec>`. A change time can't be set,
    /// so a fixture reads this once the link is in its final place and writes the file
    /// from it. Anything but a link is an error.
    #[cfg(unix)]
    pub(crate) fn link_stamp(path: &Path) -> io::Result<String> {
        use std::os::unix::fs::MetadataExt as _;

        let meta = fs::symlink_metadata(path)?;
        if !meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} isn't a link", path.display()),
            ));
        }
        let (device, inode) = (meta.dev(), meta.ino());
        let (seconds, nanos) = (meta.ctime(), meta.ctime_nsec());
        Ok(format!("{device} {inode} {seconds}.{nanos:09}"))
    }

    /// Windows has no such stamp: Codex records a USN there, which `std` can't read.
    #[cfg(windows)]
    pub(crate) fn link_stamp(_: &Path) -> io::Result<String> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Creates everything inside `root`, which must exist.
    pub(crate) fn build_in(&self, root: &Path) -> io::Result<()> {
        for dir in &self.dirs {
            fs::create_dir_all(root.join(dir))?;
        }
        for (path, content) in &self.files {
            let path = root.join(path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            match content {
                Content::Bytes(bytes) => fs::write(&path, bytes)?,
                Content::Sized(len) => fs::File::create(&path)?.set_len(*len)?,
            }
        }
        for (path, target, holds) in &self.links {
            let path = root.join(path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let target = match holds {
                Holds::AbsoluteInTree => below(root, target),
                Holds::AsWritten => PathBuf::from(target.replace('/', MAIN_SEPARATOR_STR)),
            };
            symlink(&target, &path)?;
        }
        for (dir, at) in &self.modified {
            let below = self
                .files
                .iter()
                .filter(|(path, _)| Path::new(path).starts_with(dir));
            for (path, _) in below {
                let file = fs::File::options().write(true).open(root.join(path))?;
                file.set_modified(*at)?;
            }
        }
        for (dir, at) in &self.modified_dirs {
            open_dir(&root.join(dir))?.set_modified(*at)?;
        }
        Ok(())
    }
}

/// `path` below `root`, part by part: a link keeps its target as given, and Windows
/// doesn't read a `/` in one.
fn below(root: &Path, path: &str) -> PathBuf {
    path.split('/')
        .fold(root.to_path_buf(), |at, part| at.join(part))
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// A link of the kind `target` needs, which a relative one finds from the link's own
/// directory, as the system will. One to nothing is a file link.
#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    let resolved = link.parent().unwrap_or(link).join(target);
    if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// The directory `path`, open so its times can be set.
#[cfg(unix)]
fn open_dir(path: &Path) -> io::Result<fs::File> {
    fs::File::open(path)
}

/// The directory `path`, open so its times can be set. Windows opens a directory only
/// with backup semantics; the tools that build this have no `windows-sys` to name the
/// constants.
#[cfg(windows)]
fn open_dir(path: &Path) -> io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    fs::File::options()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}
