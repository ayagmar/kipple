//! Spike S2 (throwaway, branch `spike/m1-s2-s3` only): running-executable identity with
//! `sysinfo` versus the direct OS APIs.
//!
//! Usage:
//! - `s2-procid snapshot [iterations]`: probe every process both ways, classify each
//!   outcome, and time both.
//! - `s2-procid scenarios`: deleted or replaced executables, exited processes and a
//!   process the caller may not inspect.
//! - `s2-procid sleep <secs>` and `s2-procid exit`: children for the scenarios.
#![expect(
    clippy::disallowed_methods,
    clippy::print_stdout,
    reason = "throwaway spike: spawns children, sleeps, cleans its temp dir and prints a report"
)]

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod direct;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod direct;
#[cfg(windows)]
#[path = "windows.rs"]
mod direct;

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// A file's identity: device and inode on Unix, volume serial and file index on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileId {
    pub(crate) volume: u64,
    pub(crate) file: u64,
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}:{}", self.volume, self.file)
    }
}

/// What probing one PID found.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// The running image's path and file identity. `deleted`: the file has no links left.
    Identified {
        path: PathBuf,
        id: FileId,
        deleted: bool,
    },
    /// A path, but no identity for it: the file is gone, replaced or unreadable.
    PathOnly { path: PathBuf, error: io::Error },
    /// The OS refused to tell us.
    Denied(io::Error),
    /// The process has exited but is still visible (zombie, or a handle keeps it).
    Exited(&'static str),
    /// The PID vanished between listing and probing.
    Gone,
    /// A process with no executable file (kernel thread, idle, system).
    NoExecutable(&'static str),
    /// Anything else.
    Other(io::Error),
}

impl Outcome {
    /// For "is this release running?": what the safety model must make of this outcome.
    const fn verdict(&self) -> &'static str {
        match self {
            Self::Identified { .. } => "known",
            Self::Exited(_) | Self::Gone => "not running",
            Self::NoExecutable(_) => "irrelevant",
            Self::PathOnly { .. } | Self::Denied(_) | Self::Other(_) => "UNKNOWN",
        }
    }

    const fn kind(&self) -> &'static str {
        match self {
            Self::Identified { deleted: false, .. } => "identified",
            Self::Identified { deleted: true, .. } => "identified (deleted file)",
            Self::PathOnly { .. } => "path only",
            Self::Denied(_) => "denied",
            Self::Exited(_) => "exited",
            Self::Gone => "gone",
            Self::NoExecutable(_) => "no executable",
            Self::Other(_) => "other error",
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identified { path, id, deleted } => {
                write!(f, "identified {} id={id} deleted={deleted}", path.display())
            }
            Self::PathOnly { path, error } => {
                write!(f, "path only {} ({error})", path.display())
            }
            Self::Denied(error) => write!(f, "denied ({error})"),
            Self::Exited(how) => write!(f, "exited ({how})"),
            Self::Gone => write!(f, "gone"),
            Self::NoExecutable(why) => write!(f, "no executable ({why})"),
            Self::Other(error) => write!(f, "other error ({error})"),
        }
    }
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("snapshot") => {
            let iterations = args
                .get(2)
                .map_or(Ok(11), |n| n.parse().map_err(io::Error::other))?;
            snapshot(iterations)
        }
        Some("scenarios") => scenarios(),
        Some("sleep") => {
            let secs = args
                .get(2)
                .map_or(Ok(30), |n| n.parse().map_err(io::Error::other))?;
            std::thread::sleep(Duration::from_secs(secs));
            Ok(())
        }
        Some("exit") => Ok(()),
        _ => Err(io::Error::other(
            "usage: s2-procid <snapshot|scenarios|sleep|exit>",
        )),
    }
}

/// Every PID probed directly, in PID order.
fn probe_all() -> io::Result<BTreeMap<u32, Outcome>> {
    Ok(direct::list_pids()?
        .into_iter()
        .map(|pid| (pid, direct::probe(pid)))
        .collect())
}

/// `sysinfo`'s view: PID to executable path, refreshed from scratch.
fn sysinfo_exes() -> HashMap<u32, Option<PathBuf>> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_exe(UpdateKind::Always),
    );
    system
        .processes()
        .iter()
        .map(|(pid, process)| (pid.as_u32(), process.exe().map(Path::to_path_buf)))
        .collect()
}

/// The identity `sysinfo` users would get: `stat` on the reported path.
fn sysinfo_ids(exes: &HashMap<u32, Option<PathBuf>>) -> usize {
    exes.values()
        .flatten()
        .filter(|path| direct::file_id(path).is_ok())
        .count()
}

fn snapshot(iterations: usize) -> io::Result<()> {
    println!("os: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    if let Some(note) = direct::inventory_note() {
        println!("inventory: {note}");
    }

    let outcomes = probe_all()?;
    let exes = sysinfo_exes();
    report_outcomes(&outcomes);
    report_sysinfo(&outcomes, &exes);

    let direct_ms = median_ms(iterations, || probe_all().map(drop))?;
    let sysinfo_ms = median_ms(iterations, || {
        sysinfo_exes();
        Ok(())
    })?;
    let sysinfo_stat_ms = median_ms(iterations, || {
        sysinfo_ids(&sysinfo_exes());
        Ok(())
    })?;
    println!(
        "timing, median of {iterations}: direct (path + identity) {direct_ms:.2} ms, \
         sysinfo (path) {sysinfo_ms:.2} ms, sysinfo + stat(path) {sysinfo_stat_ms:.2} ms"
    );
    Ok(())
}

fn report_outcomes(outcomes: &BTreeMap<u32, Outcome>) {
    let mut kinds: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for outcome in outcomes.values() {
        *kinds
            .entry((outcome.verdict(), outcome.kind()))
            .or_default() += 1;
    }
    println!("direct: {} processes", outcomes.len());
    for ((verdict, kind), count) in &kinds {
        println!("  {count:>5}  {kind:<26} -> {verdict}");
    }
    for (pid, outcome) in outcomes
        .iter()
        .filter(|(_, o)| o.verdict() == "UNKNOWN")
        .take(8)
    {
        println!("  example pid {pid}: {outcome}");
    }
}

/// Where `sysinfo` says "no executable", what was actually going on.
fn report_sysinfo(outcomes: &BTreeMap<u32, Outcome>, exes: &HashMap<u32, Option<PathBuf>>) {
    let with_exe = exes.values().filter(|e| e.is_some()).count();
    println!(
        "sysinfo: {} processes, {with_exe} with an exe path, {} with an identity via stat(path)",
        exes.len(),
        sysinfo_ids(exes)
    );
    let mut hidden: BTreeMap<&str, usize> = BTreeMap::new();
    for (pid, exe) in exes {
        if exe.is_none() {
            let kind = outcomes
                .get(pid)
                .map_or("not in direct snapshot", Outcome::kind);
            *hidden.entry(kind).or_default() += 1;
        }
    }
    println!("  sysinfo exe() == None, by direct outcome:");
    for (kind, count) in &hidden {
        println!("  {count:>5}  {kind}");
    }
}

fn median_ms(iterations: usize, mut run: impl FnMut() -> io::Result<()>) -> io::Result<f64> {
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        run()?;
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    Ok(samples.get(samples.len() / 2).copied().unwrap_or_default())
}

fn scenarios() -> io::Result<()> {
    let dir = std::env::temp_dir().join(format!("s2-procid-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let result = run_scenarios(&dir);
    fs::remove_dir_all(&dir)?;
    result
}

fn run_scenarios(dir: &Path) -> io::Result<()> {
    println!("os: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    deleted_executable(dir)?;
    replaced_executable(dir)?;
    exited_process()?;
    for (pid, label) in direct::privileged_pids() {
        compare(&format!("privileged process ({label})"), pid);
    }
    Ok(())
}

/// A copy of this binary at `path`, started with `sleep 30`.
fn start_copy(path: &Path) -> io::Result<Child> {
    fs::copy(std::env::current_exe()?, path)?;
    let child = Command::new(path).args(["sleep", "30"]).spawn()?;
    std::thread::sleep(Duration::from_millis(300));
    Ok(child)
}

fn stop(mut child: Child) -> io::Result<()> {
    child.kill()?;
    child.wait().map(drop)
}

/// The running file is unlinked (Unix) or, where that is refused, renamed (Windows).
fn deleted_executable(dir: &Path) -> io::Result<()> {
    let path = dir.join(format!("deleted{}", std::env::consts::EXE_SUFFIX));
    let child = start_copy(&path)?;
    compare("running, file in place", child.id());
    match fs::remove_file(&path) {
        Ok(()) => compare("running, file deleted", child.id()),
        Err(error) => {
            println!("[running, file deleted] delete refused: {error}");
            fs::rename(&path, dir.join("renamed.bin"))?;
            compare("running, file renamed away", child.id());
        }
    }
    stop(child)
}

/// An in-place upgrade: the running file is replaced by a different file at the same path.
fn replaced_executable(dir: &Path) -> io::Result<()> {
    let path = dir.join(format!("replaced{}", std::env::consts::EXE_SUFFIX));
    let child = start_copy(&path)?;
    let moved = dir.join("old.bin");
    if let Err(error) = fs::remove_file(&path) {
        println!("[replaced in place] delete refused ({error}), renaming instead");
        fs::rename(&path, &moved)?;
    }
    fs::write(&path, b"a different release")?;
    let new_id = direct::file_id(&path)?;
    println!("[replaced in place] the new file at the path has id {new_id}");
    compare("running, replaced in place", child.id());
    stop(child)
}

/// A child that has exited, before and after it is reaped.
fn exited_process() -> io::Result<()> {
    let mut child = Command::new(std::env::current_exe()?).arg("exit").spawn()?;
    std::thread::sleep(Duration::from_millis(500));
    compare("exited, not yet reaped", child.id());
    let pid = child.id();
    child.wait()?;
    compare("exited and reaped", pid);
    Ok(())
}

fn compare(label: &str, pid: u32) {
    let exes = sysinfo_exes();
    let from_sysinfo = match exes.get(&pid) {
        None => "not listed".to_owned(),
        Some(None) => "exe() == None".to_owned(),
        Some(Some(path)) => format!(
            "exe() == {} -> stat(path) id {}",
            path.display(),
            direct::file_id(path).map_or_else(|e| format!("error ({e})"), |id| id.to_string())
        ),
    };
    let outcome = direct::probe(pid);
    println!("[{label}] pid {pid}");
    println!("    direct:  {outcome} -> {}", outcome.verdict());
    println!("    sysinfo: {from_sysinfo}");
}
