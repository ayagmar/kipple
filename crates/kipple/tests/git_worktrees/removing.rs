//! `builtin.git.worktree-remove` behind pinned 17's predicates, on real
//! git: every gate fact holds the removal at review and again at apply, and a removal that
//! runs takes the checkout and nothing else. The removal ships on Linux alone, so a test
//! that removes returns early elsewhere; one that only judges runs Linux's rules anywhere.

use std::collections::BTreeSet;
use std::time::Duration;

use kipple_core::{
    Confirmed, Consequence, Cost, Ending, FactState, FactValue, Finding, Hold, ItemOutcome, Level,
    Loss, Modified, NativeKind, PlanMode, Platform, Policy, Proposal, Recovery, Skip, Verdict,
};
#[cfg(unix)]
use kipple_core::{Execution, NativeArg, PlanEntry};

use super::{
    CASES, Case, Head, Repo, Scan, UNREFERENCED, expiry_behind_include, left_behind, orig_head,
    reset_away, visited_after, write,
};
use crate::apply::{Approval, Between, REMOVE, removes_here};
use crate::repo::{Moment, Refs, listed};

/// The facts of `finding`, which was measured.
const fn facts(finding: &Finding) -> &kipple_core::Facts {
    finding.facts.as_ref().unwrap()
}

/// The facts the rule of `finding` finds wanting.
fn unmet(finding: &Finding) -> Vec<&'static str> {
    facts(finding)
        .unmet
        .iter()
        .map(|unmet| unmet.fact)
        .collect()
}

/// What each level makes of `finding`.
fn verdicts(finding: &Finding) -> Vec<Verdict> {
    Level::ALL
        .iter()
        .map(|level| {
            let policy = Policy {
                level: *level,
                ..Policy::default()
            };
            policy.verdict(&finding.rule, &finding.path, facts(finding))
        })
        .collect()
}

const fn is_review(verdict: &Verdict) -> bool {
    matches!(verdict, Verdict::Review(_))
}

/// Whether the branch `name` is still there.
pub(super) fn has_branch(repo: &Repo, name: &str) -> bool {
    let reference = format!("refs/heads/{name}");
    let args = ["rev-parse", "--verify", "-q", reference.as_str()];
    repo.run_git(&repo.main, &args).status.success()
}

/// Catches a predicate missing from the pack, or one that reads clear on the very state it
/// exists to stop: the clean worktree meets every predicate and is Review on Linux, and
/// each case of `each_gate_fact_is_supplied` holds it on the fact it names.
#[test]
fn each_gate_fact_holds() {
    let clean = Repo::new();
    let scan = clean.removal_on(Platform::Linux).scan();
    let finding = scan.finding("wt");
    assert_eq!(finding.rule.0, REMOVE);
    assert_eq!(unmet(finding), Vec::<&str>::new(), "a clean worktree");
    if removes_here() {
        assert!(
            verdicts(finding).iter().all(is_review),
            "{:?}",
            verdicts(finding)
        );
    }

    for case in CASES {
        let repo = Repo::new();
        (case.set)(&repo);

        let judged = repo.removal_on(Platform::Linux).scan();

        held_by(&judged, case);
    }
}

/// That the finding of `case` is held for its fact: by the predicate on it, or, for a
/// registration that isn't two-way, by being no removal at all.
fn held_by(scan: &Scan, case: &Case) {
    let finding = scan.finding(case.at);
    if finding.rule.0 == REMOVE {
        let unmet = unmet(finding);
        assert!(unmet.contains(&case.fact), "{}: {unmet:?}", case.name);
    } else {
        let proposal = &facts(finding).proposal;
        let removes_nothing = matches!(proposal, Proposal::Report | Proposal::Advice);
        assert!(removes_nothing, "{}: {proposal:?}", case.name);
    }
    if removes_here() {
        let verdicts = verdicts(finding);
        let protected = |verdict: &Verdict| matches!(verdict, Verdict::Protected(_));
        assert!(
            verdicts.iter().all(protected),
            "{}: {verdicts:?}",
            case.name
        );
    }
}

/// A side branch made and committed on in the worktree, squash-merged into its branch and
/// deleted: the side commit's branch reflog went with the branch, so only the worktree's
/// own reflog names it.
fn squashed_and_deleted(repo: &Repo) {
    repo.wt(&["switch", "-q", "-c", "side"]);
    commit_in_worktree(repo, "side.txt");
    repo.wt(&["switch", "-q", "wt"]);
    repo.wt(&["merge", "-q", "--squash", "side"]);
    repo.wt(&["commit", "-q", "-m", "squashed"]);
    repo.wt(&["branch", "-q", "-D", "side"]);
}

/// A commit of a new file `name` on the worktree's branch.
fn commit_in_worktree(repo: &Repo, name: &str) {
    write(&repo.checkout.join(name), b"x\n");
    repo.wt(&["add", "--", name]);
    repo.wt(&["commit", "-q", "-m", name]);
}

/// A commit on `main` and one on the worktree's branch, so the two have diverged.
fn diverged(repo: &Repo) {
    repo.commit("main.txt", b"m\n");
    commit_in_worktree(repo, "wt.txt");
}

/// An amend of a commit made in the worktree.
fn amend(repo: &Repo) {
    commit_in_worktree(repo, "a.txt");
    repo.wt(&["commit", "-q", "--amend", "-m", "amended"]);
}

fn rebase(repo: &Repo) {
    diverged(repo);
    repo.wt(&["rebase", "-q", "main"]);
}

fn pull_rebase(repo: &Repo) {
    diverged(repo);
    repo.wt(&["pull", "-q", "--rebase", ".", "main"]);
}

fn reset_back(repo: &Repo) {
    commit_in_worktree(repo, "a.txt");
    repo.wt(&["reset", "-q", "--hard", "HEAD~1"]);
}

/// Two plain commits a day and two days after the rest, so the worktree's newest reflog
/// lines are newer than the branch's line that names what the history replaced.
fn two_commits_later(repo: &Repo) {
    for (days, name) in [(1, "later.txt"), (2, "latest.txt")] {
        write(&repo.checkout.join(name), b"x\n");
        repo.wt(&["add", "--", name]);
        repo.wt_at(Moment::days_ahead(days), &["commit", "-q", "-m", name]);
    }
}

/// A merge that conflicts and is resolved, with rerere on: rerere leaves an empty
/// `MERGE_RR` in the admin directory.
fn rerere_merge(repo: &Repo) {
    repo.git(&["config", "rerere.enabled", "true"]);
    write(&repo.checkout.join("file.txt"), b"theirs\n");
    repo.wt(&["commit", "-q", "-a", "-m", "theirs"]);
    repo.commit("file.txt", b"ours\n");
    let output = repo.run_git(&repo.checkout, &["merge", "main"]);
    assert!(!output.status.success(), "the merge conflicts: {output:?}");
    write(&repo.checkout.join("file.txt"), b"both\n");
    repo.wt(&["add", "--", "file.txt"]);
    repo.wt(&["commit", "-q", "--no-edit"]);
}

/// Commits with a split index, which leaves `sharedindex.<id>` files beside the index.
fn split_index(repo: &Repo) {
    repo.git(&["config", "core.splitIndex", "true"]);
    commit_in_worktree(repo, "a.txt");
    commit_in_worktree(repo, "b.txt");
}

/// An ordinary history an agent's worktree may have, which loses nothing when removed.
struct History {
    name: &'static str,
    set: fn(&Repo),
}

const HISTORIES: &[History] = &[
    History {
        name: "plain commits",
        set: |repo| {
            commit_in_worktree(repo, "a.txt");
            commit_in_worktree(repo, "b.txt");
        },
    },
    History {
        name: "branch switch",
        set: |repo| {
            repo.wt(&["switch", "-q", "-c", "other"]);
            commit_in_worktree(repo, "other.txt");
            repo.wt(&["switch", "-q", "wt"]);
        },
    },
    History {
        name: "amend",
        set: amend,
    },
    History {
        name: "amend, then two commits",
        set: |repo| {
            amend(repo);
            two_commits_later(repo);
        },
    },
    History {
        name: "rebase",
        set: rebase,
    },
    History {
        name: "rebase, then two commits",
        set: |repo| {
            rebase(repo);
            two_commits_later(repo);
        },
    },
    History {
        name: "pull --rebase",
        set: pull_rebase,
    },
    History {
        name: "pull --rebase, then two commits",
        set: |repo| {
            pull_rebase(repo);
            two_commits_later(repo);
        },
    },
    History {
        name: "reset --hard HEAD~1",
        set: reset_back,
    },
    History {
        name: "reset --hard HEAD~1, then two commits",
        set: |repo| {
            reset_back(repo);
            two_commits_later(repo);
        },
    },
    History {
        name: "plain commits, reflog expiry behind a conditional include",
        set: |repo| {
            expiry_behind_include(repo);
            commit_in_worktree(repo, "a.txt");
            commit_in_worktree(repo, "b.txt");
        },
    },
    History {
        name: "rerere on, merge resolved",
        set: rerere_merge,
    },
    History {
        name: "split index",
        set: split_index,
    },
    History {
        name: "merge",
        set: |repo| {
            diverged(repo);
            repo.wt(&["merge", "-q", "--no-edit", "main"]);
        },
    },
    History {
        name: "fetch and stash",
        set: |repo| {
            let main = repo.main.display().to_string();
            repo.wt(&["fetch", "-q", &main, "main"]);
            write(&repo.checkout.join("file.txt"), b"stashed\n");
            repo.wt(&["stash", "-q"]);
        },
    },
    History {
        name: "cherry-pick",
        set: |repo| {
            repo.commit("main.txt", b"m\n");
            repo.wt(&["cherry-pick", "main"]);
        },
    },
    History {
        name: "a commit the main worktree's reflog also names",
        set: |repo| {
            left_behind(repo);
            let left = repo.wt(&["rev-parse", "HEAD@{1}"]);
            let left = String::from_utf8_lossy(&left).trim().to_owned();
            repo.git(&["checkout", "-q", "--detach", &left]);
            repo.git(&["checkout", "-q", "main"]);
        },
    },
];

/// Catches the gate holding a worktree whose removal loses nothing, and letting one go
/// whose removal makes gc drop sooner what the worktree's reflogs kept: after each ordinary
/// history every fact holds, since a commit it rewrote or reset away is named by a branch's
/// reflog line as new as the worktree's, and the real removal shrinks nothing. A failure
/// lists the admin directory, which an unknown entry would explain on another git.
#[test]
fn ordinary_histories_are_eligible() {
    for history in HISTORIES {
        let repo = Repo::new();
        (history.set)(&repo);

        let scan = repo.removal_on(Platform::Linux).scan();

        let finding = scan.finding("wt");
        let mut entries: Vec<String> = std::fs::read_dir(repo.admin())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort_unstable();
        assert_eq!(
            unmet(finding),
            Vec::<&str>::new(),
            "{}: admin directory {entries:?}",
            history.name
        );
        removal_shrinks_nothing(&repo, history.name);
    }
}

/// That the real removal of `repo`'s worktree, then `gc --prune=now`, leaves every commit
/// the worktree's reflogs named that git had: removing it must not shorten the time gc
/// keeps any commit. Only where the removal runs.
fn removal_shrinks_nothing(repo: &Repo, name: &str) {
    if !removes_here() {
        return;
    }
    let had: Vec<String> = logged_commits(repo)
        .into_iter()
        .filter(|id| present(repo, id))
        .collect();
    let removal = repo.removal();
    let scan = removal.scan();

    let ran = removal.remove(&scan, &[scan.finding("wt")]);

    assert!(
        matches!(ran.outcome(), ItemOutcome::Done(_)),
        "{name}: {ran:#?}"
    );
    repo.git(&["gc", "-q", "--prune=now"]);
    let gone: Vec<&String> = had.iter().filter(|id| !present(repo, id)).collect();
    assert_eq!(gone, Vec::<&String>::new(), "{name}: gone after gc");
}

/// Every commit id in either column of the worktree's reflogs.
fn logged_commits(repo: &Repo) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let mut pending = vec![repo.admin().join("logs")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let columns = text.lines().flat_map(|line| line.split(' ').take(2));
            ids.extend(
                columns
                    .filter(|id| !id.bytes().all(|byte| byte == b'0'))
                    .map(str::to_owned),
            );
        }
    }
    ids
}

/// Whether git has the object `id`.
fn present(repo: &Repo, id: &str) -> bool {
    let args = ["cat-file", "-e", id];
    repo.run_git(&repo.main, &args).status.success()
}

/// Catches the gate letting a worktree go because a branch's reflog names the commit it
/// keeps, though that line is older than the worktree's own and gc expires each line by
/// its time: a commit reset away on the branch 40 or 29 days ago and visited today on a
/// detached `HEAD`. The worktree is held, and the hold is real: the same history with the
/// worktree removed by plain git loses the commit to the expiry gc runs two days later,
/// which keeps it while the worktree is there.
#[test]
fn a_commit_an_older_branch_line_names_holds_it() {
    for days in [40, 29] {
        let kept = Repo::new();
        let abandoned = visited_after(&kept, days);

        let scan = kept.removal_on(Platform::Linux).scan();

        let finding = scan.finding("wt");
        assert_eq!(failed(finding), vec![UNREFERENCED], "{days} days");
        let removed = Repo::new();
        let lost = visited_after(&removed, days);
        removed.wt(&["worktree", "remove", "."]);
        for repo in [&kept, &removed] {
            repo.git(&[
                "reflog",
                "expire",
                "--all",
                "--expire-unreachable=28.days.ago",
            ]);
            repo.git(&["gc", "-q", "--prune=now"]);
        }
        assert!(
            present(&kept, &abandoned),
            "{days} days: kept with the worktree"
        );
        assert!(!present(&removed, &lost), "{days} days: lost without it");
    }
}

/// A way to leave a commit only the worktree's admin directory names, and the revision
/// that names it there.
struct Lost {
    name: &'static str,
    set: fn(&Repo),
    names: &'static str,
}

/// Catches a commit only the worktree's admin directory names lost with it, as git leaves
/// one after a commit on a detached `HEAD` and a switch or reset away, or a reset whose
/// commit only `ORIG_HEAD` names: git shows no branch holds it, and the worktree is held on
/// that fact alone.
#[test]
fn a_commit_only_the_worktree_names_holds_it() {
    let states = [
        Lost {
            name: "switched back",
            set: left_behind,
            names: "HEAD@{1}",
        },
        Lost {
            name: "reset away",
            set: reset_away,
            names: "HEAD@{1}",
        },
        Lost {
            name: "ORIG_HEAD",
            set: orig_head,
            names: "ORIG_HEAD",
        },
        Lost {
            name: "squash-merged side branch, deleted",
            set: squashed_and_deleted,
            names: "HEAD@{2}",
        },
    ];
    for Lost { name, set, names } in states {
        let repo = Repo::new();
        set(&repo);
        let lost = repo.wt(&["rev-parse", names]);
        let lost = String::from_utf8_lossy(&lost).trim().to_owned();
        let holding = repo.git(&["for-each-ref", "--contains", &lost, "refs/heads/"]);
        assert_eq!(holding, b"", "{name}: no branch holds {lost}");

        let scan = repo.removal_on(Platform::Linux).scan();

        let finding = scan.finding("wt");
        let unreferenced = scan.fact_at("wt", "worktree.unreferenced_commits");
        assert_eq!(unreferenced, FactValue::Count(1), "{name}");
        assert_eq!(
            failed(finding),
            vec!["worktree.unreferenced_commits"],
            "{name}"
        );
    }
}

/// A worktree of a Node project whose dependencies are installed: `node_modules/` is
/// ignored, and Node's rule removes it.
fn with_dependencies() -> Repo {
    let repo = Repo::bare_bones(Refs::Files);
    repo.commit(".gitignore", b"node_modules/\n");
    repo.commit("package.json", b"{\"name\":\"web\"}");
    repo.commit("package-lock.json", b"{\"lockfileVersion\":3}");
    repo.add_worktree();
    let lock = repo.checkout.join("node_modules/.package-lock.json");
    write(&lock, b"{\"lockfileVersion\":3}");
    repo
}

/// Catches an agent worktree with installed dependencies held as unclassified: its
/// `node_modules` is a finding a rule removes, so it is no file git alone would lose.
#[test]
fn clean_worktree_with_a_classified_ignored_artifact_is_eligible() {
    let repo = with_dependencies();

    let scan = repo.removal_on(Platform::Linux).scan();

    let finding = scan.finding("wt");
    assert_eq!(
        scan.fact("worktree.unclassified_ignored"),
        FactValue::Count(0)
    );
    assert_eq!(unmet(finding), Vec::<&str>::new());
    if removes_here() {
        assert!(
            verdicts(finding).iter().all(is_review),
            "{:?}",
            verdicts(finding)
        );
    }
}

/// Catches the old-git job passing on a git other than the floor it is there to prove: that
/// job sets `KIPPLE_EXPECT_GIT`, and then a stale build or a `PATH` that finds another git
/// fails here, since every test in this binary finds git on `PATH` as this one does. A run
/// that doesn't set it proves nothing about the floor, and this test says nothing there.
#[test]
fn gate_ran_on_the_expected_git() {
    let Some(expected) = std::env::var_os("KIPPLE_EXPECT_GIT") else {
        return;
    };
    let expected = expected.to_string_lossy().into_owned();
    // The oldest git the removal is proven on (`WORKTREE_REMOVE_OLDEST` in the integration).
    assert_eq!(expected, "2.39.5", "the step builds the removal floor");

    let found = Repo::bare_bones(Refs::Files).real_git().version;

    assert_eq!(
        found.split_whitespace().next(),
        Some(expected.as_str()),
        "{found}"
    );
}

/// The removal's arguments in the plan the person saw.
#[cfg(unix)]
#[expect(clippy::panic, reason = "an item that isn't native fails the test")]
pub(super) fn reviewed_argv(ran: &crate::apply::Ran) -> Vec<NativeArg> {
    let entries = ran.plan.entries();
    assert_eq!(entries.len(), 1, "one item: {:?}", ran.plan);
    let Some(Execution::Native(spec)) = entries.first().map(PlanEntry::execution) else {
        panic!("a native item: {:?}", ran.plan);
    };
    spec.command.argv()
}

/// Catches a removal that deletes the branch with the checkout: the worktree's branch, and
/// what it holds, stay.
#[test]
fn branch_survives_removal() {
    if !removes_here() {
        return;
    }
    let repo = Repo::new();
    repo.wt(&["commit", "-q", "--allow-empty", "-m", "only on wt"]);
    let head = repo.wt(&["rev-parse", "HEAD"]);
    let removal = repo.removal();
    let scan = removal.scan();

    let ran = removal.remove(&scan, &[scan.finding("wt")]);

    assert!(matches!(ran.outcome(), ItemOutcome::Done(_)), "{ran:#?}");
    assert!(!repo.checkout.exists(), "the checkout went");
    assert!(has_branch(&repo, "wt"), "the branch stayed");
    assert_eq!(repo.git(&["rev-parse", "refs/heads/wt"]), head);
}

/// The change a case makes to the reviewed worktree at `projects/wt`. The unborn case makes
/// another worktree; after review it is this one whose HEAD goes unborn.
fn after_review(case: &Case) -> fn(&Repo) {
    if case.at == "wt" {
        case.set
    } else {
        |repo| {
            repo.wt(&["checkout", "-q", "--orphan", "new"]);
        }
    }
}

/// Catches a fact checked only at review: each state that holds a worktree, reached after
/// the person said yes, holds it at apply for that fact, and the checkout stays as it was
/// left. A fresh scan shows the state reads unmet on the case's fact alone, so the item
/// held for an unmet fact was held for it. A case reached by moving `HEAD` may be caught by
/// the witness first, and a stopped rebase leaves changes beside the operation.
#[test]
fn each_gate_fact_rechecked_at_apply() {
    if !removes_here() {
        return;
    }
    for case in CASES {
        let repo = Repo::new();
        let removal = repo.removal();
        let scan = removal.scan();
        let finding = scan.finding("wt");
        assert_eq!(unmet(finding), Vec::<&str>::new(), "{}", case.name);

        let set = after_review(case);
        let changed = repo.clone();
        let ran = removal.remove_changed(&scan, &[finding], Box::new(move || set(&changed)));

        let now = repo.removal().scan();
        held_at_apply(case, ran.outcome(), now.finding("wt"));
        assert!(repo.checkout.join("file.txt").exists(), "{}", case.name);
        assert!(repo.checkout.join(".git").exists(), "{}", case.name);
    }
}

/// That `outcome` is the hold `case` causes, `now` being the worktree scanned afresh in the
/// state the case left. A row that stops git's questions, a submodule's, leaves the rows
/// after it unknown beside it.
#[expect(clippy::panic, reason = "an item that isn't held fails the test")]
fn held_at_apply(case: &Case, outcome: &ItemOutcome, now: &Finding) {
    let witnessed =
        case.head == Head::Moves && *outcome == ItemOutcome::Skipped(Skip::WitnessChanged);
    if witnessed {
        return;
    }
    let ItemOutcome::Skipped(Skip::Protected(holds)) = outcome else {
        panic!("{}: {outcome:?}", case.name);
    };
    // Which ignored paths a rule removes only a scan settles, so a set that changed since
    // is unknown at apply. A registration rewritten one way is listed at the path it names
    // now, so the one reviewed is gone and nothing of it is known.
    let unknown = case.value == FactValue::Unknown;
    let hold = match case.fact {
        "worktree.unclassified_ignored" | "registration_state" => Hold::FactUnknown,
        _ if unknown => Hold::FactUnknown,
        _ => Hold::FactUnmet,
    };
    let by_facts = holds
        .iter()
        .all(|held| matches!(held, Hold::FactUnmet | Hold::FactUnknown));
    assert!(
        holds.contains(&hold) && by_facts,
        "{}: {holds:?}",
        case.name
    );
    if now.rule.0 != REMOVE {
        assert_eq!(
            case.fact, "registration_state",
            "{}: {:?}",
            case.name, now.rule
        );
        return;
    }
    let failed = failed(now);
    if case.head == Head::Stays {
        let mut expected: Vec<&str> = std::iter::once(case.fact)
            .filter(|_| !unknown)
            .chain(case.also.iter().copied())
            .collect();
        expected.sort_unstable();
        let mut failed = failed;
        failed.sort_unstable();
        assert_eq!(failed, expected, "{}", case.name);
    } else if unknown {
        assert!(unmet(now).contains(&case.fact), "{}: {failed:?}", case.name);
    } else {
        assert!(failed.contains(&case.fact), "{}: {failed:?}", case.name);
    }
}

/// The facts of `finding` that are known and not what its rule accepts.
fn failed(finding: &Finding) -> Vec<&'static str> {
    facts(finding)
        .unmet
        .iter()
        .filter(|unmet| unmet.state == FactState::Failed)
        .map(|unmet| unmet.fact)
        .collect()
}

/// Catches a registration replaced under the same path read as the one reviewed: the
/// facts all read clear again, and only the witness tells the two apart.
#[test]
fn witness_differs_for_a_replaced_registration() {
    if !removes_here() {
        return;
    }
    let repo = Repo::new();
    let removal = repo.removal();
    let scan = removal.scan();

    let changed = repo.clone();
    let ran = removal.remove_changed(
        &scan,
        &[scan.finding("wt")],
        Box::new(move || {
            changed.git(&["worktree", "remove", "../wt"]);
            changed.git(&["worktree", "add", "-q", "../wt", "wt"]);
        }),
    );

    assert_eq!(ran.outcome(), &ItemOutcome::Skipped(Skip::WitnessChanged));
    assert!(
        repo.checkout.join("file.txt").exists(),
        "the checkout stays"
    );
}

/// Catches a clean checkout moved to another commit read as the one reviewed: it is still
/// clean and its HEAD still reachable, and only the witness's HEAD differs.
#[test]
fn witness_differs_for_a_clean_reachable_head_change() {
    if !removes_here() {
        return;
    }
    let repo = Repo::new();
    let removal = repo.removal();
    let scan = removal.scan();

    let changed = repo.clone();
    let ran = removal.remove_changed(
        &scan,
        &[scan.finding("wt")],
        Box::new(move || {
            changed.wt(&["commit", "-q", "--allow-empty", "-m", "moved on"]);
        }),
    );

    assert_eq!(ran.outcome(), &ItemOutcome::Skipped(Skip::WitnessChanged));
    assert!(
        repo.checkout.join("file.txt").exists(),
        "the checkout stays"
    );
}

/// Catches a file added inside an ignored directory after review: `status` prints
/// `node_modules/` and stops there, so only the comparison of every leaf sees it. The
/// apply freezes the tree before it reads the worktree again, so a file added after that
/// freeze is the window 04 §11 names, which no check before the spawn closes.
#[test]
fn file_added_inside_a_classified_artifact_drops_the_item() {
    if !removes_here() {
        return;
    }
    let repo = with_dependencies();
    let removal = repo.removal();
    let scan = removal.scan();
    let added = repo.checkout.join("node_modules/added.js");

    let file = added.clone();
    let ran = removal.remove_after(
        &scan,
        &[scan.finding("wt")],
        Box::new(move || {
            write(&file, b"// new\n");
        }),
    );

    assert!(
        !matches!(ran.outcome(), ItemOutcome::Done(_)),
        "{:?}",
        ran.outcome()
    );
    assert!(added.exists(), "the added file stays");
    assert!(listed(&repo, "wt"), "and so does the worktree");
}

/// Catches a registration that went between review and apply read as the one reviewed:
/// what git lists then is matched by the path it printed, and nothing names it.
#[test]
fn revisit_matches_by_printed_path_and_drops_a_gone_registration() {
    if !removes_here() {
        return;
    }
    let repo = Repo::new();
    let removal = repo.removal();
    let scan = removal.scan();

    let admin = repo.admin();
    let ran = removal.remove_changed(
        &scan,
        &[scan.finding("wt")],
        Box::new(move || {
            super::remove_dir(&admin);
        }),
    );

    assert!(
        matches!(ran.outcome(), ItemOutcome::Skipped(_)),
        "{:?}",
        ran.outcome()
    );
    assert!(
        repo.checkout.join("file.txt").exists(),
        "the checkout stays"
    );

    #[cfg(unix)]
    {
        // Below a link, git prints a path outside the root: the item still finds its record.
        let linked = Repo::new();
        let root = super::through_a_link(&linked);
        let through = linked.removal().projects_at(root.clone());
        let read = through.scan();
        let finding = read.finding("wt");
        assert_eq!(finding.path, root.join("wt"));

        let done = through.remove(&read, &[finding]);

        assert!(matches!(done.outcome(), ItemOutcome::Done(_)), "{done:#?}");
        assert!(!linked.checkout.exists(), "the checkout went");
        assert!(!listed(&linked, "wt"), "git no longer lists it");
    }
}

/// Catches the sync of a registration directory git removed with its last worktree read
/// as a failure, or one left out: with a worktree left and with none, the removal is done,
/// which needs every sync to have succeeded.
#[test]
fn sync_covers_the_parent_and_registration_dirs() {
    if !removes_here() {
        return;
    }
    for others in [1, 0] {
        let repo = Repo::new();
        if others == 1 {
            repo.git(&["worktree", "add", "-q", "-b", "other", "../other"]);
        }
        let removal = repo.removal();
        let scan = removal.scan();

        let ran = removal.remove(&scan, &[scan.finding("wt")]);

        assert!(
            matches!(ran.outcome(), ItemOutcome::Done(_)),
            "{others} left: {ran:#?}"
        );
        assert_eq!(
            repo.main.join(".git/worktrees").exists(),
            others == 1,
            "{others} left"
        );
    }
}

/// Catches a removal git gave up on halfway read as done or as untouched: git drops the
/// registration and leaves the files it couldn't delete, and the outcome says nobody
/// knows what is left.
#[cfg(unix)]
#[test]
fn partial_removal_is_indeterminate() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    if !removes_here() {
        return;
    }
    let repo = Repo::bare_bones(Refs::Files);
    write(&repo.main.join("kept/file.txt"), b"kept\n");
    repo.git(&["add", "--", "kept"]);
    repo.git(&["commit", "-q", "-m", "kept"]);
    repo.add_worktree();
    let kept = repo.checkout.join("kept");
    let mode = |bits| fs::Permissions::from_mode(bits);
    fs::set_permissions(&kept, mode(0o555)).unwrap();
    let removal = repo.removal();
    let scan = removal.scan();

    let ran = removal.remove(&scan, &[scan.finding("wt")]);

    fs::set_permissions(&kept, mode(0o755)).unwrap();
    assert!(
        matches!(ran.outcome(), ItemOutcome::Indeterminate(_)),
        "{:?}",
        ran.outcome()
    );
    assert!(
        kept.join("file.txt").exists(),
        "what git couldn't delete stays"
    );
    assert_ne!(ran.finished.ending, Ending::Done);
}

/// Catches a worktree and an artifact inside it removed in one plan: the artifact would be
/// gone before the worktree's leaves are compared, or its removal would race git's.
#[test]
fn parent_child_selection_conflicts() {
    let repo = with_dependencies();
    let removal = repo.removal_on(Platform::Linux);
    let scan = removal.scan();
    let dependencies = repo.checkout.join("node_modules");
    let artifact = scan.0.findings.iter().find(|f| f.path == dependencies);
    let artifact = artifact.unwrap_or_else(|| panic!("found in {:?}", scan.0.findings));

    let ran = removal.run(
        &scan,
        &[scan.finding("wt"), artifact],
        Approval::PERSON,
        Between::Review(Box::new(|| {})),
    );

    assert_eq!(ran.plan.entries().len(), 0, "{:#?}", ran.plan);
    assert_eq!(ran.finished.refused.len(), 2, "{:#?}", ran.finished.refused);
    assert!(
        dependencies.join(".package-lock.json").exists(),
        "the artifact stays"
    );
    assert!(
        repo.checkout.join("file.txt").exists(),
        "the checkout stays"
    );
}

/// Catches a git reading the user's own config or hooks while kipple asks it or removes
/// with it: the test's home has a global config whose hooks and filter would leave a
/// marker, and nothing of it runs.
#[cfg(unix)]
#[test]
fn user_git_config_and_hooks_are_not_read() {
    if !removes_here() {
        return;
    }
    let repo = Repo::bare_bones(Refs::Files);
    repo.commit(".gitattributes", b"*.txt filter=mark\n");
    repo.add_worktree();
    let marker = repo.temp.path().join("marker");
    let hooks = repo.temp.path().join("home/hooks");
    let script = format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display());
    for hook in [
        "post-index-change",
        "reference-transaction",
        "post-checkout",
    ] {
        super::programs::executable(&hooks.join(hook), &script);
    }
    let mark = repo.temp.path().join("mark");
    super::programs::executable(&mark, &script);
    let global = format!(
        "[core]\n\thooksPath = {}\n[filter \"mark\"]\n\tclean = {}\n\tsmudge = {}\n",
        hooks.display(),
        mark.display(),
        mark.display()
    );
    write(&repo.temp.path().join("home/.gitconfig"), global.as_bytes());
    write(&repo.temp.path().join("home/git/config"), global.as_bytes());
    let removal = repo.removal();
    let scan = removal.scan();

    let ran = removal.remove(&scan, &[scan.finding("wt")]);

    assert!(matches!(ran.outcome(), ItemOutcome::Done(_)), "{ran:#?}");
    assert!(!marker.exists(), "nothing of the user's config ran");
}

/// Catches the removal failing its own Verify line: a registered, clean worktree reviewed
/// in the TUI is gone, with its registration, and its repository and the worktree beside
/// it are as they were.
#[test]
fn registered_worktree_removed_end_to_end() {
    if !removes_here() {
        return;
    }
    let repo = Repo::new();
    repo.git(&["worktree", "add", "-q", "-b", "other", "../other"]);
    let removal = repo.removal();
    let scan = removal.scan();

    let ran = removal.remove(&scan, &[scan.finding("wt")]);

    assert!(matches!(ran.outcome(), ItemOutcome::Done(_)), "{ran:#?}");
    assert_eq!(ran.finished.ending, Ending::Done);
    assert!(!repo.checkout.exists(), "the checkout went");
    assert!(!repo.admin().exists(), "its registration went");
    assert!(!listed(&repo, "wt"), "git no longer lists it");
    assert!(listed(&repo, "other"), "the other worktree is still listed");
    assert!(has_branch(&repo, "wt"), "the branch stays");
    assert_eq!(repo.git(&["status", "--porcelain"]), b"");
}

/// Catches the last worktree's removal read as unconfirmed: git removes `<common>/worktrees`
/// with it, which the runner's sync and `list` must both take as done.
#[test]
fn removing_the_last_worktree_is_done() {
    if !removes_here() {
        return;
    }
    let repo = Repo::new();
    let removal = repo.removal();
    let scan = removal.scan();

    let ran = removal.remove(&scan, &[scan.finding("wt")]);

    assert!(matches!(ran.outcome(), ItemOutcome::Done(_)), "{ran:#?}");
    assert_eq!(ran.finished.ending, Ending::Done);
    assert!(!repo.checkout.exists(), "the checkout went");
    assert!(
        !repo.main.join(".git/worktrees").exists(),
        "git removed its worktrees directory"
    );
}

/// Catches a worktree removed without a person reviewing it in the TUI: it is Review at
/// every level, so `clean` at the prompt, `--yes`, a profile and a plan file each refuse
/// it, and automation can't run it from a plan that has it.
#[test]
fn worktree_removal_is_review_at_every_level_and_tui_only() {
    if !removes_here() {
        return;
    }
    let repo = Repo::new();
    let removal = repo.removal();
    let scan = removal.scan();
    let finding = scan.finding("wt");
    assert_eq!(
        facts(finding).proposal,
        Proposal::Native(NativeKind::GitWorktreeRemove)
    );
    // The consequence alone makes it Review: judged as a year old, so nothing recent does.
    let consequence = Consequence {
        recovery: Recovery::None,
        loss: &[Loss::History],
        cost: Cost::Unknown,
    };
    assert_eq!(facts(finding).consequence, Some(consequence));
    let mut aged = facts(finding).clone();
    aged.modified = Modified::Ago(Duration::from_hours(365 * 24));
    for level in Level::ALL {
        let verdict = Policy::at(*level).verdict(&finding.rule, &finding.path, &aged);
        assert!(is_review(&verdict), "{level:?}: {verdict:?}");
    }

    for mode in [PlanMode::Safe, PlanMode::SafeFiles, PlanMode::SafeOnly] {
        let between = Between::Review(Box::new(|| {}));
        let approval = Approval {
            mode,
            confirmed: Confirmed::Automation,
        };
        let ran = removal.run(&scan, &[finding], approval, between);
        assert_eq!(ran.plan.entries().len(), 0, "{mode:?}");
        assert_eq!(ran.finished.refused.len(), 1, "{mode:?}");
    }
    let between = Between::Review(Box::new(|| {}));
    let approval = Approval {
        mode: PlanMode::Review,
        confirmed: Confirmed::Automation,
    };
    let automated = removal.run(&scan, &[finding], approval, between);
    assert!(
        matches!(automated.outcome(), ItemOutcome::Skipped(_)),
        "{:?}",
        automated.outcome()
    );
    assert!(
        repo.checkout.join("file.txt").exists(),
        "the checkout stays"
    );
}

/// Catches the removal reaching an OS it was never proven on: on macOS and Windows the same
/// worktree is only reported.
#[test]
fn worktree_removal_only_reports_off_linux() {
    for platform in [Platform::Macos, Platform::Windows] {
        let repo = Repo::new();

        let scan = repo.removal_on(platform).scan();

        let finding = scan.finding("wt");
        assert_ne!(finding.rule.0, REMOVE, "{platform:?}");
        assert_eq!(facts(finding).proposal, Proposal::Report, "{platform:?}");
    }
}
