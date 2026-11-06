//! Programs a repository names, and a `git` the test stands in for: Unix, where a script
//! can be one.

use std::os::unix::fs::PermissionsExt as _;

use super::*;
use kipple_core::{GitGap, Omission, Tracked, TrackedQuery as _, WorktreeQuery as _};

/// A script the test's config names, which leaves a marker if anything runs it.
struct Marker {
    script: String,
    path: PathBuf,
}

fn executable(path: &Path, text: &str) {
    write(path, text.as_bytes());
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn marker(repo: &Repo) -> Marker {
    let path = repo.temp.path().join("marker");
    let script = repo.temp.path().join("mark");
    let text = format!("#!/bin/sh\ntouch '{}'\ncat\n", path.display());
    executable(&script, &text);
    Marker {
        script: script.display().to_string(),
        path,
    }
}

/// A marker for `core.sshCommand`. It exits at once, because git waits on what it runs as a
/// transport, and a script that keeps reading would hold a test that fails open forever.
fn fetch_marker(repo: &Repo) -> Marker {
    let path = repo.temp.path().join("marker");
    let script = repo.temp.path().join("mark");
    let text = format!("#!/bin/sh\ntouch '{}'\nexit 1\n", path.display());
    executable(&script, &text);
    Marker {
        script: script.display().to_string(),
        path,
    }
}

/// Where a config key is written.
#[derive(Debug, Clone, Copy)]
enum Scope {
    Local,
    Included,
    Worktree,
}

fn configure(repo: &Repo, scope: Scope, key: &str, value: &str) {
    match scope {
        Scope::Local => {
            repo.git(&["config", key, value]);
        }
        Scope::Included => {
            let file = repo.temp.path().join("included");
            let file = file.display().to_string();
            repo.git(&["config", "-f", &file, key, value]);
            repo.git(&["config", "include.path", &file]);
        }
        Scope::Worktree => {
            repo.git(&["config", "extensions.worktreeConfig", "true"]);
            repo.wt(&["config", "--worktree", key, value]);
        }
    }
}

/// A worktree whose `file.txt` a filter and a diff driver named `mark` apply to.
fn attributed() -> Repo {
    let repo = Repo::bare_bones(Refs::Files);
    repo.commit(".gitattributes", b"*.txt filter=mark diff=mark\n");
    repo.add_worktree();
    repo
}

/// Changes `file.txt`'s bytes without changing its size.
fn same_size_edit(repo: &Repo) {
    write(&repo.checkout.join("file.txt"), b"CONTENTS\n");
}

/// Changes `file.txt`'s time alone, by writing its bytes again.
fn touch(repo: &Repo) {
    write(&repo.checkout.join("file.txt"), b"contents\n");
}

fn assert_nothing_ran(repo: &Repo, marker: &Marker, what: &str) -> Scan {
    let scan = repo.scan();
    assert!(!marker.path.exists(), "{what} ran no program");
    scan
}

/// Catches a scan running what a repository configures: an agent's checkout is
/// untrusted, and `status` runs a clean filter on any file it has to read again, by
/// local, included or worktree config alike.
#[test]
fn no_repository_config_runs_a_program() {
    for key in [
        "filter.mark.clean",
        "filter.mark.smudge",
        "filter.mark.process",
    ] {
        for scope in [Scope::Local, Scope::Included, Scope::Worktree] {
            for edit in [same_size_edit, touch] {
                let repo = attributed();
                let mark = marker(&repo);
                configure(&repo, scope, key, &mark.script);
                edit(&repo);

                let what = format!("{key} by {scope:?}");
                let scan = assert_nothing_ran(&repo, &mark, &what);

                assert_eq!(
                    scan.fact("worktree.config_gate"),
                    FactValue::Enum("blocked"),
                    "{what}"
                );
            }
        }
    }
}

/// Catches the keys `status` could reach that the gate lets through: isolation turns
/// the monitor and the hooks off, and nothing asked diffs.
#[test]
fn no_monitor_hook_or_diff_program_runs() {
    for (key, value) in [
        ("core.fsmonitor", None),
        ("hook.mark.event", Some("post-index-change")),
        ("diff.external", None),
        ("diff.mark.textconv", None),
    ] {
        let repo = attributed();
        let mark = marker(&repo);
        repo.git(&["config", key, value.unwrap_or(&mark.script)]);
        if value.is_some() {
            repo.git(&["config", "hook.mark.command", &mark.script]);
        }
        same_size_edit(&repo);

        assert_nothing_ran(&repo, &mark, key);
    }
}

/// A blobless clone at `projects/clone`, its worktree at `projects/wt` holding a staged
/// rename of a blob the clone never fetched, and `core.sshCommand` naming the marker:
/// `status` looks for the rename, which reads that blob, which is a fetch.
fn blobless_rename(repo: &Repo) -> Marker {
    let text = "the same line, long enough for rename detection to find it\n".repeat(8);
    repo.commit("old.txt", text.as_bytes());
    repo.git(&["config", "uploadpack.allowFilter", "true"]);
    let url = format!("file://{}", repo.main.display());
    let clone = repo.temp.path().join("projects/clone");
    let clone_text = clone.display().to_string();
    let filter = "--filter=blob:none";
    repo.git(&["clone", "-q", filter, "--no-checkout", &url, &clone_text]);
    let add = [
        "worktree",
        "add",
        "-q",
        "--no-checkout",
        "-b",
        "wt",
        "../wt",
    ];
    repo.git_in(&clone, &add);
    let mark = fetch_marker(repo);
    repo.git_in(&clone, &["config", "core.sshCommand", &mark.script]);
    let ssh = "ssh://example.invalid/x";
    repo.git_in(&clone, &["remote", "set-url", "origin", ssh]);
    rename(&repo.main, &repo.temp.path().join("origin"));
    repo.wt(&["read-tree", "HEAD"]);
    repo.wt(&["update-index", "--force-remove", "old.txt"]);
    // One byte apart: an identical blob is a rename by its name alone, read by nothing.
    write(
        &repo.checkout.join("new.txt"),
        format!("{text}.").as_bytes(),
    );
    repo.wt(&["update-index", "--add", "new.txt"]);
    mark
}

/// Catches `status` reaching a blob a blobless clone never fetched, which would run
/// `core.sshCommand`: the partial-clone class stops it, with no guard of the environment
/// left to help, and running `status` by hand shows the fixture can fire.
#[test]
fn a_blobless_clone_fetches_nothing() {
    let repo = Repo::bare_bones(Refs::Files);
    let mark = blobless_rename(&repo);
    repo.run_git(&repo.checkout, &["status", "--porcelain"]);
    assert!(mark.path.exists(), "the fixture's status tries to fetch");
    remove_file(&mark.path);

    let scan = repo.scan_in(&git_with_no_guard(&repo));

    assert!(!mark.path.exists(), "the scan ran no program");
    assert_eq!(
        scan.fact("worktree.config_gate"),
        FactValue::Enum("blocked")
    );
}

/// A `git` that logs each command and passes it on, and what it logged to.
fn logging_git(repo: &Repo) -> (Environment, PathBuf) {
    let log = repo.temp.path().join("git.log");
    let bin = repo.temp.path().join("bin");
    let text = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
        log.display(),
        real_git().display()
    );
    executable(&bin.join("git"), &text);
    (repo.env_with(bin.into_os_string()), log)
}

fn real_git() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|git| git.is_file())
        .unwrap()
}

fn runs(log: &Path, command: &str) -> usize {
    let text = fs::read_to_string(log).unwrap();
    text.lines().filter(|line| line.contains(command)).count()
}

fn status_runs(log: &Path) -> usize {
    runs(log, " status ")
}

/// Catches the order reversed: a gate that answers after `status` has already run
/// what it was there to stop. The clean repository shows the log would catch one.
#[test]
fn config_gate_runs_before_status() {
    let clean = Repo::new();
    let (env, log) = logging_git(&clean);
    clean.scan_in(&env);
    assert_eq!(status_runs(&log), 1, "a clear gate lets status run");

    let blocked = Repo::new();
    blocked.git(&["config", "filter.mark.clean", "cat"]);
    let (gated, gated_log) = logging_git(&blocked);
    let scan = blocked.scan_in(&gated);

    assert_eq!(status_runs(&gated_log), 0);
    assert_eq!(scan.fact("git.tracked_changes"), FactValue::Unknown);
}

/// Catches `status` reaching a submodule: it would enter a repository whose config
/// nothing has gated, so a submodule stops the questions first.
#[test]
fn submodule_row_runs_before_status() {
    let repo = with_submodule();
    let (env, log) = logging_git(&repo);

    let scan = repo.scan_in(&env);

    assert_eq!(status_runs(&log), 0);
    assert_eq!(scan.fact("git.tracked_changes"), FactValue::Unknown);
}

/// Catches the lazy-fetch guard lost from the isolation: the gate stops a blobless clone
/// before anything reads a blob, so only this says the second line is still there.
#[test]
fn every_git_run_forbids_a_lazy_fetch() {
    let repo = Repo::new();
    let log = repo.temp.path().join("env.log");
    let bin = repo.temp.path().join("bin");
    let text = format!(
        "#!/bin/sh\nprintf '%s %s\\n' \"$GIT_NO_LAZY_FETCH\" \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
        log.display(),
        real_git().display()
    );
    executable(&bin.join("git"), &text);

    repo.scan_in(&repo.env_with(bin.into_os_string()));

    let logged = fs::read_to_string(&log).unwrap();
    // Which git this is gets asked outside any repository, so it reads no blob.
    let asked = logged.lines().filter(|line| !line.ends_with("--version"));
    let unguarded = asked.clone().filter(|line| !line.starts_with("1 ")).count();
    assert_eq!(unguarded, 0, "every line of {logged:?}");
    assert_ne!(asked.count(), 0, "git ran");
}

/// Catches `status` entering an unregistered gitlink: with no `.gitmodules` only the index
/// names it, and the repository inside has a config nothing read.
#[test]
fn a_gitlink_alone_stops_status() {
    let repo = Repo::new();
    bare_gitlink(&repo);
    let (env, log) = logging_git(&repo);

    let scan = repo.scan_in(&env);

    assert_eq!(status_runs(&log), 0);
    assert_eq!(scan.fact("worktree.submodules"), FactValue::Bool(true));
}

/// A git that ignores `GIT_NO_LAZY_FETCH`, as every one from 2.36 to 2.44.0 does.
fn git_that_can_fetch(repo: &Repo) -> Environment {
    let bin = repo.temp.path().join("bin");
    let text = format!(
        "#!/bin/sh\nunset GIT_NO_LAZY_FETCH\nexec '{}' \"$@\"\n",
        real_git().display()
    );
    executable(&bin.join("git"), &text);
    repo.env_with(bin.into_os_string())
}

/// A git with neither of the environment's guards against a fetch, so only the gate is left.
fn git_with_no_guard(repo: &Repo) -> Environment {
    let bin = repo.temp.path().join("bin");
    let text = format!(
        "#!/bin/sh\nunset GIT_NO_LAZY_FETCH GIT_ALLOW_PROTOCOL\nexec '{}' \"$@\"\n",
        real_git().display()
    );
    executable(&bin.join("git"), &text);
    repo.env_with(bin.into_os_string())
}

/// A treeless clone at `projects/clone` with a sparse index, its worktree at `projects/wt`,
/// the tree of `away/` never fetched, and `core.sshCommand` naming the marker: reading that
/// tree is a promisor fetch, which runs the marker. The origin is the repository's main.
fn treeless_sparse_clone(repo: &Repo) -> Marker {
    write(&repo.main.join("keep/a.txt"), b"a\n");
    write(&repo.main.join("away/b.txt"), b"b\n");
    repo.git(&["add", "--", "keep", "away"]);
    repo.git(&["commit", "-q", "-m", "two directories"]);
    repo.git(&["config", "uploadpack.allowFilter", "true"]);
    let url = format!("file://{}", repo.main.display());
    let clone = repo.temp.path().join("projects/clone");
    let clone_text = clone.display().to_string();
    let filter = "--filter=tree:0";
    repo.git(&["clone", "-q", filter, "--no-checkout", &url, &clone_text]);
    repo.git_in(
        &clone,
        &["sparse-checkout", "set", "--cone", "--sparse-index", "keep"],
    );
    repo.git_in(&clone, &["checkout", "-q", "main"]);
    repo.git_in(&clone, &["worktree", "add", "-q", "-b", "wt", "../wt"]);
    drop_tree(repo, &clone, "main:away");
    let mark = fetch_marker(repo);
    repo.git_in(&clone, &["config", "core.sshCommand", &mark.script]);
    let ssh = "ssh://example.invalid/x";
    repo.git_in(&clone, &["remote", "set-url", "origin", ssh]);
    // Only the clone: the origin is a plain repository, and nothing else is under test.
    rename(&repo.main, &repo.temp.path().join("origin"));
    let listed = repo.git_in(&repo.checkout, &["ls-files", "--sparse"]);
    assert_eq!(
        String::from_utf8_lossy(&listed)
            .lines()
            .filter(|l| *l == "away/")
            .count(),
        1,
        "the fixture's index is sparse"
    );
    mark
}

/// Deletes the promisor pack holding `object` from the clone at `clone`, as checking out
/// fetched it: only an object that is missing can be fetched again.
fn drop_tree(repo: &Repo, clone: &Path, object: &str) {
    let name = repo.git_in(clone, &["rev-parse", object]);
    let name = String::from_utf8_lossy(&name).trim().to_owned();
    let packs = clone.join(".git/objects/pack");
    for entry in fs::read_dir(&packs).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "idx") {
            continue;
        }
        let text = path.display().to_string();
        let held = repo.git_in(clone, &["verify-pack", "-v", &text]);
        if String::from_utf8_lossy(&held).contains(&name) {
            remove_pack(&path);
        }
    }
}

/// Removes the pack whose index is at `index`.
fn remove_pack(index: &Path) {
    for extension in ["idx", "pack", "promisor", "rev"] {
        let file = index.with_extension(extension);
        if file.exists() {
            remove_file(&file);
        }
    }
}

/// Catches a command before the gate reading an object: `ls-files` expands a sparse index,
/// which in a treeless clone is a fetch, so a scan would run the repository's
/// `core.sshCommand` and report the gate that should have stopped it as blocked.
#[test]
fn a_treeless_sparse_clone_runs_nothing_at_scan() {
    let repo = Repo::bare_bones(Refs::Files);
    let mark = treeless_sparse_clone(&repo);
    let clone = repo.projects().join("clone");
    let program = format!("!{}", mark.script);
    repo.git_in(&clone, &["config", "remote.origin.vcs", "\n"]);
    repo.git_in(&clone, &["config", "help.autocorrect", "-1"]);
    repo.git_in(&clone, &["config", "alias.remote-x", &program]);
    write(&repo.checkout.join("away/web/package.json"), b"{}\n");
    write(
        &repo.checkout.join("away/web/node_modules/dep/index.js"),
        b"\n",
    );

    // Neither second line is here: what holds is that no fetch starts.
    let scan = repo.scan_in(&git_with_no_guard(&repo));

    assert!(!mark.path.exists(), "the scan ran no program");
    assert_eq!(
        scan.fact("worktree.config_gate"),
        FactValue::Enum("blocked")
    );
    let artifact = scan
        .0
        .findings
        .iter()
        .find(|f| f.path.ends_with("node_modules"));
    let tracked = artifact.and_then(|f| f.facts.as_ref()).map(|f| &f.tracked);
    assert!(
        matches!(
            tracked,
            Some(Tracked::InsideRepository {
                gap: GitGap::Failed,
                ..
            })
        ),
        "the artifact is held: {tracked:?}"
    );
}

/// Ways a repository comes to be a promisor, as its config names them.
const PROMISORS: [(&str, Change); 4] = [
    ("remote.origin.promisor", |repo| {
        repo.git(&["config", "remote.origin.promisor", "true"]);
    }),
    ("remote.origin.partialclonefilter", |repo| {
        repo.git(&["config", "remote.origin.partialclonefilter", "blob:none"]);
    }),
    ("extensions.partialClone", |repo| {
        repo.git(&["config", "extensions.partialClone", "origin"]);
    }),
    ("an included file", |repo| {
        let included = repo.temp.path().join("included");
        write(&included, b"[remote \"origin\"]\n\tpromisor = true\n");
        let text = included.display().to_string();
        repo.git(&["config", "include.path", &text]);
    }),
];

/// A worktree with one tracked file in `src`, whose count is 1.
fn with_tracked_file() -> Repo {
    let repo = Repo::new();
    write(&repo.checkout.join("src/kept.txt"), b"x\n");
    repo.wt(&["add", "--", "src"]);
    repo
}

/// What git answers of `src` in the worktree of `repo`, and how many `ls-files` that took.
fn asked_what_is_tracked(repo: &Repo) -> (Result<u64, GitGap>, usize) {
    let (env, log) = logging_git(repo);
    let src = repo.checkout.join("src");
    let count = GitQuery::new(&env).count(&repo.checkout, &src, &CancelToken::new());
    (count, runs(&log, " ls-files "))
}

/// Catches `ls-files` reaching a promisor: from there a lazy fetch can start, and a fetch
/// reaches programs and URLs no transport list covers (`fetch.bundleURI`, `core.askPass`),
/// so the only safe fetch is the one that never starts. A complete repository still answers,
/// which also shows the log would catch a run.
#[test]
fn a_promisor_never_reaches_ls_files_when_asked_what_is_tracked() {
    let complete = asked_what_is_tracked(&with_tracked_file());
    assert_eq!(complete.0, Ok(1));
    assert_ne!(complete.1, 0, "a complete repository is asked");

    for (name, promisor) in PROMISORS {
        let repo = with_tracked_file();
        promisor(&repo);

        let (count, listed) = asked_what_is_tracked(&repo);

        assert_eq!(count, Err(GitGap::Failed), "{name}");
        assert_eq!(listed, 0, "{name}");
    }
}

/// Catches a config git couldn't list read as a repository with no promisor.
#[test]
fn a_config_git_couldnt_list_holds_the_tracked_query() {
    let repo = with_tracked_file();
    let bin = repo.temp.path().join("bin");
    let text = format!(
        "#!/bin/sh\ncase \"$*\" in\n*'config --includes'*) exit 1 ;;\nesac\nexec '{}' \"$@\"\n",
        real_git().display()
    );
    executable(&bin.join("git"), &text);
    let query = GitQuery::new(&repo.env_with(bin.into_os_string()));
    let src = repo.checkout.join("src");

    let count = query.count(&repo.checkout, &src, &CancelToken::new());

    assert_eq!(count, Err(GitGap::Failed));
}

/// Catches the same read in the tracked query, which has no gate: it runs for an artifact
/// in any repository. A sparse directory stays what it is, and is refused.
#[test]
fn a_treeless_sparse_clone_runs_nothing_when_asked_what_is_tracked() {
    let repo = Repo::bare_bones(Refs::Files);
    let mark = treeless_sparse_clone(&repo);
    let query = GitQuery::new(&git_that_can_fetch(&repo));
    let cancel = CancelToken::new();
    let kept = repo.checkout.join("keep");
    let left = repo.checkout.join("away/left");
    fs::create_dir_all(&left).unwrap();

    assert_eq!(
        query.count(&repo.checkout, &left, &cancel),
        Err(GitGap::Failed),
        "below a sparse directory with its tree missing, git can't say"
    );
    assert_eq!(
        query.count(&repo.checkout, &kept, &cancel),
        Err(GitGap::Failed),
        "what is on disk there expands the index, and that lost entries"
    );
    assert!(!mark.path.exists(), "asking ran no program");
}

/// Catches a sparse directory hiding what is tracked below it: an item there has to count
/// its files once the index expands, whatever the repository's config says of expanding.
#[test]
fn what_is_tracked_below_a_sparse_directory_is_counted() {
    let repo = Repo::bare_bones(Refs::Files);
    write(&repo.main.join("keep/a.txt"), b"a\n");
    write(&repo.main.join("away/left/b.txt"), b"b\n");
    repo.git(&["add", "--", "keep", "away"]);
    repo.git(&["commit", "-q", "-m", "two directories"]);
    repo.git(&["sparse-checkout", "set", "--cone", "--sparse-index", "keep"]);
    repo.git(&["config", "sparse.expectFilesOutsideOfPatterns", "true"]);
    let left = repo.main.join("away/left");
    fs::create_dir_all(&left).unwrap();
    let query = GitQuery::new(&repo.env());

    assert_eq!(query.count(&repo.main, &left, &CancelToken::new()), Ok(1));
}

/// Catches `--sparse` passed to a git it misleads. Below 2.35 git refuses it, which holds every
/// artifact in every repository on a git kipple supports. On 2.35 it has the flag and not the
/// expansion for files on disk, so a tracked file below a sparse directory reads as nothing
/// tracked. From 2.36 leaving it out would expand a sparse index for nothing.
#[test]
fn sparse_is_asked_of_a_git_that_has_it_and_no_other() {
    for (version, refused) in [
        ("2.34.1", "*--sparse*"),
        ("2.35.0", "*--sparse*"),
        ("2.36.0", "*--cached\\ --full-name*"),
    ] {
        let repo = Repo::new();
        write(&repo.main.join("src/kept.txt"), b"x\n");
        repo.git(&["add", "--", "src"]);
        let bin = repo.temp.path().join("bin");
        let text = format!(
            "#!/bin/sh\ncase \"$*\" in\n*--version*) echo 'git version {version}' ;;\n\
             {refused}) echo \"error: unknown option\" >&2; exit 129 ;;\n\
             *) exec '{}' \"$@\" ;;\nesac\n",
            real_git().display()
        );
        executable(&bin.join("git"), &text);
        let query = GitQuery::new(&repo.env_with(bin.into_os_string()));

        let count = query.count(&repo.main, &repo.main.join("src"), &CancelToken::new());

        assert_eq!(count, Ok(1), "git {version}");
    }
}

/// Catches a gate read clear over config git never read: from a checkout someone else owns,
/// `config --list` exits 0 printing only kipple's own `-c` keys, so a filter stays unseen.
/// The wrapper refuses everything but the listing, which runs from the repository.
#[test]
fn a_gate_over_config_git_refused_stays_unknown() {
    let repo = Repo::new();
    repo.git(&["config", "filter.mark.clean", "cat"]);
    let bin = repo.temp.path().join("bin");
    let text = format!(
        "#!/bin/sh\ncase \"$*\" in\n*'worktree list'*) ;;\n\
         *) GIT_TEST_ASSUME_DIFFERENT_OWNER=1; export GIT_TEST_ASSUME_DIFFERENT_OWNER ;;\n\
         esac\nexec '{}' \"$@\"\n",
        real_git().display()
    );
    executable(&bin.join("git"), &text);

    let scan = repo.scan_in(&repo.env_with(bin.into_os_string()));

    assert_eq!(scan.fact("worktree.config_gate"), FactValue::Unknown);
}

/// Catches the first record's state standing when records disagree and nothing can be
/// placed: git fails, so the original's reads present with its rows unknown and a stale
/// copy's reads invalid, and which one spoke would depend on how the two names sort.
#[test]
fn records_that_disagree_with_nothing_placed_read_invalid() {
    for name in ["aaa-copy", "zzz-copy"] {
        let repo = Repo::new();
        copy_tree(&repo.main, &repo.projects().join(name));
        repo.wt(&["switch", "-q", "-c", "later"]);
        let bin = repo.temp.path().join("bin");
        let text = format!(
            "#!/bin/sh\ncase \"$*\" in\n*rev-parse*) exit 1 ;;\nesac\nexec '{}' \"$@\"\n",
            real_git().display()
        );
        executable(&bin.join("git"), &text);

        let scan = repo.scan_in(&repo.env_with(bin.into_os_string()));

        assert_eq!(
            scan.fact("registration_state"),
            FactValue::Enum(RegistrationState::RegisteredInvalid.name()),
            "beside {name}"
        );
    }
}

/// Catches a transport denial a repository can name its way past. `Git::tracked` has no gate,
/// and a remote can pick a helper by any name, the empty one as well, which git runs as
/// `git remote-<name>`: an alias of that name in the repository's config then runs a program,
/// and `help.autocorrect` turns a name no alias can hold into one that can. The third route's
/// alias form needs git 2.54, and an older git ignores it.
#[test]
fn no_helper_a_repository_names_runs_when_asked_what_is_tracked() {
    let routes = [
        ("remote.origin.url", "::x", "alias.remote-"),
        ("remote.origin.vcs", "none", "alias.remote-none"),
        ("remote.origin.vcs", "_", "alias.remote-_.command"),
        ("remote.origin.vcs", "\n", "alias.remote-x"),
    ];
    for (key, value, alias) in routes {
        let repo = Repo::bare_bones(Refs::Files);
        let mark = treeless_sparse_clone(&repo);
        let clone = repo.projects().join("clone");
        repo.git_in(&clone, &["config", key, value]);
        let program = format!("!{}", mark.script);
        repo.git_in(&clone, &["config", alias, &program]);
        repo.git_in(&clone, &["config", "help.autocorrect", "-1"]);
        let query = GitQuery::new(&git_that_can_fetch(&repo));
        let left = repo.checkout.join("away/left");
        fs::create_dir_all(&left).unwrap();

        let count = query.count(&repo.checkout, &left, &CancelToken::new());

        assert_eq!(count, Err(GitGap::Failed), "{key}={value}");
        assert!(!mark.path.exists(), "{key}={value} ran no program");
    }
}

/// Catches `ls-files` ahead of the gate again: on a blocked gate nothing that can read the
/// index runs. The clean repository shows the log would catch one.
#[test]
fn a_blocked_gate_runs_no_ls_files() {
    let clean = Repo::new();
    let (env, log) = logging_git(&clean);
    clean.scan_in(&env);
    assert_ne!(runs(&log, " ls-files "), 0, "a clear gate lets it run");

    let blocked = Repo::new();
    blocked.git(&["config", "filter.mark.clean", "cat"]);
    let (gated, gated_log) = logging_git(&blocked);
    blocked.scan_in(&gated);

    assert_eq!(runs(&gated_log, " ls-files "), 0);
}

/// Catches the same through real git, with a repository outside the roots below a directory
/// that isn't UTF-8. A system that refuses such a name, as macOS does, skips it and says so.
#[test]
#[expect(
    clippy::print_stderr,
    reason = "a test that skips says so in its output"
)]
fn a_checkout_of_a_repository_below_a_name_that_is_not_utf8_is_found() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let repo = Repo::bare_bones(Refs::Files);
    let odd = repo.temp.path().join(OsStr::from_bytes(b"out\xff"));
    if let Err(error) = fs::create_dir(&odd) {
        eprintln!("skipped: this file system refuses a name that isn't UTF-8: {error}");
        return;
    }
    let away = odd.join("app");
    let source = repo.main.display().to_string();
    repo.git_in(&odd, &["clone", "-q", &source, "app"]);
    repo.git_in(
        &away,
        &["worktree", "add", "-q", "-b", "far", "../../projects/far"],
    );

    let scan = repo.scan();

    assert_eq!(
        scan.fact_at("far", "worktree.linked"),
        FactValue::Bool(true)
    );
}

/// Catches the worktree floor raised for everything: a git too old for `list -z` can
/// still answer what is tracked, and only worktrees are lost, with an omission.
#[test]
fn old_git_is_too_old_for_worktrees_only() {
    let repo = Repo::new();
    write(&repo.main.join("src/kept.txt"), b"x\n");
    repo.git(&["add", "--", "src"]);
    let bin = repo.temp.path().join("bin");
    let text = format!(
        "#!/bin/sh\ncase \"$*\" in\n*--version*) echo 'git version 2.35.0' ;;\n\
         *) exec '{}' \"$@\" ;;\nesac\n",
        real_git().display()
    );
    executable(&bin.join("git"), &text);
    let env = repo.env_with(bin.into_os_string());
    let query = GitQuery::new(&env);
    let cancel = CancelToken::new();

    assert_eq!(query.list(&repo.main, &cancel), Err(GitGap::TooOld));
    assert_eq!(
        query.count(&repo.main, &repo.main.join("src"), &cancel),
        Ok(1)
    );

    let report = repo.scan_in(&env).0;
    let omissions = report.integrations.iter().flat_map(|i| &i.omissions);
    let too_old = omissions.filter(|omission| {
        matches!(
            omission,
            Omission::GitInventory {
                gap: GitGap::TooOld,
                ..
            }
        )
    });
    assert_eq!(too_old.count(), 1, "once for the repository");
}
