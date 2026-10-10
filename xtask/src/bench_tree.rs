//! `cargo xtask bench-tree`: the reference trees for the walker and scan benchmarks
//! (`docs/11-delivery-plan.md`, S3 procedure).
//!
//! The tree is a pure function of the entry count. The seed, depth and fan-out are fixed,
//! files are written in a fixed order, and no clock or host state is read, so every
//! machine builds the same tree. Most entries sit in `node_modules`-like and `target`-like
//! hot spots. Every project also has a hidden directory and a `.gitignore` that lists the
//! hot spots, so a walker that honours ignore files or skips hidden entries undercounts.

use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Fixed seed for file sizes, so the same entry count always gives the same tree.
const SEED: u64 = 0x6b69_7070_6c65;

/// A project's `src/` tree: this many levels below `src/`, each directory with this many
/// subdirectories and files.
const SRC_DEPTH: u32 = 3;
const SRC_DIRS: u32 = 3;
const SRC_FILES: u32 = 6;

/// A `node_modules` hot spot: top-level packages, each with a nested `node_modules` of
/// `NESTED_PACKAGES` packages, down to `PACKAGE_DEPTH` levels of nesting.
const TOP_PACKAGES: u32 = 40;
const NESTED_PACKAGES: u32 = 2;
const PACKAGE_DEPTH: u32 = 2;
const PACKAGE_LIB_FILES: u32 = 8;

/// A `target` hot spot: Cargo-like build output.
const TARGET_DEPS_FILES: u32 = 300;
const TARGET_BUILD_DIRS: u32 = 20;
const TARGET_INCREMENTAL_DIRS: u32 = 10;

/// File sizes are drawn from three bands: most files are small, a few are large.
const SMALL_FILE: u64 = 1024;
const MEDIUM_FILE: u64 = 8 * 1024;
const LARGE_FILE: u64 = 64 * 1024;

const GITIGNORE: &[u8] = b"node_modules/\ntarget/\n.cache/\n";

/// What was written.
#[derive(Debug, Default)]
struct Summary {
    dirs: u64,
    files: u64,
    bytes: u64,
}

/// Builds the reference tree with exactly `entries` files and directories inside `dir`,
/// which must be missing or empty.
pub(crate) fn run(dir: &Path, entries: u64) -> Result<()> {
    let summary = generate(dir, entries)?;
    writeln!(
        io::stderr(),
        "xtask: wrote {} entries ({} directories, {} files, {} bytes) to {}",
        summary.dirs + summary.files,
        summary.dirs,
        summary.files,
        summary.bytes,
        dir.display()
    )?;
    Ok(())
}

fn generate(dir: &Path, entries: u64) -> Result<Summary> {
    ensure_empty(dir)?;
    let mut tree = Tree::new(entries);
    let mut index = 0;
    while tree.remaining > 0 {
        tree.project(dir, index)?;
        index += 1;
    }
    Ok(tree.summary)
}

/// The tree is written into `dir`, so refuse anything that already has contents.
fn ensure_empty(dir: &Path) -> Result<()> {
    match fs::read_dir(dir) {
        Ok(mut contents) => {
            if contents.next().is_some() {
                bail!(
                    "{} is not empty; bench-tree only writes into an empty or missing directory",
                    dir.display()
                );
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))
        }
        Err(error) => Err(error).with_context(|| format!("could not read {}", dir.display())),
    }
}

struct Tree {
    rng: SplitMix64,
    /// Entries still to write. Once it reaches zero every write is skipped.
    remaining: u64,
    /// File contents are slices of these random bytes, so a compressing file system
    /// still allocates space for them.
    content: Vec<u8>,
    summary: Summary,
}

impl Tree {
    fn new(entries: u64) -> Self {
        let mut rng = SplitMix64(SEED);
        let content = (0..LARGE_FILE)
            .map(|_| rng.next().to_le_bytes()[0])
            .collect();
        Self {
            rng,
            remaining: entries,
            content,
            summary: Summary::default(),
        }
    }

    /// Every second project has a `node_modules` hot spot, every fourth a `target` one.
    fn project(&mut self, root: &Path, index: u32) -> Result<()> {
        let path = &root.join(format!("project-{index:04}"));
        self.dir(path)?;
        self.gitignore(&path.join(".gitignore"))?;
        self.src(&path.join("src"), SRC_DEPTH)?;
        let cache = path.join(".cache");
        self.dir(&cache)?;
        self.files(&cache, "entry", "bin", 4)?;
        if index % 4 == 3 {
            self.target(&path.join("target"))?;
        }
        if index % 2 == 1 {
            self.node_modules(&path.join("node_modules"), 0, TOP_PACKAGES)?;
        }
        Ok(())
    }

    fn src(&mut self, path: &Path, depth: u32) -> Result<()> {
        self.dir(path)?;
        self.files(path, "module", "rs", SRC_FILES)?;
        if depth == 0 {
            return Ok(());
        }
        for index in 0..SRC_DIRS {
            self.src(&path.join(format!("dir-{index}")), depth - 1)?;
        }
        Ok(())
    }

    fn node_modules(&mut self, path: &Path, depth: u32, packages: u32) -> Result<()> {
        self.dir(path)?;
        for index in 0..packages {
            self.package(&path.join(format!("pkg-{index:03}")), depth)?;
        }
        Ok(())
    }

    fn package(&mut self, path: &Path, depth: u32) -> Result<()> {
        self.dir(path)?;
        self.file(&path.join("package.json"))?;
        self.file(&path.join("index.js"))?;
        self.file(&path.join("README.md"))?;
        let lib = path.join("lib");
        self.dir(&lib)?;
        self.files(&lib, "chunk", "js", PACKAGE_LIB_FILES)?;
        if depth < PACKAGE_DEPTH {
            self.node_modules(&path.join("node_modules"), depth + 1, NESTED_PACKAGES)?;
        }
        Ok(())
    }

    fn target(&mut self, path: &Path) -> Result<()> {
        let debug = path.join("debug");
        self.dir(path)?;
        self.dir(&debug)?;
        let deps = debug.join("deps");
        self.dir(&deps)?;
        self.files(&deps, "lib", "rlib", TARGET_DEPS_FILES)?;
        for (name, count, files) in [
            ("build", TARGET_BUILD_DIRS, 5),
            ("incremental", TARGET_INCREMENTAL_DIRS, 10),
        ] {
            let parent = debug.join(name);
            self.dir(&parent)?;
            for index in 0..count {
                let unit = parent.join(format!("unit-{index:02}"));
                self.dir(&unit)?;
                self.files(&unit, "out", "o", files)?;
            }
        }
        Ok(())
    }

    fn files(&mut self, dir: &Path, stem: &str, extension: &str, count: u32) -> Result<()> {
        for index in 0..count {
            self.file(&dir.join(format!("{stem}-{index:03}.{extension}")))?;
        }
        Ok(())
    }

    fn dir(&mut self, path: &Path) -> Result<()> {
        if !self.take_entry() {
            return Ok(());
        }
        fs::create_dir(path).with_context(|| format!("could not create {}", path.display()))?;
        self.summary.dirs += 1;
        Ok(())
    }

    fn file(&mut self, path: &Path) -> Result<()> {
        let len = usize::try_from(self.file_size()).context("file size does not fit in memory")?;
        if !self.take_entry() {
            return Ok(());
        }
        write_file(path, self.content.get(..len).unwrap_or(&self.content))?;
        self.summary.record_file(len);
        Ok(())
    }

    fn gitignore(&mut self, path: &Path) -> Result<()> {
        if !self.take_entry() {
            return Ok(());
        }
        write_file(path, GITIGNORE)?;
        self.summary.record_file(GITIGNORE.len());
        Ok(())
    }

    const fn take_entry(&mut self) -> bool {
        let Some(remaining) = self.remaining.checked_sub(1) else {
            return false;
        };
        self.remaining = remaining;
        true
    }

    /// 85% of files are under 1 KiB, 14% under 8 KiB and 1% under 64 KiB.
    const fn file_size(&mut self) -> u64 {
        let band = match self.rng.below(100) {
            0..85 => SMALL_FILE,
            85..99 => MEDIUM_FILE,
            _ => LARGE_FILE,
        };
        self.rng.below(band)
    }
}

impl Summary {
    const fn record_file(&mut self, len: usize) {
        self.files += 1;
        self.bytes += len as u64;
    }
}

fn write_file(path: &Path, content: &[u8]) -> Result<()> {
    fs::write(path, content).with_context(|| format!("could not write {}", path.display()))
}

/// `SplitMix64`: a tiny, well-known generator. It only needs to be fixed, not strong.
struct SplitMix64(u64);

impl SplitMix64 {
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const ENTRIES: u64 = 20_000;

    /// Every entry under `root` as (relative path, size or `None` for a directory), sorted.
    fn listing(root: &Path) -> Vec<(PathBuf, Option<u64>)> {
        let mut entries = Vec::new();
        list_into(root, root, &mut entries);
        entries.sort();
        entries
    }

    fn list_into(root: &Path, dir: &Path, entries: &mut Vec<(PathBuf, Option<u64>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if meta.is_dir() {
                entries.push((relative, None));
                list_into(root, &path, entries);
            } else {
                entries.push((relative, Some(meta.len())));
            }
        }
    }

    #[test]
    fn the_same_entry_count_always_builds_the_same_tree() {
        let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let summary = generate(first.path(), ENTRIES).unwrap();
        generate(second.path(), ENTRIES).unwrap();

        let listed = listing(first.path());
        assert_eq!(listed, listing(second.path()));
        assert_eq!(listed.len() as u64, ENTRIES);
        assert_eq!(summary.dirs + summary.files, ENTRIES);
    }

    #[test]
    fn most_entries_sit_in_hot_spots_behind_ignore_files_and_hidden_dirs() {
        let dir = tempfile::tempdir().unwrap();
        generate(dir.path(), ENTRIES).unwrap();
        let listed = listing(dir.path());

        let in_dir = |name: &str| {
            listed
                .iter()
                .filter(|(path, _)| path.components().any(|c| c.as_os_str() == name))
                .count()
        };
        let hot = in_dir("node_modules") + in_dir("target");
        assert!(
            hot * 2 > listed.len(),
            "only {hot} of {} entries are in hot spots",
            listed.len()
        );
        assert!(in_dir("target") > 0);
        assert!(in_dir(".cache") > 0);
        assert!(listed.iter().any(|(path, _)| path.ends_with(".gitignore")));
    }

    #[test]
    fn refuses_to_write_into_a_directory_that_has_contents() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("keep.txt"), b"user data").unwrap();

        assert!(generate(dir.path(), 10).is_err());
        assert_eq!(listing(dir.path()).len(), 1);
    }
}
