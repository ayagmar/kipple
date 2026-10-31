//! How a clean is confirmed, or isn't (spec M3c pinned 15, 16, D-070): Ctrl+C at the
//! question, and the typed size above `review.confirm_by_typing_above`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kipple::CANCELLED;
use kipple_core::CancelToken;

use super::{ACME, Asked, Demo, RUST_APP, TARGET, Terminal, json, present};

/// What is at a path in a tree: a file's bytes, a link's target, or a directory.
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    File(Vec<u8>),
    Link(PathBuf),
    Dir,
}

/// Every entry under `root`, by its path below it.
fn tree(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut entries = BTreeMap::new();
    let mut left = vec![root.to_owned()];
    while let Some(dir) = left.pop() {
        for child in fs::read_dir(&dir).unwrap() {
            let path = child.unwrap().path();
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            let entry = if kind.is_symlink() {
                Entry::Link(fs::read_link(&path).unwrap())
            } else if kind.is_dir() {
                left.push(path.clone());
                Entry::Dir
            } else {
                Entry::File(fs::read(&path).unwrap())
            };
            entries.insert(path.strip_prefix(root).unwrap().to_owned(), entry);
        }
    }
    entries
}

/// Catches Ctrl+C at the question waiting for Enter, or a cancelled question running
/// the plan or storing a run (pinned 15): the command exits 130, says nothing was
/// changed, and writes nothing, even with a "y" already typed.
#[test]
fn ctrl_c_at_the_question_exits_130_and_writes_nothing() {
    let demo = Demo::new();
    let before = tree(demo.temp.path());

    let ran = demo.run_with(
        &["clean", "--only", TARGET],
        Asked::Person(Terminal::interrupted("y\n")),
        &CancelToken::new(),
    );

    assert_eq!(ran.code, ExitCode::from(CANCELLED), "{}", ran.err);
    assert!(ran.err.contains("Cancelled at the question"), "{}", ran.err);
    assert_eq!(
        tree(demo.temp.path()),
        before,
        "no run, lock or item changed"
    );
}

/// The config file in `demo` that asks for the size typed above `above`.
fn typing_above(demo: &Demo, above: &str) -> String {
    let config = demo.at("config.toml");
    let text = format!("[review]\nconfirm_by_typing_above = \"{above}\"\n");
    fs::write(&config, text).unwrap();
    config.to_str().unwrap().to_owned()
}

/// The size the prompt on `err` asks to have typed.
fn size_asked(err: &str) -> &str {
    let (_, rest) = err.split_once("Type ").unwrap();
    let (size, _) = rest.split_once(" to confirm").unwrap();
    size
}

/// Catches `y` confirming a clean above the threshold, or the size shown not confirming
/// it (pinned 16): `y` declines and says it isn't the size, and the size as shown, typed
/// without spaces in another case, runs the plan.
#[test]
fn above_the_threshold_only_the_typed_size_confirms() {
    let demo = Demo::new();
    let config = typing_above(&demo, "1 KB");
    let args = ["--config", &config, "clean", "--only", TARGET];

    let said_y = demo.answer(&args, "y\n");

    assert_eq!(said_y.code, ExitCode::from(3), "{}", said_y.err);
    assert!(
        said_y.err.contains("isn't the size shown"),
        "{}",
        said_y.err
    );
    assert!(present(&demo.at(ACME)), "acme's target stays");

    let size = size_asked(&said_y.err).replace(' ', "").to_uppercase();
    let typed = demo.answer(&args, &format!("{size}\n"));

    assert_eq!(typed.code, ExitCode::SUCCESS, "{}", typed.err);
    assert!(!present(&demo.at(ACME)), "acme's target is gone");
    assert!(!present(&demo.at(RUST_APP)), "rust-app's target is gone");
}

/// Catches the threshold asking below it (pinned 16): a plan under it is confirmed with
/// `y`.
#[test]
fn below_the_threshold_y_confirms() {
    let demo = Demo::new();
    let config = typing_above(&demo, "1 TB");

    let ran = demo.answer(&["--config", &config, "clean", "--only", TARGET], "y\n");

    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    assert!(!ran.err.contains("Type "), "{}", ran.err);
    assert!(!present(&demo.at(ACME)), "acme's target is gone");
}

/// Catches the threshold blocking automation (pinned 16, J3's automation exception):
/// `--yes` ignores it.
#[test]
fn yes_ignores_the_threshold() {
    let demo = Demo::new();
    let config = typing_above(&demo, "1 KB");
    let args = [
        "--config", &config, "clean", "--only", TARGET, "--yes", "--json",
    ];

    let ran = demo.run(&args);

    assert_eq!(ran.code, ExitCode::SUCCESS, "{}", ran.err);
    assert_eq!(json(&ran.out)["ending"], "done");
}
