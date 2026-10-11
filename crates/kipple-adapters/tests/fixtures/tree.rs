//! `FixtureTree`: builds an upstream layout in a directory from test code
//! (`docs/06-adapters-and-rules.md` §7). Shared by the integration tests and
//! `cargo xtask demo-home`.

use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

/// Directories and files to create, relative to the directory the tree is built in.
#[derive(Debug, Default)]
pub(crate) struct FixtureTree {
    dirs: Vec<String>,
    files: Vec<(String, Content)>,
    modified: Vec<(String, SystemTime)>,
}

#[derive(Debug)]
enum Content {
    Bytes(Vec<u8>),
    /// A sparse file of this length, so large sizes cost no disk.
    Sized(u64),
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

    /// Every file below `dir` was last modified `at`, which overrides earlier calls for the
    /// same files. Directories keep the time they were created.
    pub(crate) fn modified(&mut self, dir: &str, at: SystemTime) -> &mut Self {
        self.modified.push((dir.to_owned(), at));
        self
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
        Ok(())
    }
}
