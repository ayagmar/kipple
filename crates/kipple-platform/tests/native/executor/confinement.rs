//! Confinement as a property (04 §10; the M3 verify line asks for 10k cases): over
//! generated trees full of links and hard links that lead out of the target, and
//! generated changes between freeze and apply, a delete removes only what it froze,
//! never what a link leads to, nothing outside the target, nothing created after the
//! freeze and nothing changed since it.
//!
//! Each case is seeded by its number, so a failure names the case that reproduces it.
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]

use std::path::{Path, PathBuf};

use kipple_core::{CancelToken, DeleteOp, Marks, NodeId, Status};

use super::support::{Report, Site, freeze, link, listing, present, run_through, write};

/// How many cases run.
const CASES: u64 = 10_000;

/// How deep the generated trees go below the target.
const DEPTH: usize = 3;

/// A small, fast, deterministic generator: `SplitMix64`.
struct SplitMix(u64);

impl SplitMix {
    const fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % bound as u64).unwrap_or(0)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            return None;
        }
        items.get(self.below(items.len()))
    }
}

/// One generated case: the target, a sibling of it inside the root, and what the
/// changes after the freeze made that must survive.
struct Case {
    site: Site,
    target: PathBuf,
    elsewhere: PathBuf,
    /// Frozen regular files with one name, which a change may edit or replace.
    files: Vec<PathBuf>,
    /// Frozen directories below the target, with their depth.
    dirs: Vec<(PathBuf, usize)>,
    /// Directories replaced since the freeze: nothing below them is looked at.
    replaced: Vec<PathBuf>,
    /// What must still be there after the delete.
    survivors: Vec<PathBuf>,
    changes: usize,
}

impl Case {
    fn new(rng: &mut SplitMix) -> Self {
        let site = Site::new();
        let target = site.at("target");
        let elsewhere = site.at("elsewhere");
        write(&elsewhere.join("f"), b"inside the root, outside the target");
        write(&elsewhere.join("sub/g"), b"further in");
        std::fs::create_dir(&target).unwrap();
        let mut case = Self {
            site,
            target: target.clone(),
            elsewhere,
            files: Vec::new(),
            dirs: vec![(target, 0)],
            replaced: Vec::new(),
            survivors: Vec::new(),
            changes: 0,
        };
        for index in 0..=rng.below(12) {
            case.add(rng, index);
        }
        case.dirs.remove(0);
        case
    }

    /// Adds entry `index` somewhere in the tree: a file, a directory, a link to
    /// something outside the target, or a second name of a file outside it.
    fn add(&mut self, rng: &mut SplitMix, index: usize) {
        let roomy: Vec<_> = self
            .dirs
            .iter()
            .filter(|(_, depth)| *depth < DEPTH)
            .collect();
        let Some((parent, depth)) = rng.pick(&roomy).map(|(path, depth)| (path.clone(), *depth))
        else {
            return;
        };
        let path = parent.join(format!("e{index}"));
        match rng.below(6) {
            0 | 1 => {
                write(&path, &vec![b'x'; rng.below(64)]);
                self.files.push(path);
            }
            2 => {
                std::fs::create_dir(&path).unwrap();
                self.dirs.push((path, depth + 1));
            }
            3 | 4 => {
                let leads = [
                    self.site.outside.join("keep"),
                    self.site.outside.join("dir"),
                    self.elsewhere.join("f"),
                    self.elsewhere.clone(),
                    self.target.clone(),
                    parent,
                ];
                link(rng.pick(&leads).unwrap(), &path);
            }
            _ => {
                let others = [self.site.outside.join("keep"), self.elsewhere.join("f")];
                std::fs::hard_link(rng.pick(&others).unwrap(), &path).unwrap();
            }
        }
    }

    fn standing(&self, path: &Path) -> bool {
        !self.replaced.iter().any(|dir| path.starts_with(dir))
    }

    /// Makes up to three changes after the freeze, as another process might.
    fn change(&mut self, rng: &mut SplitMix) {
        for _ in 0..rng.below(4) {
            let files: Vec<_> = self
                .files
                .iter()
                .filter(|p| self.standing(p))
                .cloned()
                .collect();
            let dirs: Vec<_> = self
                .dirs
                .iter()
                .map(|(path, _)| path.clone())
                .filter(|p| self.standing(p))
                .collect();
            match rng.below(4) {
                0 => self.edit(rng.pick(&files)),
                1 => self.replace_file(rng.pick(&files)),
                2 => self.replace_dir(rng.pick(&dirs)),
                _ => {
                    let dir = rng.pick(&dirs).cloned();
                    self.create(&dir.unwrap_or_else(|| self.target.clone()));
                }
            }
        }
    }

    fn edit(&mut self, file: Option<&PathBuf>) {
        let Some(file) = file else { return };
        let mut contents = std::fs::read(file).unwrap();
        contents.extend_from_slice(b" edited");
        std::fs::write(file, contents).unwrap();
        self.survivors.push(file.clone());
        self.changes += 1;
    }

    fn replace_file(&mut self, file: Option<&PathBuf>) {
        let Some(file) = file else { return };
        self.site
            .replace(file, |path| write(path, b"put there after"));
        self.survivors.push(file.clone());
        self.changes += 1;
    }

    fn replace_dir(&mut self, dir: Option<&PathBuf>) {
        let Some(dir) = dir else { return };
        let outside = self.site.outside.join("dir");
        self.site.replace(dir, |path| link(&outside, path));
        self.survivors.retain(|path| !path.starts_with(dir));
        self.replaced.push(dir.clone());
        self.survivors.push(dir.clone());
        self.changes += 1;
    }

    fn create(&mut self, dir: &Path) {
        let path = dir.join(format!("new{}", self.changes));
        write(&path, b"appeared after the freeze");
        self.survivors.push(path);
        self.changes += 1;
    }
}

/// Catches any way a delete reaches past what it froze: through a link, a hard link, a
/// directory swapped for a link, or into entries created or changed since the freeze.
#[test]
fn a_delete_never_reaches_past_what_it_froze() {
    for number in 0..CASES {
        let mut rng = SplitMix(number);
        let mut case = Case::new(&mut rng);
        let anchor = case.site.anchor("target");
        let tree = freeze(&anchor).tree;
        let outside = listing(&case.site.outside);
        let elsewhere = listing(&case.elsewhere);
        case.change(&mut rng);

        let mut gone = Vec::new();
        let marks = Marks::of(&tree);
        let op = DeleteOp::for_platform_tests(anchor, tree.clone(), marks);
        let outcome = run_through(&op, &CancelToken::new(), |report, _| {
            if let Report::Removed(id, _) = report {
                gone.push(id);
            }
        });

        let context = format!("case {number}: {outcome:?}");
        assert!(
            matches!(outcome.status(), Status::Done | Status::Partial),
            "{context}"
        );
        assert_eq!(listing(&case.site.outside), outside, "{context}");
        assert_eq!(listing(&case.elsewhere), elsewhere, "{context}");
        case.site.assert_outside_intact();
        for survivor in &case.survivors {
            assert!(
                present(survivor),
                "{context}: {} survives",
                survivor.display()
            );
        }
        check_removed(&case, &tree, &gone, &context);
        if case.changes == 0 {
            assert_eq!(outcome.status(), Status::Done, "{context}");
            assert!(!present(&case.target), "{context}");
        }
    }
}

/// Fails unless every entry reported removed is gone.
fn check_removed(case: &Case, tree: &kipple_core::FrozenTree, gone: &[NodeId], context: &str) {
    for id in gone {
        let path = tree.entry(*id).unwrap().path().below(&case.target);
        if case.standing(&path) {
            assert!(
                !present(&path),
                "{context}: {} was reported removed",
                path.display()
            );
        }
    }
}
