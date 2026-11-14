//! `cargo xtask demo-home` and `cargo xtask verify-demo` (`docs/11-delivery-plan.md`,
//! M1, M3b): the shared demo tree, and the end-to-end check of the built `kipple scan
//! --json` and `kipple clean` against it with every root redirected into it.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::cmd;
use crate::demo_home;

/// Builds `demo-home` in `dir`, which must be missing or empty, and writes the roots
/// file that points every root into it and fixes the clock at the time the demo is
/// looked at, and `path.txt`, the `PATH` a run on it gets: a directory holding the host's
/// git and nothing else, so no other program of the host's can start.
pub(crate) fn build(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    if fs::read_dir(dir)?.next().is_some() {
        bail!("{} is not empty", dir.display());
    }
    let now = demo_home::looked_at();
    demo_home::tree(now)
        .build_in(dir)
        .with_context(|| format!("could not build demo-home in {}", dir.display()))?;
    let mut roots = String::new();
    for (name, relative) in demo_home::ROOTS {
        writeln!(roots, "{name} = {}", dir.join(relative).display())?;
    }
    let seconds = now.duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
    writeln!(roots, "now = {seconds}")?;
    fs::write(dir.join("roots.txt"), roots)?;
    let path = demo_home::path(dir).context("could not give demo-home its PATH")?;
    let mut path = path.into_encoded_bytes();
    path.push(b'\n');
    fs::write(dir.join("path.txt"), path)?;
    Ok(())
}

/// `dir` as git would print it, so the demo's roots are spelled as git spells its
/// worktrees. A scan maps git's spelling back to the walk's only for a checkout git builds
/// a witness for, and all but one of the demo's have no commit, while its
/// registered-missing one has nothing on disk to map to (docs/04 §11). Links are resolved, since macOS keeps its temp
/// directory below one, and on Windows the long names of a path the runner gives in short
/// ones. Windows resolves to a verbatim path, which its tools don't all read, so a drive's
/// loses that prefix and any other is left as given.
fn physical(dir: &Path) -> PathBuf {
    let Ok(resolved) = fs::canonicalize(dir) else {
        return dir.to_owned();
    };
    let Some(text) = resolved.to_str() else {
        return dir.to_owned();
    };
    match text.strip_prefix(r"\\?\") {
        None => resolved,
        Some(rest) if rest.starts_with("UNC") => dir.to_owned(),
        Some(rest) => PathBuf::from(rest),
    }
}

/// The cargo flags of the build whose roots can be redirected into `demo-home`.
const FEATURES: [&str; 3] = ["--features", "test-roots", "--locked"];

/// Lints and tests the `test-roots` build, then scans `demo-home` with the built binary
/// at each carefulness level and compares each result with its committed snapshot for
/// this platform, then cleans the demo's Cargo targets.
pub(crate) fn verify(root: &Path) -> Result<()> {
    cmd::run(
        cmd::command("cargo", root)
            .args(["clippy", "-p", "kipple", "--all-targets"])
            .args(FEATURES)
            .args(["--", "-D", "warnings"]),
    )?;
    cmd::run(
        cmd::command("cargo", root)
            .args(["nextest", "run", "-p", "kipple"])
            .args(FEATURES),
    )?;
    let binary = build_kipple(root)?;
    let temp = tempfile::tempdir()?;
    let demo = physical(temp.path()).join("demo");
    build(&demo)?;
    let kipple = Built::new(binary, root, &demo)?;
    // Balanced is the default, so its scan names no config and reads the demo's own
    // config directory, which has none. The other levels come from a config file in the
    // run's temp dir: never the runner's.
    snapshot(&kipple, None, "demo-scan")?;
    for level in ["careful", "thorough"] {
        let config = temp.path().join(format!("{level}.toml"));
        fs::write(&config, format!("[care]\nlevel = \"{level}\"\n"))?;
        snapshot(&kipple, Some(&config), &format!("demo-scan-{level}"))?;
    }
    clean(&kipple)
}

/// Builds kipple with the `test-roots` feature and returns where cargo put the binary.
fn build_kipple(root: &Path) -> Result<PathBuf> {
    let messages = cmd::stdout(
        cmd::command("cargo", root)
            .args([
                "build",
                "-p",
                "kipple",
                "--message-format",
                "json-render-diagnostics",
            ])
            .args(FEATURES),
    )?;
    let built = messages
        .split(|&byte| byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .filter(|message| text(message, "/target/name") == Some("kipple"))
        .find_map(|message| text(&message, "/executable").map(PathBuf::from));
    built.context("cargo built no kipple binary")
}

/// The `test-roots` build of kipple, run on one demo with the demo's `PATH`. `cargo run`
/// can't be given a `PATH` of its own, since cargo itself is found through it.
struct Built<'demo> {
    /// The built binary.
    binary: PathBuf,
    root: &'demo Path,
    demo: &'demo Path,
    path: OsString,
}

impl<'demo> Built<'demo> {
    fn new(binary: PathBuf, root: &'demo Path, demo: &'demo Path) -> Result<Self> {
        let path = demo_home::path(demo).context("could not give demo-home its PATH")?;
        Ok(Self {
            binary,
            root,
            demo,
            path,
        })
    }

    /// `kipple` on the demo, with every root in it, its `PATH`, and no config of the
    /// runner's.
    fn command(&self) -> Command {
        let mut command = cmd::command(&self.binary, self.root);
        command
            .env("KIPPLE_TEST_ROOTS", self.demo.join("roots.txt"))
            .env("PATH", &self.path)
            .env_remove("KIPPLE_CONFIG");
        command
    }
}

/// The rule `verify` cleans with.
const CARGO_TARGET: &str = "builtin.cargo.target";

/// Runs `clean --only builtin.cargo.target --yes` on the demo: it must remove exactly the
/// Safe Cargo targets a scan finds and store a run, `history` must list that run, and a
/// second `clean` must find nothing to clean.
fn clean(kipple: &Built<'_>) -> Result<()> {
    let scan = json(kipple, &["scan", "--json"])?;
    let mut safe: Vec<PathBuf> = array(&scan, "findings")?
        .iter()
        .filter(|finding| {
            text(finding, "/rule") == Some(CARGO_TARGET)
                && text(finding, "/verdict/bucket") == Some("safe")
        })
        .filter_map(|finding| text(finding, "/path/display").map(PathBuf::from))
        .collect();
    safe.sort();
    if safe.is_empty() {
        bail!("the scan found no Safe Cargo target in demo-home");
    }
    let only = ["clean", "--only", CARGO_TARGET, "--yes", "--json"];
    let receipt = json(kipple, &only)?;
    let mut removed = Vec::new();
    for item in array(&receipt, "items")? {
        if text(item, "/outcome/status") != Some("done") {
            bail!("clean didn't finish an item: {item}");
        }
        removed.extend(text(item, "/path").map(PathBuf::from));
    }
    removed.sort();
    if removed != safe {
        let refused = receipt.get("refused").unwrap_or(&Value::Null);
        bail!("clean removed {removed:?}, not the Safe Cargo targets {safe:?}; refused: {refused}");
    }
    if let Some(left) = safe.iter().find(|path| path.symlink_metadata().is_ok()) {
        bail!(
            "clean said it removed {} but it is still there",
            left.display()
        );
    }
    let Some(run) = text(&receipt, "/run") else {
        bail!("clean stored no run");
    };
    let history = json(kipple, &["history", "--json"])?;
    if !array(&history, "runs")?
        .iter()
        .any(|listed| text(listed, "/id") == Some(run))
    {
        bail!("history doesn't list the run {run}");
    }
    let again = json(kipple, &only)?;
    if !array(&again, "items")?.is_empty() {
        bail!("a second clean found something: {again}");
    }
    Ok(())
}

/// Runs the built binary on its demo with `args` and returns the JSON it printed.
fn json(kipple: &Built<'_>, args: &[&str]) -> Result<Value> {
    let output = cmd::stdout(kipple.command().args(args))?;
    serde_json::from_slice(&output)
        .with_context(|| format!("`kipple {}` printed no JSON", args.join(" ")))
}

/// The array `key` of `value`.
fn array<'value>(value: &'value Value, key: &str) -> Result<&'value [Value]> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .with_context(|| format!("no `{key}` array in {value}"))
}

/// The string at `pointer` in `value`.
fn text<'value>(value: &'value Value, pointer: &str) -> Option<&'value str> {
    value.pointer(pointer).and_then(Value::as_str)
}

/// Scans the demo with the built binary, with `config` when given, and compares the
/// result with the committed snapshot `name` for this platform.
fn snapshot(kipple: &Built<'_>, config: Option<&Path>, name: &str) -> Result<()> {
    let mut command = kipple.command();
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    let output = cmd::stdout(command.args(["scan", "--json"]))?;
    let mut report: Value = serde_json::from_slice(&output).context("the scan printed no JSON")?;
    let prefix = kipple.demo.display().to_string();
    let mut outside = Vec::new();
    normalize(&mut report, &prefix, &mut outside);
    if !outside.is_empty() {
        bail!(
            "the scan read paths outside demo-home: {}",
            outside.join(", ")
        );
    }
    let platform = report
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    insta::assert_snapshot!(
        format!("{name}-{platform}"),
        serde_json::to_string_pretty(&report)?
    );
    Ok(())
}

/// Makes `value` the same on every machine: the demo's location becomes `<demo>` with
/// `/` separators, and allocated sizes, which depend on the file system, are redacted.
/// Every path display that isn't inside the demo is collected in `outside`.
fn normalize(value: &mut Value, prefix: &str, outside: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                if key == "allocated" || key == "unique_reclaim" {
                    *item = Value::String("[redacted]".to_owned());
                    continue;
                }
                if let (true, Value::String(text)) = (key == "display", &*item)
                    && !text.starts_with(prefix)
                {
                    outside.push(text.clone());
                }
                normalize(item, prefix, outside);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize(item, prefix, outside);
            }
        }
        Value::String(text) => {
            if let Some(rest) = text.strip_prefix(prefix) {
                *text = format!("<demo>{}", rest.replace('\\', "/"));
            } else if text.contains(prefix) {
                *text = text.replace(prefix, "<demo>").replace('\\', "/");
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}
