//! Asking git about a repository's worktrees (`docs/06-adapters-and-rules.md` §4).
//!
//! `git worktree remove` exits 0 and deletes what its own refusal doesn't cover, so each
//! fact is proven here on its own, and one that can't be is an `Err`, which holds the
//! worktree.
//!
//! A repository's own config can name a program that `status` runs, and no override reaches
//! all of them. So `inspect` asks in a fixed order. It reads the registration's files first,
//! and stops there unless the checkout is what its registration says: from a directory with
//! no `.git`, git would answer for the repository above it. Then the config's names, never
//! its values, because a value can hold a credential. No command that can read an object
//! runs before that gate is clear: `ls-files` expands a sparse index, which in a partial
//! clone is a fetch, and some gits ignore `GIT_NO_LAZY_FETCH`. Then `ls-files --stage`, where
//! a submodule stops the rest, so `status` never recurses into a config nothing has read.
//! Only then `status` and what follows.

use std::io;
use std::path::{Path, PathBuf};

use kipple_core::{
    CancelToken, Checkout, ConfigClass, ConfigGate, EntryKind, FsProbe as _, GitGap, ObjectId,
    ProbeError, Registration, RegistrationState, WorktreePlace, WorktreeQuery, WorktreeState,
    WorktreeWitness,
};

use super::GitQuery;
use super::command::{Git, identity, os_string};
use crate::fs_probe::NoFollowFs;

/// The first git with `worktree list --porcelain -z`. Without it a path or a lock reason
/// holding a newline can't be read, so older git answers no worktree query.
const WORKTREE_OLDEST: [u64; 2] = [2, 36];

/// The most kept of one of a registration's small files.
const LINK_LIMIT: u64 = 64 * 1024;

/// What an operation in progress leaves in an admin directory.
const OPERATIONS: [&str; 7] = [
    "rebase-merge",
    "rebase-apply",
    "BISECT_LOG",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "MERGE_HEAD",
    "sequencer",
];

/// The locks git takes in an admin directory while it changes the index or `HEAD`.
const LOCKS: [&str; 2] = ["index.lock", "HEAD.lock"];

impl GitQuery {
    /// The git that may answer about worktrees.
    fn worktree_git(&self) -> Result<&Git, GitGap> {
        let git = self.git()?;
        if git.older_than(&WORKTREE_OLDEST) {
            return Err(GitGap::TooOld);
        }
        Ok(git)
    }
}

impl WorktreeQuery for GitQuery {
    fn list(&self, repository: &Path, cancel: &CancelToken) -> Result<Vec<Registration>, GitGap> {
        if link_is_gone(repository)? {
            return Err(GitGap::Unplaced);
        }
        let tail = ["worktree", "list", "--porcelain", "-z"];
        parse_list(&self.worktree_git()?.ask(repository, &tail, cancel)?)
    }

    fn inspect(&self, registration: &Registration, cancel: &CancelToken) -> WorktreeState {
        let links = match Links::read(registration) {
            Ok(Seen::Linked(links)) => links,
            Ok(Seen::Other(state)) => {
                return WorktreeState {
                    registration: Ok(state),
                    ..WorktreeState::unknown(GitGap::Failed)
                };
            }
            Err(gap) => return WorktreeState::unknown(gap),
        };
        let git = self.worktree_git();
        let mut state = WorktreeState {
            registration: Ok(RegistrationState::RegisteredPresent),
            operation_in_progress: operation_in_progress(&links.admin),
            per_worktree_refs: per_worktree_refs(&links.admin),
            index_locked: any_of(&links.admin, &LOCKS),
            ..WorktreeState::unknown(git.as_ref().map_or_else(|gap| *gap, |_| GitGap::Failed))
        };
        if let Ok(git) = git {
            let asking = Asking {
                git,
                registration,
                links: &links,
                cancel,
            };
            asking.fill(&mut state);
        }
        state
    }
}

/// Whether `<dir>/.git` is a file naming something that isn't there: git can only fail from
/// there, and that failure says less than this does.
fn link_is_gone(dir: &Path) -> Result<bool, GitGap> {
    let gitfile = dir.join(".git");
    if kind(&gitfile)? != Some(EntryKind::File) {
        return Ok(false);
    }
    let Some(target) = text_of(&gitfile)?.and_then(|text| gitfile_target(&text)) else {
        return Ok(false);
    };
    Ok(kind(&dir.join(target))?.is_none())
}

/// How many refs the admin directory `admin` holds.
fn per_worktree_refs(admin: &Path) -> Result<u64, GitGap> {
    refuse_reftable(admin)?;
    files_below(&admin.join("refs"))
}

/// Whether what an operation leaves is in the admin directory `admin`.
fn operation_in_progress(admin: &Path) -> Result<bool, GitGap> {
    refuse_reftable(admin)?;
    any_of(admin, &OPERATIONS)
}

/// Refuses a reftable worktree: its refs and pseudo-refs such as `CHERRY_PICK_HEAD` are in
/// its table, and what is left under `admin` would read as nothing there.
fn refuse_reftable(admin: &Path) -> Result<(), GitGap> {
    if exists(&admin.join("reftable"))? {
        return Err(GitGap::Unsupported);
    }
    Ok(())
}

/// What a registration's files say.
enum Seen {
    /// The checkout and its admin directory name each other.
    Linked(Links),
    /// They don't, for this reason. Nothing may ask git from there.
    Other(RegistrationState),
}

/// The files that tie a checkout to its repository.
struct Links {
    checkout: PathBuf,
    gitfile: PathBuf,
    admin: PathBuf,
    gitdir: PathBuf,
    common: PathBuf,
}

impl Links {
    /// Reads `<checkout>/.git`, `<admin>/gitdir` and `<admin>/commondir`, each link resolved
    /// against its own directory. A file that isn't there says the registration is broken;
    /// one that can't be read says nothing.
    fn read(registration: &Registration) -> Result<Seen, GitGap> {
        let invalid = Ok(Seen::Other(RegistrationState::RegisteredInvalid));
        let checkout = registration.path.clone();
        match kind(&checkout)? {
            None => return Ok(Seen::Other(RegistrationState::RegisteredMissing)),
            Some(EntryKind::Dir) => {}
            Some(_) => return invalid,
        }
        let gitfile = checkout.join(".git");
        let Some(target) = text_of(&gitfile)?.and_then(|text| gitfile_target(&text)) else {
            return invalid;
        };
        let admin = checkout.join(target);
        let gitdir = admin.join("gitdir");
        let in_worktrees = admin
            .parent()
            .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "worktrees"));
        let back = text_of(&gitdir)?.and_then(|text| link_path(&text));
        let common = text_of(&admin.join("commondir"))?.and_then(|text| link_path(&text));
        let (true, false, Some(back), Some(common)) =
            (in_worktrees, registration.prunable, back, common)
        else {
            return invalid;
        };
        if !same_file(&admin.join(back), &gitfile)? || !holds_record(&admin, registration)? {
            return invalid;
        }
        let common = admin.join(common);
        Ok(Seen::Linked(Self {
            checkout,
            gitfile,
            admin,
            gitdir,
            common,
        }))
    }
}

/// Whether the admin directory `admin` is the one `registration` was printed from: its `HEAD`
/// names the record's branch, or its commit when it has none. A record with neither says
/// nothing to compare, and a reftable `HEAD` is a stub whose rows stay unknown anyway.
fn holds_record(admin: &Path, registration: &Registration) -> Result<bool, GitGap> {
    if registration.checkout == Checkout::Unnamed || exists(&admin.join("reftable"))? {
        return Ok(true);
    }
    let Some(head) = text_of(&admin.join("HEAD"))? else {
        return Ok(false);
    };
    let head = head.strip_suffix(b"\n").unwrap_or(&head);
    Ok(match &registration.checkout {
        Checkout::Branch(branch) => head
            .strip_prefix(b"ref: refs/heads/")
            .is_some_and(|name| lossy(name) == *branch),
        Checkout::Detached => registration
            .head
            .as_ref()
            .is_some_and(|commit| commit.0.as_bytes() == head),
        Checkout::Unnamed => true,
    })
}

/// One worktree being asked about.
struct Asking<'query> {
    git: &'query Git,
    registration: &'query Registration,
    links: &'query Links,
    cancel: &'query CancelToken,
}

impl Asking<'_> {
    /// Fills in what git says of a worktree whose files agree, in the order the module's
    /// header gives. A row not reached stays unknown.
    fn fill(&self, state: &mut WorktreeState) {
        state.config_gate = self.config_gate();
        state.place = self.place();
        state.witness = self.witness(&state.place);
        if state.config_gate != Ok(ConfigGate::Clear) {
            return;
        }
        state.submodules = self.submodules();
        if state.submodules != Ok(false) {
            return;
        }
        match self.status() {
            Ok(status) => {
                state.tracked_changes = Ok(status.tracked);
                state.untracked_files = Ok(status.untracked);
                state.ignored = Ok(status.ignored);
            }
            Err(gap) => {
                state.tracked_changes = Err(gap);
                state.untracked_files = Err(gap);
                state.ignored = Err(gap);
            }
        }
        state.hidden_changes = self.hidden_changes();
        state.head_reachable = self.head_reachable();
    }

    fn ask(&self, tail: &[&str]) -> Result<Vec<u8>, GitGap> {
        self.git.ask(&self.links.checkout, tail, self.cancel)
    }

    fn ask_index(&self, tail: &[&str]) -> Result<Vec<u8>, GitGap> {
        self.git.ask_index(&self.links.checkout, tail, self.cancel)
    }

    /// Whether anything says it has a submodule, populated or not: `remove` refuses only a
    /// populated one, and deletes the rest.
    fn submodules(&self) -> Result<bool, GitGap> {
        let links = self.links;
        if exists(&links.admin.join("modules"))? || exists(&links.checkout.join(".gitmodules"))? {
            return Ok(true);
        }
        has_gitlink(&self.ask_index(&["ls-files", "-z", "--sparse", "--stage"])?)
    }

    /// What the config says of what may be asked. A `reftable` directory counts whatever
    /// the config says, because a change to git's stub file mustn't open it.
    fn config_gate(&self) -> Result<ConfigGate, GitGap> {
        if exists(&self.links.admin.join("reftable"))? {
            return Ok(ConfigGate::Blocked(ConfigClass::RefStorage));
        }
        let names = self.ask(&["config", "--includes", "--null", "--name-only", "--list"])?;
        let class = records(&names)?.into_iter().find_map(config_class);
        Ok(class.map_or(ConfigGate::Clear, ConfigGate::Blocked))
    }

    /// Where git places the worktree, which must be where its files place it. Each is
    /// compared by identity, because git prints a path its own way.
    fn place(&self) -> Result<WorktreePlace, GitGap> {
        let links = self.links;
        let stdout = self.ask(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--show-toplevel",
        ])?;
        let place = parse_place(&stdout)?;
        let agrees = same_file(&place.top, &links.checkout)?
            && same_file(&place.git_dir, &links.admin)?
            && same_file(&place.common_dir, &links.common)?;
        if agrees {
            Ok(place)
        } else {
            Err(GitGap::OtherWorkTree)
        }
    }

    /// What apply compares, once git placed the worktree where its files do.
    fn witness(&self, place: &Result<WorktreePlace, GitGap>) -> Result<WorktreeWitness, GitGap> {
        let place = place.as_ref().map_err(|gap| *gap)?;
        let links = self.links;
        let known = |path: &Path| identity(path).ok_or(GitGap::Failed);
        let branch = match &self.registration.checkout {
            Checkout::Branch(branch) => Some(branch.clone()),
            Checkout::Detached | Checkout::Unnamed => None,
        };
        Ok(WorktreeWitness {
            common: known(&place.common_dir)?,
            admin: known(&links.admin)?,
            gitfile: known(&links.gitfile)?,
            gitdir_file: known(&links.gitdir)?,
            head: self.registration.head.clone().ok_or(GitGap::Failed)?,
            branch,
        })
    }

    /// The one `status` the tracked, untracked and ignored rows come from. The override
    /// beats a repository's `status.showUntrackedFiles=no`, which hides files `remove`
    /// then deletes.
    fn status(&self) -> Result<Status, GitGap> {
        parse_status(&self.ask(&[
            "-c",
            "status.showUntrackedFiles=all",
            "status",
            "--porcelain=v2",
            "-z",
            "--ignore-submodules=none",
            "--untracked-files=all",
            "--ignored=matching",
        ])?)
    }

    /// How many paths carry a tag `status` doesn't look behind: `skip-worktree`, or
    /// lowercase for assume-unchanged.
    fn hidden_changes(&self) -> Result<u64, GitGap> {
        parse_tags(&self.ask_index(&["ls-files", "-z", "--sparse", "-v"])?)
    }

    /// Whether a local branch holds HEAD, the worktree's own included, since removal never
    /// deletes a branch. A worktree with no commit has none to hold.
    fn head_reachable(&self) -> Result<bool, GitGap> {
        if self.registration.head.is_none() {
            return Ok(false);
        }
        let refs = self.ask(&[
            "for-each-ref",
            "--contains",
            "HEAD",
            "--format=%(refname)",
            "refs/heads/",
        ])?;
        Ok(!refs.is_empty())
    }
}

/// What `status --porcelain=v2 -z` printed.
#[derive(Debug, Default, PartialEq, Eq)]
struct Status {
    tracked: u64,
    untracked: u64,
    ignored: Vec<PathBuf>,
}

/// `status --porcelain=v2 -z`'s records. A rename's `2` record is followed by the path it
/// came from, which is a record of its own. Anything else is an answer not understood.
fn parse_status(stdout: &[u8]) -> Result<Status, GitGap> {
    let mut status = Status::default();
    let parts = records(stdout)?;
    let mut parts = parts.into_iter();
    while let Some(record) = parts.next() {
        match record {
            [b'1' | b'u', b' ', ..] => status.tracked += 1,
            [b'2', b' ', ..] => {
                status.tracked += 1;
                parts.next().ok_or(GitGap::Truncated)?;
            }
            [b'?', b' ', _, ..] => status.untracked += 1,
            [b'!', b' ', path @ ..] if !path.is_empty() => {
                let path = os_string(path).ok_or(GitGap::Truncated)?;
                status.ignored.push(PathBuf::from(path));
            }
            _ => return Err(GitGap::Truncated),
        }
    }
    Ok(status)
}

/// How many of `ls-files -z -v`'s records are tagged anything but `H`. A sparse directory is
/// a gap, as it is in every `ls-files` here: what it holds wasn't listed.
fn parse_tags(stdout: &[u8]) -> Result<u64, GitGap> {
    let mut hidden = 0;
    for record in records(stdout)? {
        match record {
            [.., b'/'] => return Err(GitGap::Unplaced),
            [b'H', b' ', _, ..] => {}
            [_, b' ', _, ..] => hidden += 1,
            _ => return Err(GitGap::Truncated),
        }
    }
    Ok(hidden)
}

/// Whether `ls-files -z --stage` names a gitlink: `<mode> <oid> <stage>\t<path>`.
fn has_gitlink(stdout: &[u8]) -> Result<bool, GitGap> {
    let mut found = false;
    for record in records(stdout)? {
        if !record.contains(&b'\t') || !record.contains(&b' ') {
            return Err(GitGap::Truncated);
        }
        if record.ends_with(b"/") {
            return Err(GitGap::Unplaced);
        }
        found = found || record.starts_with(b"160000 ");
    }
    Ok(found)
}

/// The three paths `rev-parse` prints, one a line.
fn parse_place(stdout: &[u8]) -> Result<WorktreePlace, GitGap> {
    let body = stdout.strip_suffix(b"\n").ok_or(GitGap::Truncated)?;
    let lines: Vec<&[u8]> = body.split(|byte| *byte == b'\n').collect();
    let [git_dir, common_dir, top] = lines[..] else {
        return Err(GitGap::Truncated);
    };
    Ok(WorktreePlace {
        git_dir: absolute(git_dir).ok_or(GitGap::Truncated)?,
        common_dir: absolute(common_dir).ok_or(GitGap::Truncated)?,
        top: absolute(top).ok_or(GitGap::Truncated)?,
    })
}

/// The class of the config key `name`, if it is one the gate stops at. Git prints a key's
/// section and variable in lowercase, and the part between them as written.
fn config_class(name: &[u8]) -> Option<ConfigClass> {
    let name = String::from_utf8_lossy(name).to_ascii_lowercase();
    let (section, rest) = name.split_once('.')?;
    let variable = rest.rsplit('.').next()?;
    match (section, variable) {
        ("filter", "clean" | "smudge" | "process") => Some(ConfigClass::Filter),
        ("remote", "promisor" | "partialclonefilter") | ("extensions", "partialclone") => {
            Some(ConfigClass::PartialClone)
        }
        ("core", "worktree") => Some(ConfigClass::WorkTree),
        ("extensions", "refstorage") => Some(ConfigClass::RefStorage),
        _ => None,
    }
}

/// The records of output that ends each one in a NUL.
fn records(stdout: &[u8]) -> Result<Vec<&[u8]>, GitGap> {
    if stdout.is_empty() {
        return Ok(Vec::new());
    }
    let body = stdout.strip_suffix(b"\0").ok_or(GitGap::Truncated)?;
    Ok(body.split(|byte| *byte == 0).collect())
}

/// `worktree list --porcelain -z`: attributes that each end in a NUL, and one more NUL after
/// each record. Git always lists the main worktree, so nothing at all is an answer cut
/// short, never "no worktrees". A record it doesn't understand is an unknown layout.
fn parse_list(stdout: &[u8]) -> Result<Vec<Registration>, GitGap> {
    let body = stdout.strip_suffix(b"\0\0").ok_or(GitGap::Truncated)?;
    let attributes: Vec<&[u8]> = body.split(|byte| *byte == 0).collect();
    attributes
        .split(|attribute| attribute.is_empty())
        .map(parse_record)
        .collect()
}

fn parse_record(attributes: &[&[u8]]) -> Result<Registration, GitGap> {
    let (first, rest) = attributes.split_first().ok_or(GitGap::Truncated)?;
    let path = first
        .strip_prefix(b"worktree ")
        .and_then(absolute)
        .ok_or(GitGap::Truncated)?;
    let mut registration = Registration {
        path,
        head: None,
        checkout: Checkout::Unnamed,
        locked: None,
        prunable: false,
        bare: false,
    };
    for attribute in rest {
        read_attribute(&mut registration, attribute)?;
    }
    Ok(registration)
}

/// Sets what `attribute` says. `prunable`'s reason is translated text, so it is read by its
/// presence alone. A lock's reason may hold a newline.
fn read_attribute(registration: &mut Registration, attribute: &[u8]) -> Result<(), GitGap> {
    let mut parts = attribute.splitn(2, |byte| *byte == b' ');
    let name = parts.next().unwrap_or_default();
    let value = parts.next();
    match (name, value) {
        (b"bare", None) => registration.bare = true,
        (b"HEAD", Some(value)) => registration.head = object(value)?,
        (b"detached", None) => registration.checkout = Checkout::Detached,
        (b"branch", Some(value)) => {
            let branch = value.strip_prefix(b"refs/heads/");
            let branch = branch
                .filter(|branch| !branch.is_empty())
                .ok_or(GitGap::Truncated)?;
            registration.checkout = Checkout::Branch(lossy(branch));
        }
        (b"locked", reason) => registration.locked = Some(lossy(reason.unwrap_or_default())),
        (b"prunable", _) => registration.prunable = true,
        _ => return Err(GitGap::Truncated),
    }
    Ok(())
}

/// The commit `value` names. All zeroes is an unborn branch: no commit.
fn object(value: &[u8]) -> Result<Option<ObjectId>, GitGap> {
    let sized = value.len() == 40 || value.len() == 64;
    let hex = |byte: &u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte);
    if !sized || !value.iter().all(hex) {
        return Err(GitGap::Truncated);
    }
    let unborn = value.iter().all(|byte| *byte == b'0');
    Ok((!unborn).then(|| ObjectId(lossy(value))))
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The path `bytes` hold, if it is absolute.
fn absolute(bytes: &[u8]) -> Option<PathBuf> {
    let path = PathBuf::from(os_string(bytes)?);
    path.is_absolute().then_some(path)
}

/// The path a `.git` file names.
fn gitfile_target(text: &[u8]) -> Option<PathBuf> {
    link_path(text.strip_prefix(b"gitdir: ")?)
}

/// The path a link file of git's holds, without its line ending.
fn link_path(text: &[u8]) -> Option<PathBuf> {
    let text = text.strip_suffix(b"\n").unwrap_or(text);
    let text = text.strip_suffix(b"\r").unwrap_or(text);
    (!text.is_empty()).then(|| os_string(text).map(PathBuf::from))?
}

/// Whether `error` says nothing is there, which on an unmounted disk it says too.
fn absent(error: &ProbeError) -> bool {
    matches!(
        error.io_kind(),
        Some(io::ErrorKind::NotFound | io::ErrorKind::NotADirectory)
    )
}

/// What is at `path` itself, if anything.
fn kind(path: &Path) -> Result<Option<EntryKind>, GitGap> {
    match NoFollowFs.metadata(path) {
        Ok(meta) => Ok(Some(meta.kind)),
        Err(error) if absent(&error) => Ok(None),
        Err(_) => Err(GitGap::Failed),
    }
}

fn exists(path: &Path) -> Result<bool, GitGap> {
    kind(path).map(|kind| kind.is_some())
}

/// The regular file at `path`, if that is what is there.
fn text_of(path: &Path) -> Result<Option<Vec<u8>>, GitGap> {
    if kind(path)? != Some(EntryKind::File) {
        return Ok(None);
    }
    let bytes = NoFollowFs.read_file(path, LINK_LIMIT);
    bytes.map(Some).map_err(|_| GitGap::Failed)
}

/// Whether `a` and `b` are one file, which needs both identities known.
fn same_file(a: &Path, b: &Path) -> Result<bool, GitGap> {
    let b = identity(b).ok_or(GitGap::Failed)?;
    Ok(identity(a) == Some(b))
}

/// Whether any of `names` is in `dir`.
fn any_of(dir: &Path, names: &[&str]) -> Result<bool, GitGap> {
    for name in names {
        if exists(&dir.join(name))? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// How many entries that aren't directories are below `dir`, which may not be there.
fn files_below(dir: &Path) -> Result<u64, GitGap> {
    let mut files = 0;
    let mut pending = vec![dir.to_owned()];
    while let Some(at) = pending.pop() {
        let entries = match NoFollowFs.read_dir(&at) {
            Ok(entries) => entries,
            Err(error) if absent(&error) => continue,
            Err(_) => return Err(GitGap::Failed),
        };
        for entry in entries {
            if entry.kind == EntryKind::Dir {
                pending.push(at.join(entry.name));
            } else {
                files += 1;
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute path on this system, as git prints one.
    fn base() -> String {
        std::env::temp_dir()
            .join("wt")
            .to_str()
            .unwrap()
            .replace('\\', "/")
    }

    fn head() -> String {
        "0dc8".repeat(10)
    }

    /// Two records as S5 git §4 lists them: a main and a locked worktree with a reason
    /// that holds a newline.
    fn listing() -> Vec<u8> {
        let (base, head) = (base(), head());
        format!(
            "worktree {base}/repo\0HEAD {head}\0branch refs/heads/main\0\0\
             worktree {base}/wt\0HEAD {head}\0detached\0locked agent pid 1\nx\0prunable gone\0\0"
        )
        .into_bytes()
    }

    /// Catches a lock reason's newline splitting a record, and `prunable` read by its
    /// translated text, either of which would misread a locked or missing worktree.
    #[test]
    fn a_record_keeps_every_attribute_git_prints() {
        let parsed = parse_list(&listing()).unwrap();

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].checkout, Checkout::Branch("main".to_owned()));
        assert_eq!(parsed[0].locked, None);
        assert_eq!(parsed[1].checkout, Checkout::Detached);
        assert_eq!(parsed[1].locked.as_deref(), Some("agent pid 1\nx"));
        assert_eq!(parsed[1].head, Some(ObjectId(head())));
        assert!(parsed[1].prunable, "a record with `prunable` is prunable");
    }

    /// Catches an unborn branch taken for a commit, which would ask git about a HEAD that
    /// doesn't resolve, and a bare main refused for having no `HEAD`.
    #[test]
    fn an_unborn_branch_has_no_commit_and_a_bare_main_lists_its_worktrees() {
        let (base, zero) = (base(), "0".repeat(40));
        let text = format!(
            "worktree {base}/bare.git\0bare\0\0\
             worktree {base}/wt\0HEAD {zero}\0branch refs/heads/new\0locked\0\0"
        );

        let parsed = parse_list(text.as_bytes()).unwrap();

        assert!(parsed[0].bare, "the first record is the bare main");
        assert_eq!(parsed[1].head, None);
        assert_eq!(
            parsed[1].locked.as_deref(),
            Some(""),
            "a lock with no reason"
        );
    }

    /// Catches an answer cut at any byte read as a shorter list: a registration past the
    /// cut would drop out of the report, and that reads as "no anomalies".
    #[test]
    fn list_without_final_double_nul_is_a_gap() {
        let whole = listing();
        for end in 0..whole.len() {
            let cut = &whole[..end];
            if !cut.ends_with(b"\0\0") {
                assert_eq!(parse_list(cut), Err(GitGap::Truncated), "cut at {end}");
            }
        }
        assert_eq!(parse_list(b""), Err(GitGap::Truncated), "nothing at all");
        assert_eq!(parse_list(b"\0\0"), Err(GitGap::Truncated), "no record");
    }

    /// Catches an attribute a newer git adds being skipped: it may say the worktree must
    /// not be touched.
    #[test]
    fn unknown_list_attribute_blocks() {
        let base = base();
        for attribute in [
            "pinned",
            "pinned by agent",
            "HEAD",
            "HEAD xyz",
            "branch main",
        ] {
            let text = format!("worktree {base}/repo\0{attribute}\0\0");
            assert_eq!(
                parse_list(text.as_bytes()),
                Err(GitGap::Truncated),
                "{attribute}"
            );
        }
        let relative = "worktree repo\0bare\0\0";
        assert_eq!(parse_list(relative.as_bytes()), Err(GitGap::Truncated));
    }

    /// A small seeded generator: a failure names the seed that reproduces it.
    struct Seeded(u64);

    impl Seeded {
        fn next(&mut self) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(self.0 >> 33).unwrap()
        }
    }

    /// Catches a parser that panics, or that hands back a registration git never printed,
    /// on what a damaged or hostile listing holds: every record it accepts names an
    /// absolute path, and the first attribute of each is still `worktree`.
    #[test]
    fn list_output_survives_generated_input() {
        let whole = listing();
        for seed in 0..2000 {
            let Ok(parsed) = parse_list(&mangled(&whole, seed)) else {
                continue;
            };
            assert!(!parsed.is_empty(), "seed {seed}: an empty list is a gap");
            for registration in parsed {
                assert!(
                    registration.path.is_absolute(),
                    "seed {seed}: {registration:?} has an absolute path"
                );
            }
        }
    }

    /// `whole` with a few bytes changed, dropped or added, the same for the same `seed`.
    fn mangled(whole: &[u8], seed: u64) -> Vec<u8> {
        let mut random = Seeded(seed);
        let mut bytes = whole.to_vec();
        for _ in 0..=random.next() % 6 {
            let at = random.next() % bytes.len();
            match random.next() % 4 {
                0 => bytes[at] = 0,
                1 => bytes[at] = u8::try_from(random.next() % 256).unwrap(),
                2 => drop(bytes.remove(at)),
                _ => bytes.insert(at, b' '),
            }
        }
        bytes
    }

    /// Catches a rename's second path read as a record: it would fail a clean status with a
    /// staged rename, or count a path named like a status line.
    #[test]
    fn status_counts_each_kind_and_a_rename_takes_its_old_path() {
        let stdout = b"1 .M N... 100644 100644 100644 a a a.txt\0\
                       2 R. N... 100644 100644 100644 a a R100 new\0? odd\0\
                       u UU N... 1 1 1 1 a a a c.txt\0? new.txt\0! debug.log\0! node_modules/\0";

        let status = parse_status(stdout).unwrap();

        assert_eq!(status.tracked, 3);
        assert_eq!(status.untracked, 1);
        assert_eq!(
            status.ignored,
            [PathBuf::from("debug.log"), PathBuf::from("node_modules/")]
        );
    }

    /// Catches a status cut short or of a shape never seen counted as clean.
    #[test]
    fn a_status_not_understood_is_a_gap() {
        for stdout in [&b"? new.txt"[..], b"# branch.oid x\0", b"2 R. x\0", b"! \0"] {
            assert_eq!(parse_status(stdout), Err(GitGap::Truncated), "{stdout:?}");
        }
        assert_eq!(parse_status(b""), Ok(Status::default()), "a clean checkout");
    }

    /// Catches `skip-worktree` and assume-unchanged read as plain tracked files: `status`
    /// shows neither, and `remove` deletes their edits.
    #[test]
    fn every_tag_but_plain_tracked_is_hidden() {
        assert_eq!(parse_tags(b"H a\0H b\0"), Ok(0));
        assert_eq!(parse_tags(b"H a\0S b\0h c\0s d\0"), Ok(3));
        assert_eq!(parse_tags(b"Ha\0"), Err(GitGap::Truncated));
    }

    /// Catches a sparse directory taken for one path: what it holds wasn't listed, so
    /// neither row can read clear.
    #[test]
    fn a_directory_in_the_index_is_a_gap() {
        assert_eq!(parse_tags(b"H a\0S away/\0"), Err(GitGap::Unplaced));
        let sparse = b"100644 aa 0\ta\x00040000 bb 0\taway/\0";
        assert_eq!(has_gitlink(sparse), Err(GitGap::Unplaced));
    }

    /// Catches a key the gate names being let through by its spelling, and a key it doesn't
    /// name stopping every repository: `core.sshCommand` is only reached through a fetch.
    #[test]
    fn each_key_of_the_gate_has_its_class() {
        let cases = [
            ("filter.lfs.clean", Some(ConfigClass::Filter)),
            ("filter.a.b.smudge", Some(ConfigClass::Filter)),
            ("FILTER.Lfs.Process", Some(ConfigClass::Filter)),
            ("filter.lfs.required", None),
            ("remote.origin.promisor", Some(ConfigClass::PartialClone)),
            (
                "remote.o.partialclonefilter",
                Some(ConfigClass::PartialClone),
            ),
            ("extensions.partialclone", Some(ConfigClass::PartialClone)),
            ("core.worktree", Some(ConfigClass::WorkTree)),
            ("extensions.refstorage", Some(ConfigClass::RefStorage)),
            ("core.sshcommand", None),
            ("core.fsmonitor", None),
            ("remote.origin.url", None),
            ("user.name", None),
        ];
        for (name, class) in cases {
            assert_eq!(config_class(name.as_bytes()), class, "{name}");
        }
    }

    /// Catches a line ending kept in a path, which would resolve to nothing and call a sound
    /// registration broken.
    #[test]
    fn a_link_file_gives_its_path_without_its_line_ending() {
        assert_eq!(
            gitfile_target(b"gitdir: ../repo/.git/worktrees/x\n"),
            Some(PathBuf::from("../repo/.git/worktrees/x"))
        );
        assert_eq!(gitfile_target(b"gitdir: x\r\n"), Some(PathBuf::from("x")));
        assert_eq!(gitfile_target(b"gitdir: \n"), None);
        assert_eq!(gitfile_target(b"not a link"), None);
    }
}
