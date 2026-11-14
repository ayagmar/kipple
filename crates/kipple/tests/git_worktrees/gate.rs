//! The `gate`: what a real git says through the platform's `GitQuery`, supplied as facts.

#[cfg(unix)]
mod programs;
mod removing;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use kipple_core::{
    CancelToken, Checkout, FactValue, Registration, RegistrationState, Roots, SymbolicRoot,
    WorktreeQuery as _,
};
use kipple_platform::GitQuery;

use crate::repo::{Refs, Repo, Scan, scan_of, write};

/// Catches a false block: every row reads clear on a clean worktree, or nothing could
/// ever be eligible once m4c gives these rules predicates.
#[test]
fn a_clean_worktree_reads_clear_on_every_row() {
    let scan = Repo::new().scan();

    for (fact, value) in [
        ("worktree.linked", FactValue::Bool(true)),
        ("repository_identity", FactValue::Enum("verified")),
        ("worktree.locked", FactValue::Bool(false)),
        ("worktree.submodules", FactValue::Bool(false)),
        ("worktree.config_gate", FactValue::Enum("clear")),
        ("git.tracked_changes", FactValue::Count(0)),
        ("git.untracked_files", FactValue::Count(0)),
        ("git.hidden_changes", FactValue::Count(0)),
        ("worktree.unclassified_ignored", FactValue::Count(0)),
        ("worktree.operation_in_progress", FactValue::Bool(false)),
        ("worktree.per_worktree_refs", FactValue::Count(0)),
        ("worktree.index_locked", FactValue::Bool(false)),
        ("worktree.head_reachable", FactValue::Bool(true)),
    ] {
        assert_eq!(scan.fact(fact), value, "{fact}");
    }
    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredPresent.name())
    );
}

/// One state a worktree can be in, and the blocking value of the row it shows in.
struct Case {
    name: &'static str,
    set: fn(&Repo),
    at: &'static str,
    fact: &'static str,
    value: FactValue,
}

const fn case(name: &'static str, set: fn(&Repo), fact: &'static str, value: FactValue) -> Case {
    Case {
        name,
        set,
        at: "wt",
        fact,
        value,
    }
}

const IGNORED: &str = "worktree.unclassified_ignored";
const OPERATION: &str = "worktree.operation_in_progress";
const LOCKED_INDEX: &str = "worktree.index_locked";

fn ignored_file(repo: &Repo) {
    write(&repo.checkout.join(".gitignore"), b"local.env\n");
    write(&repo.checkout.join("local.env"), b"x\n");
}

fn excluded_file(repo: &Repo) {
    write(&repo.main.join(".git/info/exclude"), b"local.env\n");
    write(&repo.checkout.join("local.env"), b"x\n");
}

fn ignored_repository(repo: &Repo) {
    write(&repo.checkout.join(".gitignore"), b"scratch/\n");
    let nested = repo.checkout.join("scratch");
    fs::create_dir(&nested).unwrap();
    repo.git_in(&nested, &["init", "-q"]);
}

fn nested_worktree(repo: &Repo) {
    write(&repo.checkout.join(".gitignore"), b"scratch/\n");
    repo.git(&["worktree", "add", "-q", "-b", "n", "../wt/scratch/nested"]);
}

fn hidden_edit(repo: &Repo, flag: &str) {
    repo.wt(&["update-index", flag, "file.txt"]);
    write(&repo.checkout.join("file.txt"), b"changed\n");
}

fn rebase_stopped(repo: &Repo) {
    write(&repo.checkout.join("file.txt"), b"theirs\n");
    repo.wt(&["commit", "-q", "-a", "-m", "theirs"]);
    repo.commit("file.txt", b"ours\n");
    let output = repo.run_git(&repo.checkout, &["rebase", "main"]);
    assert!(!output.status.success(), "the rebase stops: {output:?}");
}

/// A gitlink in the index with no `.gitmodules`, so only the index says it is there.
fn bare_gitlink(repo: &Repo) {
    let head = repo.wt(&["rev-parse", "HEAD"]);
    let head = String::from_utf8_lossy(&head);
    let entry = format!("160000,{},sm", head.trim());
    repo.wt(&["update-index", "--add", "--cacheinfo", &entry]);
}

fn unreachable_head(repo: &Repo) {
    repo.wt(&["checkout", "-q", "--detach"]);
    write(&repo.checkout.join("file.txt"), b"changed\n");
    repo.wt(&["commit", "-q", "-a", "-m", "lost"]);
}

fn one_way_registration(repo: &Repo) {
    let elsewhere = repo.temp.path().join("projects/elsewhere/.git");
    let line = format!("{}\n", elsewhere.display());
    write(&repo.admin().join("gitdir"), line.as_bytes());
}

fn unborn(repo: &Repo) {
    repo.git(&["worktree", "add", "-q", "--detach", "../unborn"]);
    let unborn = repo.temp.path().join("projects/unborn");
    repo.git_in(&unborn, &["checkout", "-q", "--orphan", "new"]);
}

const CASES: &[Case] = &[
    case(
        "locked",
        |repo| {
            repo.git(&["worktree", "lock", "--reason", "agent", "../wt"]);
        },
        "worktree.locked",
        FactValue::Bool(true),
    ),
    case(
        "tracked change",
        |repo| write(&repo.checkout.join("file.txt"), b"changed\n"),
        "git.tracked_changes",
        FactValue::Count(1),
    ),
    case(
        "untracked",
        |repo| write(&repo.checkout.join("new.txt"), b"x\n"),
        "git.untracked_files",
        FactValue::Count(1),
    ),
    case(
        "untracked hidden by config",
        |repo| {
            repo.git(&["config", "status.showUntrackedFiles", "no"]);
            write(&repo.checkout.join("new.txt"), b"x\n");
        },
        "git.untracked_files",
        FactValue::Count(1),
    ),
    case(
        "ignored by .gitignore",
        ignored_file,
        IGNORED,
        FactValue::Count(1),
    ),
    case(
        "ignored by info/exclude",
        excluded_file,
        IGNORED,
        FactValue::Count(1),
    ),
    case(
        "ignored repository",
        ignored_repository,
        IGNORED,
        FactValue::Count(1),
    ),
    case(
        "nested worktree",
        nested_worktree,
        IGNORED,
        FactValue::Count(1),
    ),
    case(
        "skip-worktree edit",
        |repo| hidden_edit(repo, "--skip-worktree"),
        "git.hidden_changes",
        FactValue::Count(1),
    ),
    case(
        "assume-unchanged edit",
        |repo| hidden_edit(repo, "--assume-unchanged"),
        "git.hidden_changes",
        FactValue::Count(1),
    ),
    case(
        "real rebase",
        rebase_stopped,
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "rebase-merge",
        |repo| fs::create_dir(repo.admin().join("rebase-merge")).unwrap(),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "rebase-apply",
        |repo| fs::create_dir(repo.admin().join("rebase-apply")).unwrap(),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "bisect",
        |repo| write(&repo.admin().join("BISECT_LOG"), b"x\n"),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "cherry-pick",
        |repo| write(&repo.admin().join("CHERRY_PICK_HEAD"), b"x\n"),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "revert",
        |repo| write(&repo.admin().join("REVERT_HEAD"), b"x\n"),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "merge",
        |repo| write(&repo.admin().join("MERGE_HEAD"), b"x\n"),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "sequencer",
        |repo| fs::create_dir(repo.admin().join("sequencer")).unwrap(),
        OPERATION,
        FactValue::Bool(true),
    ),
    case(
        "gitlink with no .gitmodules",
        bare_gitlink,
        "worktree.submodules",
        FactValue::Bool(true),
    ),
    case(
        "modules in the admin directory",
        |repo| fs::create_dir(repo.admin().join("modules")).unwrap(),
        "worktree.submodules",
        FactValue::Bool(true),
    ),
    case(
        "index.lock",
        |repo| write(&repo.admin().join("index.lock"), b""),
        LOCKED_INDEX,
        FactValue::Bool(true),
    ),
    case(
        "HEAD.lock",
        |repo| write(&repo.admin().join("HEAD.lock"), b""),
        LOCKED_INDEX,
        FactValue::Bool(true),
    ),
    case(
        "per-worktree ref",
        |repo| {
            repo.wt(&["update-ref", "refs/worktree/keep", "HEAD"]);
        },
        "worktree.per_worktree_refs",
        FactValue::Count(1),
    ),
    case(
        "unreachable detached HEAD",
        unreachable_head,
        "worktree.head_reachable",
        FactValue::Bool(false),
    ),
    Case {
        name: "unborn branch",
        set: unborn,
        at: "unborn",
        fact: "worktree.head_reachable",
        value: FactValue::Bool(false),
    },
    case(
        "one-way registration",
        one_way_registration,
        "registration_state",
        FactValue::Enum("present-unregistered"),
    ),
];

/// Catches a row that doesn't reach its fact, or reads clear on the very state it exists
/// to show: each of these loses work if a worktree in it is removed.
#[test]
fn each_gate_fact_is_supplied() {
    for case in CASES {
        let repo = Repo::new();
        (case.set)(&repo);

        let scan = repo.scan();

        assert_eq!(
            scan.fact_at(case.at, case.fact),
            case.value,
            "{}",
            case.name
        );
    }
}

/// A repository whose worktree has a submodule at `sm`, not populated.
fn with_submodule() -> Repo {
    let repo = Repo::bare_bones(Refs::Files);
    let sub = repo.temp.path().join("sub");
    fs::create_dir(&sub).unwrap();
    repo.git_in(&sub, &["init", "-q", "-b", "main"]);
    repo.git_in(&sub, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let url = sub.display().to_string();
    let add = ["-c", "protocol.file.allow=always", "submodule", "add", "-q"];
    repo.git(&[&add[..], &[url.as_str(), "sm"]].concat());
    repo.git(&["commit", "-q", "-m", "submodule"]);
    repo.add_worktree();
    repo
}

/// Catches a submodule missed for being empty here: git refuses to remove a worktree
/// with one, populated or not, and a populated one holds a repository of its own.
#[test]
fn a_submodule_is_supplied_populated_or_not() {
    let repo = with_submodule();
    assert_eq!(
        repo.scan().fact("worktree.submodules"),
        FactValue::Bool(true)
    );

    repo.wt(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "update",
        "--init",
    ]);
    assert_eq!(
        repo.scan().fact("worktree.submodules"),
        FactValue::Bool(true)
    );
}

/// Catches `core.worktree` answered around: status would describe another directory and
/// call this worktree clean.
#[test]
fn core_worktree_makes_status_facts_unknown() {
    let repo = Repo::new();
    repo.git(&["config", "extensions.worktreeConfig", "true"]);
    let other = repo.temp.path().join("other");
    fs::create_dir(&other).unwrap();
    let other = other.display().to_string();
    repo.wt(&["config", "--worktree", "core.worktree", &other]);

    let scan = repo.scan();

    assert_eq!(
        scan.fact("worktree.config_gate"),
        FactValue::Enum("blocked")
    );
    assert_eq!(scan.fact("git.tracked_changes"), FactValue::Unknown);
    assert_eq!(scan.fact("git.untracked_files"), FactValue::Unknown);
}

/// Whether this git can make a reftable repository. A git from 2.45 on that can't is a
/// failure, because the test would otherwise pass with nothing proved.
#[expect(
    clippy::print_stderr,
    reason = "a test that skips says so in its output"
)]
fn makes_reftable() -> bool {
    let probe = Repo::bare_bones(Refs::Files);
    let args = ["init", "-q", "--ref-format=reftable", "../probe"];
    if probe.run_git(&probe.main, &args).status.success() {
        return true;
    }
    let version = probe.git(&["--version"]);
    let version = String::from_utf8_lossy(&version);
    let number = version.split_whitespace().nth(2).unwrap_or_default();
    let parts: Vec<u64> = number
        .split('.')
        .filter_map(|part| part.parse().ok())
        .collect();
    assert!(
        parts[..] < [2, 45][..],
        "{version} is 2.45 or newer and made no reftable repository"
    );
    eprintln!("skipped: {version} is older than 2.45 and can't make a reftable repository");
    false
}

/// Catches a reftable repository read as if its refs were files: its per-worktree refs
/// aren't where they are counted, so zero would be a guess. On a git older than 2.45 this
/// proves nothing, and says so.
#[test]
fn reftable_makes_facts_unknown() {
    if !makes_reftable() {
        return;
    }

    let scan = Repo::linked(Refs::Reftable).scan();

    assert_eq!(
        scan.fact("worktree.config_gate"),
        FactValue::Enum("blocked")
    );
    assert_eq!(scan.fact("worktree.per_worktree_refs"), FactValue::Unknown);
    assert_eq!(scan.fact(OPERATION), FactValue::Unknown);
    assert_eq!(scan.fact("git.tracked_changes"), FactValue::Unknown);
}

/// Catches a sparse directory read as an empty one: a gitlink or a hidden edit below it is
/// behind that record, so the rows that come from the index can't read clear.
#[test]
fn a_sparse_directory_leaves_the_index_rows_unknown() {
    let repo = Repo::bare_bones(Refs::Files);
    write(&repo.main.join("keep/a.txt"), b"a\n");
    write(&repo.main.join("away/b.txt"), b"b\n");
    repo.git(&["add", "--", "keep", "away"]);
    repo.git(&["commit", "-q", "-m", "two directories"]);
    repo.git(&["sparse-checkout", "set", "--cone", "--sparse-index", "keep"]);
    repo.add_worktree();

    let scan = repo.scan();

    assert_eq!(scan.fact("worktree.submodules"), FactValue::Unknown);
    assert_eq!(scan.fact("git.hidden_changes"), FactValue::Unknown);
}

/// Catches a config value carried into a report: the gate reads key names, and a URL or a
/// filter command can hold a credential.
#[test]
fn config_values_appear_nowhere() {
    const SECRET: &str = "s3cret-value";
    let repo = Repo::new();
    let url = format!("https://user:{SECRET}@example.invalid/x.git");
    repo.git(&["config", "remote.origin.url", &url]);
    repo.git(&["config", "filter.mark.clean", &format!("cat {SECRET}")]);

    let scan = repo.scan();

    assert_eq!(
        scan.fact("worktree.config_gate"),
        FactValue::Enum("blocked")
    );
    let report = format!("{:?}", scan.0);
    assert_eq!(report.matches(SECRET).count(), 0, "in {report}");
}

/// Catches a space or a byte past ASCII mangled between git's output and the report.
#[test]
fn a_path_with_a_space_and_a_byte_past_ascii_is_read() {
    let repo = Repo::bare_bones(Refs::Files);
    repo.git(&["worktree", "add", "-q", "-b", "odd", "../\u{e9}t\u{e9} one"]);

    let scan = repo.scan();

    assert_eq!(
        scan.fact_at("\u{e9}t\u{e9} one", "worktree.config_gate"),
        FactValue::Enum("clear")
    );
}

/// Catches a bare main refused for having no checkout: its worktrees are found through
/// their own `.git` files, and belong to it all the same.
#[test]
fn a_bare_main_lists_its_worktrees() {
    let repo = Repo::bare_bones(Refs::Files);
    let bare = repo.temp.path().join("bare.git");
    let bare_text = bare.display().to_string();
    repo.git(&["clone", "-q", "--bare", ".", &bare_text]);
    repo.git_in(
        &bare,
        &["worktree", "add", "-q", "-b", "wt", "../projects/wt"],
    );

    let scan = repo.scan();

    assert_eq!(scan.fact("worktree.linked"), FactValue::Bool(true));
    assert_eq!(
        scan.fact("repository_identity"),
        FactValue::Enum("verified")
    );
}

/// Catches a `--separate-git-dir` repository never being listed: its `.git` file names no
/// `worktrees` directory, so a worktree of it that is gone had no finding and no omission.
#[test]
fn a_separate_git_dir_repository_is_listed() {
    let repo = Repo::bare_bones(Refs::Files);
    let split = repo.temp.path().join("projects/split");
    fs::create_dir(&split).unwrap();
    let git_dir = repo.temp.path().join("split.git").display().to_string();
    repo.git_in(
        &split,
        &["init", "-q", "-b", "main", "--separate-git-dir", &git_dir],
    );
    repo.git_in(&split, &["commit", "-q", "--allow-empty", "-m", "first"]);
    repo.git_in(&split, &["worktree", "add", "-q", "-b", "wt", "../gone"]);
    remove_dir(&repo.temp.path().join("projects/gone"));

    let scan = repo.scan();

    assert_eq!(
        scan.fact_at("gone", "registration_state"),
        FactValue::Enum(RegistrationState::RegisteredMissing.name())
    );
}

/// Catches a `.git` pointing at another worktree's admin entry read as this one: every
/// fact would describe the other checkout.
#[test]
fn a_gitdir_at_another_worktree_is_invalid() {
    let repo = Repo::new();
    repo.git(&["worktree", "add", "-q", "-b", "other", "../other"]);
    let other = repo.main.join(".git/worktrees/other");
    let line = format!("gitdir: {}\n", other.display());
    write(&repo.checkout.join(".git"), line.as_bytes());

    let scan = repo.scan();

    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredInvalid.name())
    );
}

/// Has the registration of `projects/<name>` print `projects/<target>` instead, in git's own
/// spelling of it, which is what makes two records print one path on every system.
fn registers_elsewhere(repo: &Repo, name: &str, target: &str) {
    let admins = repo.main.join(".git/worktrees");
    let names_target = admins.join(target).join("gitdir");
    fs::copy(names_target, admins.join(name).join("gitdir")).unwrap();
}

/// How many git findings the scan holds.
fn git_findings(scan: &Scan) -> usize {
    let findings = scan.0.findings.iter();
    findings.filter(|f| f.integration.0 == "git").count()
}

/// Catches a registration read from the worktree its `gitdir` names: both records print one
/// path, whose rows read clear, while the real checkout and its unsaved file have nothing
/// that says so. Git lists by admin name, so both orders are run.
#[test]
fn a_gitdir_naming_another_worktree_is_never_read_as_it() {
    for [name, target] in [["wt", "other"], ["other", "wt"]] {
        let repo = Repo::new();
        repo.git(&["worktree", "add", "-q", "-b", "other", "../other"]);
        write(&repo.projects().join(name).join("unsaved.txt"), b"work\n");
        registers_elsewhere(&repo, name, target);

        let scan = repo.scan();

        assert_eq!(
            scan.fact_at(target, "registration_state"),
            FactValue::Enum(RegistrationState::RegisteredInvalid.name()),
            "{name} names {target}"
        );
        assert_eq!(
            scan.fact_at(target, "worktree.config_gate"),
            FactValue::Unknown,
            "nothing was asked of {target}"
        );
        assert_eq!(
            scan.fact_at(name, "registration_state"),
            FactValue::Enum(RegistrationState::PresentUnregistered.name()),
            "{name} names {target}"
        );
        assert_eq!(git_findings(&scan), 2, "{name} names {target}");
    }
}

/// Catches rows read from an admin directory that isn't the record's: `inspect` starts from
/// the printed path, so a record of one worktree could borrow another's clear rows, and the
/// witness would join its head to the other's admin directory.
#[test]
fn a_record_of_another_admin_directory_is_invalid() {
    let repo = Repo::new();
    let env = repo.env();
    let cancel = CancelToken::new();
    let query = GitQuery::new(&env);
    let records = query.list(&repo.main, &cancel).unwrap();
    let record = records.last().unwrap();
    let invalid = Ok(RegistrationState::RegisteredInvalid);

    let on_another_branch = Registration {
        checkout: Checkout::Branch("other".to_owned()),
        ..record.clone()
    };
    let at_another_commit = Registration {
        checkout: Checkout::Detached,
        ..record.clone()
    };

    assert_eq!(
        query.inspect(record, &cancel).registration,
        Ok(RegistrationState::RegisteredPresent)
    );
    assert_eq!(
        query.inspect(&on_another_branch, &cancel).registration,
        invalid
    );
    assert_eq!(
        query.inspect(&at_another_commit, &cancel).registration,
        invalid
    );
}

/// Copies the directory `from` to `to` as `cp -a` does, for what a repository holds.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Catches a copy's stale registration read as present: the copy still lists the checkout,
/// whose rows all come from the repository it belongs to, so it had two present findings
/// and only one repository can remove it. The walk reaches a copy on either side of its
/// original.
#[test]
fn a_copied_repository_leaves_its_worktree_to_its_own_repository() {
    for name in ["aaa-copy", "zzz-copy"] {
        let repo = Repo::new();
        copy_tree(&repo.main, &repo.projects().join(name));

        let scan = repo.scan();

        assert_eq!(
            scan.fact("registration_state"),
            FactValue::Enum(RegistrationState::RegisteredPresent.name()),
            "beside {name}"
        );
        assert_eq!(
            scan.fact("repository_identity"),
            FactValue::Enum("verified"),
            "beside {name}"
        );
        assert_eq!(git_findings(&scan), 1, "beside {name}");
    }
}

/// Catches a stale record of the copy hiding the repository that owns the checkout: the
/// copy's branch is out of date, nothing else lists it in reach, and the worktree still
/// reads as what it is.
#[test]
fn a_copy_never_stands_in_for_a_repository_outside_the_roots() {
    let repo = Repo::new();
    let outside = repo.temp.path().join("outside/app");
    fs::create_dir_all(outside.parent().unwrap()).unwrap();
    rename(&repo.main, &outside);
    repo.git_in(&outside, &["worktree", "repair", "../../projects/wt"]);
    copy_tree(&outside, &repo.projects().join("app-copy"));
    repo.wt(&["switch", "-q", "-c", "later"]);

    let scan = repo.scan();

    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredPresent.name())
    );
    assert_eq!(
        scan.fact("repository_identity"),
        FactValue::Enum("verified")
    );
    assert_eq!(git_findings(&scan), 1);
}

/// What a test does to its repository.
type Change = fn(&Repo);

/// Each way a registration and a checkout can come to disagree, as a repository gets one.
const DISAGREEMENTS: [(&str, Change); 7] = [
    ("a copy", |repo| {
        copy_tree(&repo.main, &repo.projects().join("app-copy"));
    }),
    ("a copy that moved on", |repo| {
        copy_tree(&repo.main, &repo.projects().join("app-copy"));
        repo.wt(&["switch", "-q", "-c", "later"]);
    }),
    ("a copy of a checkout", |repo| {
        copy_tree(&repo.checkout, &repo.projects().join("wt-copy"));
    }),
    ("a gitdir at another worktree", |repo| {
        registers_elsewhere(repo, "other", "wt");
    }),
    ("a moved checkout", |repo| {
        rename(&repo.checkout, &repo.projects().join("moved"));
    }),
    ("a locked checkout gone beside a copy", |repo| {
        copy_tree(&repo.main, &repo.projects().join("a-copy"));
        repo.git(&["worktree", "lock", "../wt"]);
        remove_dir(&repo.checkout);
    }),
    (
        "a copy of a copy with a later worktree of its own",
        |repo| {
            let copy = repo.temp.path().join("x");
            copy_tree(&repo.main, &copy);
            repo.git_in(&copy, &["worktree", "add", "-q", "-b", "late", "../zz/z"]);
            copy_tree(&copy, &repo.projects().join("c"));
        },
    ),
];

/// Catches any path with two findings, which is two answers about one checkout, however
/// the registration and the checkout came apart.
#[test]
fn no_path_has_two_findings() {
    for (name, disagreement) in DISAGREEMENTS {
        let repo = Repo::new();
        repo.git(&["worktree", "add", "-q", "-b", "other", "../other"]);
        disagreement(&repo);

        let scan = repo.scan();

        let git = scan.0.findings.iter().filter(|f| f.integration.0 == "git");
        let paths: Vec<_> = git.map(|f| &f.path).collect();
        let unique: BTreeSet<_> = paths.iter().collect();
        assert_eq!(paths.len(), unique.len(), "{name}: {paths:?}");
    }
}

/// Every snippet the scan advises.
fn snippets(scan: &Scan) -> Vec<&str> {
    let advice = scan.0.integrations.iter().flat_map(|i| &i.advice);
    let snippets = advice.flat_map(|advice| &advice.snippets);
    snippets.map(String::as_str).collect()
}

/// Catches `repair` advised in a copy of a live worktree: run there, it takes the
/// registration from the original, whose checkout git then no longer knows.
#[test]
fn a_copy_of_a_live_worktree_is_never_told_to_repair() {
    let repo = Repo::new();
    copy_tree(&repo.checkout, &repo.projects().join("wt-copy"));

    let scan = repo.scan();

    assert_eq!(
        scan.fact_at("wt-copy", "registration_state"),
        FactValue::Enum(RegistrationState::PresentUnregistered.name())
    );
    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredPresent.name())
    );
    assert_eq!(snippets(&scan), [] as [&str; 0]);
}

/// Catches `repair` withheld from a checkout that really moved, because what its registration
/// names lay past the roots: only the platform can read that, and it says it is gone.
#[test]
fn a_checkout_that_moved_is_told_to_repair_wherever_its_repository_is() {
    let repo = Repo::new();
    let outside = repo.temp.path().join("outside/app");
    fs::create_dir_all(outside.parent().unwrap()).unwrap();
    rename(&repo.main, &outside);
    repo.git_in(&outside, &["worktree", "repair", "../../projects/wt"]);
    rename(&repo.checkout, &repo.projects().join("moved"));

    let scan = repo.scan();

    assert_eq!(
        scan.fact_at("moved", "registration_state"),
        FactValue::Enum(RegistrationState::PresentUnregistered.name())
    );
    assert!(
        snippets(&scan).contains(&"git worktree repair"),
        "repair is advised: {:?}",
        snippets(&scan)
    );
}

/// Catches a `.git` that is a link followed to a file elsewhere: a checkout's own `.git`
/// is a plain file, so one that isn't proves nothing.
#[cfg(unix)]
#[test]
fn a_git_link_is_no_present_registration() {
    let repo = Repo::new();
    let elsewhere = repo.temp.path().join("gitfile");
    let dot_git = repo.checkout.join(".git");
    fs::copy(&dot_git, &elsewhere).unwrap();
    remove_file(&dot_git);
    std::os::unix::fs::symlink(&elsewhere, &dot_git).unwrap();

    let scan = repo.scan();

    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredInvalid.name())
    );
}

#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "a fixture replaces a file below its temp root"
)]
fn remove_file(path: &Path) {
    fs::remove_file(path).unwrap();
}

#[expect(
    clippy::disallowed_methods,
    reason = "a fixture removes a directory below its temp root, as a person would"
)]
fn remove_dir(path: &Path) {
    fs::remove_dir_all(path).unwrap();
}

#[expect(
    clippy::disallowed_methods,
    reason = "a fixture moves a directory below its temp root, as a person would"
)]
fn rename(from: &Path, to: &Path) {
    fs::rename(from, to).unwrap();
}

/// Catches a lock lost once the directory is gone: a lock on a missing checkout usually
/// means a disk that isn't mounted, and prune must leave it.
#[test]
fn a_locked_checkout_that_is_gone_stays_locked() {
    let repo = Repo::new();
    repo.git(&["worktree", "lock", "../wt"]);
    remove_dir(&repo.checkout);

    let scan = repo.scan();

    assert_eq!(scan.fact("worktree.locked"), FactValue::Bool(true));
    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredMissing.name())
    );
}

/// Catches a copy's unlocked record hiding the original's lock: nothing can be placed at a
/// missing checkout, so nothing says which repository it belongs to, and a person is
/// pointed at `prune` with no sign of the lock.
#[test]
fn a_lock_outlives_a_copy_that_has_none() {
    let repo = Repo::new();
    copy_tree(&repo.main, &repo.projects().join("a-copy"));
    repo.git(&["worktree", "lock", "../wt"]);
    remove_dir(&repo.checkout);

    let scan = repo.scan();

    assert_eq!(scan.fact("worktree.locked"), FactValue::Bool(true));
    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredMissing.name())
    );
}

/// Catches a repository that moved leaving its checkouts unreported: each `.git` file
/// points at nothing, so no repository lists them, and they are still someone's work.
#[test]
fn a_repository_moved_out_of_reach_leaves_its_worktree_unregistered() {
    let repo = Repo::new();
    rename(&repo.main, &repo.temp.path().join("moved"));

    let scan = repo.scan();

    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::PresentUnregistered.name())
    );
}

/// Catches a checkout dropped when its lost repository lay outside every root: what its
/// `.git` names can't be read through a root, so only the platform can say it is gone.
#[test]
fn a_checkout_whose_repository_lay_outside_every_root_is_unregistered() {
    let repo = Repo::bare_bones(Refs::Files);
    let outside = repo.temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let away = outside.join("app");
    let away_text = away.display().to_string();
    repo.git(&["clone", "-q", ".", &away_text]);
    repo.git_in(
        &away,
        &["worktree", "add", "-q", "-b", "far", "../../projects/far"],
    );
    repo.git_in(
        &away,
        &["worktree", "add", "-q", "-b", "near", "../../projects/near"],
    );
    rename(&away, &outside.join("gone"));

    let scan = repo.scan();

    for name in ["far", "near"] {
        assert_eq!(
            scan.fact_at(name, "registration_state"),
            FactValue::Enum(RegistrationState::PresentUnregistered.name()),
            "{name}"
        );
    }
    let omissions = scan.0.integrations.iter().flat_map(|i| &i.omissions);
    assert_eq!(omissions.count(), 0, "its one finding says what is known");
    let advice = scan.0.integrations.iter().flat_map(|i| &i.advice);
    let snippets = advice.flat_map(|advice| &advice.snippets);
    assert_eq!(snippets.count(), 0, "neither link leads anywhere to repair");
}

/// Catches a checkout found twice when its repository moved inside a root: the moved
/// repository still lists it, so it has its registration's one finding, which disagrees
/// with what is on disk.
#[test]
fn a_repository_moved_in_reach_keeps_one_finding_per_worktree() {
    let repo = Repo::new();
    rename(&repo.main, &repo.temp.path().join("projects/moved"));

    let scan = repo.scan();

    assert_eq!(
        scan.fact("registration_state"),
        FactValue::Enum(RegistrationState::RegisteredInvalid.name())
    );
}

/// The project root `projects`, reached as `<temp>/link/projects` through a link to the
/// temp dir, as a root below a linked home or macOS's `/var` is: git, asked from there,
/// prints every path it resolved, which is outside the root as the scan opened it.
#[cfg(unix)]
fn through_a_link(repo: &Repo) -> PathBuf {
    let link = repo.temp.path().join("link");
    std::os::unix::fs::symlink(repo.temp.path(), &link).unwrap();
    link.join("projects")
}

#[cfg(unix)]
fn linked_root(repo: &Repo, root: &Path) -> Scan {
    let mut roots = Roots::default();
    let projects = SymbolicRoot::ProjectRoots;
    let source = kipple_core::RootSource::CommandLine;
    let resolved = kipple_core::ResolvedRoot::new(projects, root.to_owned(), source);
    roots.add(projects, Ok(resolved));
    scan_of(&repo.env(), roots)
}

/// Catches every worktree read as unread on a linked root, the default on macOS's temp
/// dirs and on a `~/projects` that is a link: git prints the resolved path, outside the
/// root, so the worktree was only reported, and apart from the artifacts inside it.
#[cfg(unix)]
#[test]
fn worktree_under_a_linked_root_is_found_by_the_roots_spelling() {
    let repo = Repo::new();
    let root = through_a_link(&repo);

    let scan = linked_root(&repo, &root);

    let git: Vec<_> = scan
        .0
        .findings
        .iter()
        .filter(|f| f.integration.0 == "git")
        .collect();
    assert_eq!(git.len(), 1, "one finding: {git:?}");
    let finding = git.first().unwrap();
    assert_eq!(finding.path, root.join("wt"), "the root's spelling");
    assert_ne!(finding.rule.0, "builtin.git.worktree-anomalies");
    assert!(
        finding.size.is_some(),
        "it was read and measured: {finding:?}"
    );
    let facts = &finding.facts.as_ref().unwrap().supplied.values;
    assert_eq!(
        facts.get("registration_state"),
        Some(&FactValue::Enum(
            RegistrationState::RegisteredPresent.name()
        ))
    );
    assert_eq!(
        facts.get("worktree.unclassified_ignored"),
        Some(&FactValue::Count(0))
    );
}

/// Catches a copy standing for the worktree whose `.git` it hard-links: the file is the
/// worktree's own, so its identity matches, but the registration names another directory,
/// and the copy's own state would be shown at the worktree's place, while the worktree
/// itself had no finding.
#[cfg(unix)]
#[test]
fn a_hard_linked_git_file_never_stands_for_the_worktree() {
    let repo = Repo::new();
    let copy = repo.projects().join("aside/wt");
    fs::create_dir_all(&copy).unwrap();
    fs::hard_link(repo.checkout.join(".git"), copy.join(".git")).unwrap();
    let root = through_a_link(&repo);

    let scan = linked_root(&repo, &root);

    let at = |path: &Path| {
        let mut found = scan.0.findings.iter().filter(|f| f.path == path);
        let finding = found.next().unwrap();
        assert!(found.next().is_none(), "one finding at {}", path.display());
        *finding
            .facts
            .as_ref()
            .unwrap()
            .supplied
            .values
            .get("registration_state")
            .unwrap()
    };
    assert_eq!(
        at(&root.join("wt")),
        FactValue::Enum(RegistrationState::RegisteredPresent.name())
    );
    assert_eq!(
        at(&root.join("aside/wt")),
        FactValue::Enum(RegistrationState::PresentUnregistered.name())
    );
}
