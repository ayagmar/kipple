//! `kipple profile` and `clean --profile` (03 J6, spec M3c pinned 10–13, D-068): a
//! profile cleans only what it was made with, a change suspends it until it is reviewed,
//! and nothing is saved unless a person says yes at a terminal.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kipple::CANCELLED;
use kipple_core::{CancelToken, Platform};
use kipple_platform::PLATFORM;
use serde_json::Value;

use super::{
    ACME, Asked, Demo, RUST_APP, TARGET, Terminal, item_paths, items, json, present, text,
};

/// A rule other than [`TARGET`] that proposes removal on every OS.
const VENV: &str = "builtin.python.venv";

/// Saves the profile `name` as a person at a terminal who says yes, with `extra`
/// arguments before the command, such as `--config`.
fn create(demo: &Demo, extra: &[&str], name: &str, only: &[&str]) -> String {
    let mut args = extra.to_vec();
    args.extend(["profile", "create", name, "--from-safe"]);
    for rule in only {
        args.extend(["--only", rule]);
    }
    let ran = demo.answer(&args, "y\n");
    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    assert!(
        ran.err.contains(&format!("Save profile {name}? [y/N] ")),
        "{}",
        ran.err
    );
    ran.err
}

/// The paths of the Safe findings a scan of `demo` reports, sorted.
fn safe_now(demo: &Demo, extra: &[&str]) -> Vec<PathBuf> {
    let ran = demo.run(&[extra, &["scan", "--json"]].concat());
    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    let mut safe: Vec<PathBuf> = items(&json(&ran.out), "findings")
        .iter()
        .filter(|finding| text(finding, "/verdict/bucket") == "safe")
        .map(|finding| PathBuf::from(text(finding, "/path/display")))
        .collect();
    safe.sort();
    safe
}

/// The file the profile `name` is stored in, wherever this OS keeps kipple's data.
fn stored(demo: &Demo, name: &str) -> Option<PathBuf> {
    let file = format!("{name}.json");
    let mut left = vec![demo.temp.path().to_owned()];
    while let Some(dir) = left.pop() {
        for child in fs::read_dir(&dir).unwrap() {
            let path = child.unwrap().path();
            if path.is_dir() {
                left.push(path);
            } else if path.ends_with(Path::new("profiles").join(&file)) {
                return Some(path);
            }
        }
    }
    None
}

/// A config file in `demo` saying `text`, as `--config` takes it.
fn config(demo: &Demo, text: &str) -> String {
    let config = demo.at("config.toml");
    fs::write(&config, text).unwrap();
    config.to_str().unwrap().to_owned()
}

/// Catches a profile run cleaning more or less than what was Safe when it was made, or
/// one that doesn't say it was a profile run (J6, pinned 12, 13, 19): `create` then
/// `clean --profile safe --yes` removes exactly the Safe set a scan reported, the receipt
/// names the profile with nothing suspended, and a second run has nothing to clean.
#[test]
fn a_profile_cleans_exactly_the_safe_set_it_was_made_with() {
    let demo = Demo::new();
    let safe = safe_now(&demo, &[]);
    assert!(!safe.is_empty(), "demo-home has Safe findings");
    create(&demo, &[], "safe", &[]);

    let ran = demo.run(&["clean", "--profile", "safe", "--yes", "--json"]);

    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    let receipt = json(&ran.out);
    let mut removed = item_paths(&receipt);
    removed.sort();
    assert_eq!(removed, safe, "{receipt}");
    assert_eq!(receipt["profile"]["name"], "safe");
    assert_eq!(receipt["profile"]["suspended"], Value::Array(Vec::new()));
    assert!(!present(&demo.at(ACME)), "acme's target is gone");

    let again = demo.run(&["clean", "--profile", "safe", "--yes", "--json"]);
    assert_eq!(again.code, ExitCode::SUCCESS, "{}", again.err);
    assert_eq!(json(&again.out)["ending"], "nothing-to-clean");
}

/// Catches a profile run under a wider level than it was made with (J6, pinned 12, 13,
/// 17, docs/11 "Balanced to Thorough"): after `care.level` changes, the run exits 3,
/// removes nothing, names the change and how to review it, while its dry run names it
/// too and exits 0 as a preview; `profile review` saves the profile as it is now, and
/// then it runs again.
#[test]
fn a_changed_level_suspends_the_profile_until_it_is_reviewed() {
    let demo = Demo::new();
    create(&demo, &[], "safe", &[]);
    let thorough = config(&demo, "[care]\nlevel = \"thorough\"\n");
    let run = [
        "--config",
        &thorough,
        "clean",
        "--profile",
        "safe",
        "--yes",
        "--json",
    ];

    let suspended = demo.run(&run);

    assert_eq!(suspended.code, ExitCode::from(3), "{}", suspended.err);
    assert!(
        suspended.err.contains("its level changed"),
        "{}",
        suspended.err
    );
    assert!(
        suspended.err.contains("kipple profile review safe"),
        "{}",
        suspended.err
    );
    let receipt = json(&suspended.out);
    assert_eq!(receipt["ending"], "none-ran");
    assert!(
        !items(&receipt["profile"], "suspended").is_empty(),
        "the receipt lists the suspended rules: {receipt}"
    );
    assert!(present(&demo.at(ACME)), "acme's target stays");
    assert!(demo.history().is_empty(), "no run is stored");

    let previewed = demo.run(&[
        "--config",
        &thorough,
        "clean",
        "--profile",
        "safe",
        "--dry-run",
    ]);

    assert_eq!(previewed.code, ExitCode::SUCCESS, "{}", previewed.err);
    assert!(
        previewed.out.contains("Dry run: nothing was changed."),
        "{}",
        previewed.out
    );
    assert!(
        previewed.err.contains("its level changed"),
        "{}",
        previewed.err
    );

    let reviewed = demo.answer(&["--config", &thorough, "profile", "review", "safe"], "y\n");

    assert_eq!(reviewed.code, ExitCode::SUCCESS, "{}", reviewed.err);
    assert!(
        reviewed.err.contains("Profile safe changes:")
            && reviewed.err.contains("Saved profile safe."),
        "{}",
        reviewed.err
    );

    let ran = demo.run(&run);

    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    assert!(!present(&demo.at(ACME)), "acme's target is gone");
}

/// Catches a profile's trash authority leaking to `--only --yes`, or a trash profile
/// skipping what it was shown to move (pinned 3): with `clean.method = "trash"`,
/// `--only --yes` still refuses each folder as not reviewed whole, while a profile whose
/// creation showed the trash line moves them. Linux only: macOS's trash isn't driven
/// here, and Windows has none (D-062).
#[test]
fn a_trash_profile_moves_items_whole_while_only_yes_still_skips_them() {
    if PLATFORM != Platform::Linux {
        return;
    }
    let demo = Demo::new();
    let trash = config(&demo, "[clean]\nmethod = \"trash\"\n");
    let only = [
        "--config", &trash, "clean", "--only", TARGET, "--yes", "--json",
    ];

    let skipped = demo.run(&only);

    assert_eq!(skipped.code, ExitCode::from(3), "{}", skipped.err);
    assert!(present(&demo.at(ACME)), "acme's target stays");

    let shown = create(&demo, &["--config", &trash], "bins", &[TARGET]);
    assert!(
        shown.contains("trash move takes each folder whole"),
        "{shown}"
    );
    let ran = demo.run(&[
        "--config",
        &trash,
        "clean",
        "--profile",
        "bins",
        "--yes",
        "--json",
    ]);

    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    let receipt = json(&ran.out);
    let methods: Vec<&str> = items(&receipt, "items")
        .iter()
        .map(|item| text(item, "/method"))
        .collect();
    assert_eq!(methods, ["trash", "trash"], "{receipt}");
    assert!(
        !present(&demo.at(ACME)) && !present(&demo.at(RUST_APP)),
        "both moved"
    );
}

/// Catches a profile saved without a clear yes (pinned 13, 15): "n" saves nothing and
/// exits 3, and Ctrl+C at the question, with "y" already typed or typed after it while
/// the answer is read, exits 130 and saves nothing.
#[test]
fn nothing_is_saved_without_a_yes() {
    let demo = Demo::new();
    let create = ["profile", "create", "safe", "--from-safe"];

    let declined = demo.answer(&create, "n\n");
    let interrupted = demo.run_with(
        &create,
        Asked::Person(Terminal::interrupted("y\n")),
        &CancelToken::new(),
    );
    let reading = CancelToken::new();
    let while_read = demo.run_with(
        &create,
        Asked::Person(Terminal::interrupted_while_read("y\n", &reading)),
        &reading,
    );

    assert_eq!(declined.code, ExitCode::from(3), "{}", declined.err);
    assert!(
        declined.err.contains("Nothing was saved."),
        "{}",
        declined.err
    );
    for ran in [&interrupted, &while_read] {
        assert_eq!(ran.code, ExitCode::from(CANCELLED), "{}", ran.err);
        assert!(!ran.err.contains("Saved profile"), "{}", ran.err);
    }
    assert_eq!(stored(&demo, "safe"), None, "no profile file");
    let list = demo.run(&["profile", "list"]);
    assert_eq!(list.code, ExitCode::SUCCESS, "{}", list.err);
    assert!(list.out.contains("No profiles yet."), "{}", list.out);
}

/// Catches `create` replacing a profile, or `remove` of a name with no profile passing
/// for done (pinned 13): a taken name exits 2, removing it works once, and the second
/// remove exits 2.
#[test]
fn create_refuses_a_taken_name_and_remove_an_unknown_one() {
    let demo = Demo::new();
    create(&demo, &[], "safe", &[]);

    let again = demo.answer(&["profile", "create", "safe", "--from-safe"], "y\n");
    let removed = demo.run(&["profile", "remove", "safe"]);
    let gone = demo.run(&["profile", "remove", "safe"]);

    assert_eq!(again.code, ExitCode::from(2), "{}", again.err);
    assert!(again.err.contains("exists"), "{}", again.err);
    assert!(
        !again.err.contains("Save profile"),
        "refused before it is shown: {}",
        again.err
    );
    assert_eq!(removed.code, ExitCode::SUCCESS, "{}", removed.err);
    assert_eq!(gone.code, ExitCode::from(2), "{}", gone.err);
}

/// Catches a profile a newer kipple wrote being run by this one, which can't know what
/// it means (pinned 11): it exits 2, says to upgrade, and removes nothing.
#[test]
fn a_profile_from_a_newer_kipple_is_refused() {
    let demo = Demo::new();
    create(&demo, &[], "safe", &[]);
    let file = stored(&demo, "safe").unwrap();
    let newer = fs::read_to_string(&file)
        .unwrap()
        .replace("kipple.profile/v1", "kipple.profile/v2");
    fs::write(&file, newer).unwrap();

    let ran = demo.run(&["clean", "--profile", "safe", "--yes"]);

    assert_eq!(ran.code, ExitCode::from(2), "{}", ran.err);
    assert!(ran.err.contains("newer kipple"), "{}", ran.err);
    assert!(present(&demo.at(ACME)), "acme's target stays");
}

/// Catches `review` widening a profile on its own (pinned 13): a rule `--only` left out
/// stays out, "nothing to review" writes nothing, and only `--add` includes it, shown
/// before the question.
#[test]
fn review_adds_a_left_out_rule_only_when_named() {
    let demo = Demo::new();
    create(&demo, &[], "bins", &[TARGET]);
    let file = stored(&demo, "bins").unwrap();
    let saved = fs::read(&file).unwrap();

    let unchanged = demo.answer(&["profile", "review", "bins"], "y\n");

    assert_eq!(unchanged.code, ExitCode::SUCCESS, "{}", unchanged.err);
    assert!(
        unchanged.err.contains("Nothing to review"),
        "{}",
        unchanged.err
    );
    assert_eq!(fs::read(&file).unwrap(), saved, "nothing was written");

    let added = demo.answer(&["profile", "review", "bins", "--add", VENV], "y\n");

    assert_eq!(added.code, ExitCode::SUCCESS, "{}", added.err);
    assert!(
        added.err.contains(&format!("{VENV}: added")),
        "{}",
        added.err
    );
    let list = demo.run(&["profile", "list"]);
    assert!(
        list.out.contains("bins  2 active, 0 suspended, 0 pending"),
        "{}",
        list.out
    );
}

/// Catches `review` widening a profile with a rule new since it was made (pinned 12,
/// 13): the rule stays pending, said and not saved, until `--add` names it.
#[test]
fn review_adds_a_pending_rule_only_when_named() {
    let demo = Demo::new();
    create(&demo, &[], "bins", &[TARGET]);
    let file = stored(&demo, "bins").unwrap();
    // The profile as an older kipple without the venv rule saved it.
    let mut older: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let entries = older["entries"].as_array_mut().unwrap();
    entries.retain(|entry| entry["rule"] != VENV);
    fs::write(&file, serde_json::to_vec(&older).unwrap()).unwrap();
    let saved = fs::read(&file).unwrap();

    let unchanged = demo.answer(&["profile", "review", "bins"], "y\n");

    assert_eq!(unchanged.code, ExitCode::SUCCESS, "{}", unchanged.err);
    assert!(
        unchanged.err.contains(&format!(
            "{VENV}: new since the profile was made, not added"
        )),
        "{}",
        unchanged.err
    );
    assert_eq!(fs::read(&file).unwrap(), saved, "nothing was written");

    let added = demo.answer(&["profile", "review", "bins", "--add", VENV], "y\n");

    assert_eq!(added.code, ExitCode::SUCCESS, "{}", added.err);
    assert!(
        added.err.contains(&format!("{VENV}: added")),
        "{}",
        added.err
    );
    let list = demo.run(&["profile", "list"]);
    assert!(
        list.out.contains("bins  2 active, 0 suspended, 0 pending"),
        "{}",
        list.out
    );
}

/// Stdout that replaces `file` with bytes that aren't a profile when the first thing is
/// written to it.
struct SpoilsOnWrite {
    file: PathBuf,
    spoiled: bool,
}

impl Write for SpoilsOnWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.spoiled {
            fs::write(&self.file, b"not a profile")?;
            self.spoiled = true;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Catches a profile run reading its profile again once the scan is done (pinned 13: it
/// is read once, before the scan): the file replaced while the scan streams its first
/// event, before anything is planned, doesn't stop the run, which cleans what the
/// profile it read holds.
#[test]
fn a_profile_replaced_during_the_scan_runs_as_it_was_read() {
    let demo = Demo::new();
    create(&demo, &[], "bins", &[TARGET]);
    let file = stored(&demo, "bins").unwrap();
    let mut stdout = SpoilsOnWrite {
        file: file.clone(),
        spoiled: false,
    };

    let ran = demo.run_on(
        &["clean", "--profile", "bins", "--yes", "--events", "jsonl"],
        Asked::Nobody,
        &CancelToken::new(),
        &mut stdout,
    );

    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    assert!(stdout.spoiled, "replaced during the scan");
    assert_eq!(fs::read(&file).unwrap(), b"not a profile");
    assert!(
        !present(&demo.at(ACME)) && !present(&demo.at(RUST_APP)),
        "both cleaned"
    );
}

/// Catches a profile run without `--yes` skipping the person, or running on a pipe
/// (pinned 13): at a terminal it shows the plan and runs on "y".
#[test]
fn a_profile_run_without_yes_asks_first() {
    let demo = Demo::new();
    create(&demo, &[], "bins", &[TARGET]);

    let declined = demo.answer(&["clean", "--profile", "bins"], "n\n");
    assert_eq!(declined.code, ExitCode::from(3), "{}", declined.err);
    assert!(present(&demo.at(ACME)), "acme's target stays");

    let ran = demo.answer(&["clean", "--profile", "bins"], "y\n");

    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    assert!(ran.err.contains("Clean 2 items? [y/N]"), "{}", ran.err);
    assert!(!present(&demo.at(ACME)), "acme's target is gone");
}

/// Catches a profile being saved or run with nobody to confirm it, or a name that could
/// reach outside the profile folder being used (pinned 11, 13): each exits before
/// anything is scanned or opened, nothing is written, and the profile run says to pass
/// `--yes`.
#[test]
fn without_a_person_or_a_good_name_nothing_is_saved_or_run() {
    let demo = Demo::new();

    for (args, code, says) in [
        (
            &["profile", "create", "safe", "--from-safe"][..],
            3,
            "needs a terminal",
        ),
        (&["profile", "review", "safe"], 3, "needs a terminal"),
        (&["clean", "--profile", "safe"], 3, "pass --yes"),
        (
            &["profile", "create", "../safe", "--from-safe"],
            2,
            "isn't a profile name",
        ),
        (
            &["clean", "--profile", "Safe", "--yes"],
            2,
            "isn't a profile name",
        ),
        (&["profile", "remove", "a/b"], 2, "isn't a profile name"),
    ] {
        let ran = demo.run(args);

        assert_eq!(ran.code, ExitCode::from(code), "{args:?}: {}", ran.err);
        assert!(ran.err.contains(says), "{args:?}: {}", ran.err);
    }
    assert!(present(&demo.at(ACME)), "acme's target stays");
    assert!(
        !present(&demo.at("home/.local/share/kipple")),
        "kipple's data folder wasn't made"
    );
}
