//! Asking git which entries inside an item it tracks (`docs/04-safety-model.md` §3.2,
//! D-066).
//!
//! kipple's own walk finds the work tree (the directory holding `.git`); this module only
//! asks it. git is found once on `PATH`, as `doctor` finds it, and must be 2.32 or newer.
//! Each question runs it twice through the platform runner, from the work tree, isolated
//! from the system and global config, the XDG config, hooks, the pager, the fsmonitor and
//! optional locks: `rev-parse --show-toplevel`, which must name the same directory by file
//! identity, and `ls-files -z --cached --full-name` with one literal pathspec: the item's
//! path, or the nearest directory above it whose every name is ASCII (and on Windows the
//! name it is stored under, not an 8.3 short name). git matches pathspecs as strings,
//! folding at most ASCII case, while a file system may fold Unicode case and
//! normalization or take a short name; asking about the item by such a name could miss
//! every path git holds under another spelling of it. Where a directory on the asked
//! path folds case, git is asked ignoring ASCII case.
//!
//! Every path git returns is resolved handle-relative from the work tree, without
//! following a link. One that doesn't resolve to the item is another path below the
//! asked directory and is passed over; one whose way there leads through a link can't
//! be told apart and blocks. Below the item, a path that isn't on disk places nothing;
//! one that is a directory (a submodule), lies through a link, or matches no frozen entry
//! by identity blocks the item. Anything git can't answer in full is a [`GitGap`], which
//! blocks it too.

mod command;
mod place;

use std::path::Path;
use std::sync::OnceLock;

use kipple_core::{
    Anchor, CancelToken, FileIdentity, FrozenTree, GitGap, NodeId, RelPath, TrackedQuery,
};

use crate::environment::Environment;
use crate::executor::sys::{self, Dir};
use crate::os;
use command::{Git, Listing, Pathspec};
use place::{Entries, Located, Place, Placer};

/// The platform's [`TrackedQuery`]: git, found on the `PATH` of the environment it is
/// given, the first time a repository is asked about.
#[derive(Debug)]
pub struct GitQuery {
    env: Environment,
    git: OnceLock<Result<Git, GitGap>>,
}

impl GitQuery {
    /// The query for the machine `env` describes.
    #[must_use]
    pub fn new(env: &Environment) -> Self {
        Self {
            env: env.clone(),
            git: OnceLock::new(),
        }
    }

    fn git(&self) -> Result<&Git, GitGap> {
        self.git
            .get_or_init(|| command::resolve(&self.env))
            .as_ref()
            .map_err(|gap| *gap)
    }

    /// The item at `item`, reached from `work_tree`, and every path git tracks at or
    /// below it there.
    fn ask(
        &self,
        work_tree: &Path,
        item: &Path,
        cancel: &CancelToken,
    ) -> Result<(Located, Listing), GitGap> {
        let git = self.git()?;
        let relative = item
            .strip_prefix(work_tree)
            .ok()
            .and_then(|relative| RelPath::of(relative).ok())
            .ok_or(GitGap::OtherWorkTree)?;
        let work_tree = os::physical(work_tree).map_err(|_| GitGap::Failed)?;
        let located = place::locate(&work_tree, relative.components().to_vec())?;
        let pathspec = Pathspec::of(located.asked(), located.matching)?;
        let toplevel = git.toplevel(&work_tree, cancel)?;
        if identity(&toplevel) != Some(located.work_tree_identity) {
            return Err(GitGap::OtherWorkTree);
        }
        let listing = git.tracked(&work_tree, &pathspec, cancel)?;
        Ok((located, listing))
    }
}

impl TrackedQuery for GitQuery {
    fn count(&self, work_tree: &Path, item: &Path, cancel: &CancelToken) -> Result<u64, GitGap> {
        let _quiet = sys::NoMaterialize::new();
        let (located, listing) = self.ask(work_tree, item, cancel)?;
        Ok(place_all(&located, &listing, cancel)?.tracked)
    }

    fn entries(
        &self,
        work_tree: &Path,
        anchor: &Anchor,
        tree: &FrozenTree,
        cancel: &CancelToken,
    ) -> Result<Vec<NodeId>, GitGap> {
        let _quiet = sys::NoMaterialize::new();
        let (located, listing) = self.ask(work_tree, &anchor.path(), cancel)?;
        let placed = place_all(&located, &listing, cancel)?;
        Entries::of(tree, &located, &placed.on_disk)
    }
}

/// Where the paths git tracks in an item are.
#[derive(Debug)]
struct Placed {
    /// How many paths it tracks there.
    tracked: u64,
    /// Where those on disk are.
    on_disk: Vec<Place>,
}

/// Where each path of `listing` is.
fn place_all(located: &Located, listing: &Listing, cancel: &CancelToken) -> Result<Placed, GitGap> {
    let mut placer = Placer::new(located);
    let mut answer = Placed {
        tracked: 0,
        on_disk: Vec::new(),
    };
    for record in listing.records() {
        if cancel.is_cancelled() {
            return Err(GitGap::Failed);
        }
        match placer.place(&record?)? {
            Place::Outside => continue,
            Place::Absent => {}
            place @ (Place::Item | Place::Below { .. }) => answer.on_disk.push(place),
        }
        answer.tracked += 1;
    }
    Ok(answer)
}

/// Which directory `path` is, its links resolved.
fn identity(path: &Path) -> Option<FileIdentity> {
    let dir = Dir::open_root(&os::physical(path).ok()?).ok()?;
    Some(dir.seen().ok()?.snapshot.identity)
}
