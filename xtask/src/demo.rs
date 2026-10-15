//! `cargo xtask demo-home` and `cargo xtask verify-demo` (`docs/11-delivery-plan.md`,
//! M1): the shared demo tree, and the end-to-end check of the built `kipple scan --json`
//! against it with every root redirected into it.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::cmd;
use crate::demo_home;

/// Builds `demo-home` in `dir`, which must be missing or empty, and writes the roots
/// file that points every root into it and fixes the clock at the time the demo is
/// looked at.
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
    Ok(())
}

/// Lints and tests the `test-roots` build, then scans `demo-home` with the built binary
/// and compares the result with the committed snapshot for this platform.
pub(crate) fn verify(root: &Path) -> Result<()> {
    let features = ["--features", "test-roots", "--locked"];
    cmd::run(
        cmd::command("cargo", root)
            .args(["clippy", "-p", "kipple", "--all-targets"])
            .args(features)
            .args(["--", "-D", "warnings"]),
    )?;
    cmd::run(
        cmd::command("cargo", root)
            .args(["nextest", "run", "-p", "kipple"])
            .args(features),
    )?;
    let temp = tempfile::tempdir()?;
    let demo = temp.path().join("demo");
    build(&demo)?;
    let output = cmd::stdout(
        cmd::command("cargo", root)
            .args(["run", "--quiet", "-p", "kipple"])
            .args(features)
            .args(["--", "scan", "--json"])
            .env("KIPPLE_TEST_ROOTS", demo.join("roots.txt"))
            // The config is the demo's own (none, so the defaults): never the runner's.
            .env_remove("KIPPLE_CONFIG"),
    )?;
    let mut report: Value = serde_json::from_slice(&output).context("the scan printed no JSON")?;
    let prefix = demo.display().to_string();
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
        format!("demo-scan-{platform}"),
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
