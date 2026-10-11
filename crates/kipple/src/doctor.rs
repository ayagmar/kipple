//! `kipple doctor` (`docs/03-functional-spec.md` §2): why a scan might be empty or partial.
//! It only reads: roots, tool versions and the process table.

use std::io::{self, Write};
use std::path::PathBuf;

use kipple_core::{
    EntryKind, EntryMeta, FsProbe as _, IntegrationId, Inventory, Lens, NativeTool, ProbeError,
    ProcessClass, ProcessProbe as _, ProcessSnapshot, ResolvedRoot, RootSource, SymbolicRoot,
    ToolStatus, UnresolvedRoot,
};
use kipple_platform::{
    CONFIG_FILE, Environment, KippleDir, KippleDirs, NoFollowFs, OsProcesses, PLATFORM,
    kipple_dirs, permission_hint, probe_tools, resolve_roots,
};
use schemars::JsonSchema;
use serde::Serialize;

use crate::human::escaped;
use crate::json::{DOCTOR, LensOut, PathOut, VERSION};
use crate::setup::Builtin;

/// Everything `doctor` found.
#[derive(Debug)]
pub struct Report {
    /// kipple's own directories.
    pub dirs: KippleDirs,
    /// The config file and what is there, when the config directory resolved.
    pub config_file: Option<(PathBuf, Result<EntryMeta, ProbeError>)>,
    /// Every root on this platform.
    pub roots: Vec<RootCheck>,
    /// Every built-in integration and the roots it reads.
    pub integrations: Vec<IntegrationRoots>,
    /// Every native tool kipple knows.
    pub tools: Vec<(NativeTool, ToolStatus)>,
    /// The process table, or why it could not be read.
    pub processes: Result<ProcessSnapshot, ProbeError>,
    /// What to suggest when a permission failure was diagnosed.
    pub permission_hint: Option<&'static str>,
}

/// One root and what is at its path.
#[derive(Debug)]
pub struct RootCheck {
    /// The root.
    pub root: SymbolicRoot,
    /// Where it resolved and what a no-follow probe found there, or why it has no path.
    pub status: Result<(ResolvedRoot, Result<EntryMeta, ProbeError>), UnresolvedRoot>,
}

/// A built-in integration and the roots it reads: its own and its rules'.
#[derive(Debug)]
pub struct IntegrationRoots {
    /// The integration.
    pub id: IntegrationId,
    /// Its lens.
    pub lens: Lens,
    /// Whether it runs on this platform.
    pub supported: bool,
    /// The roots it reads, each shown with its state under "Roots".
    pub roots: Vec<SymbolicRoot>,
}

/// Probes everything `doctor` reports.
#[must_use]
pub fn gather(env: &Environment, builtin: &Builtin) -> Report {
    let roots = resolve_roots(env, &[]);
    let dirs = kipple_dirs(&roots);
    let config_file = dirs.config.path.as_ref().ok().map(|dir| {
        let file = dir.join(CONFIG_FILE);
        let meta = NoFollowFs.metadata(&file);
        (file, meta)
    });
    let checks = roots
        .iter()
        .map(|(root, resolution)| RootCheck {
            root,
            status: resolution.clone().map(|resolved| {
                let meta = NoFollowFs.metadata(&resolved.path);
                (resolved, meta)
            }),
        })
        .collect();
    Report {
        dirs,
        config_file,
        roots: checks,
        integrations: integration_roots(builtin),
        tools: probe_tools(env),
        processes: OsProcesses.snapshot(),
        permission_hint: permission_hint(),
    }
}

fn integration_roots(builtin: &Builtin) -> Vec<IntegrationRoots> {
    builtin
        .descriptors
        .iter()
        .map(|descriptor| {
            let mut roots = descriptor.roots.to_vec();
            let rules = builtin.rules().map(|(_, rule)| rule);
            let own = rules.filter(|rule| {
                rule.integration == descriptor.id && rule.platforms.contains(&PLATFORM)
            });
            roots.extend(own.map(|rule| rule.root));
            roots.sort_unstable();
            roots.dedup();
            IntegrationRoots {
                id: descriptor.id,
                lens: descriptor.lens,
                supported: descriptor.platforms.contains(&PLATFORM),
                roots,
            }
        })
        .collect()
}

impl Report {
    /// Writes the report for people. Fails as soon as `out` does.
    ///
    /// # Errors
    /// When writing to `out` fails, for example because it was closed.
    pub fn write_to(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(
            out,
            "kipple {} doctor (read-only)",
            env!("CARGO_PKG_VERSION")
        )?;
        self.write_dirs(out)?;
        self.write_roots(out)?;
        self.write_integrations(out)?;
        self.write_tools(out)?;
        self.write_processes(out)?;
        self.write_notes(out)?;
        out.flush()
    }

    fn write_dirs(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "\nkipple's directories")?;
        for (name, dir) in [
            ("config", &self.dirs.config),
            ("data", &self.dirs.data),
            ("cache", &self.dirs.cache),
        ] {
            writeln!(out, "  {name:<13}{}", escaped(&dir_line(dir)))?;
        }
        if let Some((file, meta)) = &self.config_file {
            let state = match meta {
                Ok(_) => "present".to_owned(),
                Err(e) if e.io_kind() == Some(io::ErrorKind::NotFound) => {
                    "not present, the defaults apply".to_owned()
                }
                Err(e) => format!("can't be read: {e}"),
            };
            let line = format!("{}: {state}", file.display());
            writeln!(out, "  {:<13}{}", "config file", escaped(&line))?;
        }
        Ok(())
    }

    fn write_roots(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "\nRoots")?;
        for check in &self.roots {
            let line = match &check.status {
                Err(unresolved) => format!("unknown: {unresolved}"),
                Ok((resolved, meta)) => format!(
                    "{}  ({}): {}",
                    location(resolved),
                    source(resolved.source),
                    found(meta)
                ),
            };
            writeln!(out, "  {:<22}{}", check.root.name(), escaped(&line))?;
        }
        Ok(())
    }

    fn write_integrations(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "\nIntegrations")?;
        for integration in &self.integrations {
            let line = if integration.supported {
                let roots: Vec<&str> = integration.roots.iter().map(|r| r.name()).collect();
                format!("reads {}", roots.join(", "))
            } else {
                format!("not on {PLATFORM}")
            };
            writeln!(out, "  {:<12}{}", integration.id, line)?;
        }
        Ok(())
    }

    fn write_tools(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "\nNative tools")?;
        for (tool, status) in &self.tools {
            let line = match status {
                ToolStatus::NotFound => "not found on PATH".to_owned(),
                ToolStatus::Found {
                    path,
                    version: Ok(version),
                } => format!("{version}  {}", path.display()),
                ToolStatus::Found {
                    path,
                    version: Err(error),
                } => format!("{}: version probe failed: {error}", path.display()),
            };
            writeln!(out, "  {:<12}{}", tool.program(), escaped(&line))?;
        }
        Ok(())
    }

    fn write_processes(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "\nRunning programs")?;
        let snapshot = match &self.processes {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let line = format!("the processes could not be listed: {error}");
                return writeln!(out, "  {}", escaped(&line));
            }
        };
        let count = |class| {
            snapshot
                .processes
                .iter()
                .filter(|p| p.outcome.class() == class)
                .count()
        };
        writeln!(
            out,
            "  {} processes: {} identified, {} unknown, {} exited, {} without a program file",
            snapshot.processes.len(),
            count(ProcessClass::Identified),
            count(ProcessClass::Unknown),
            count(ProcessClass::NotRunning),
            count(ProcessClass::NoExecutable),
        )?;
        match &snapshot.inventory {
            Inventory::Complete => writeln!(out, "  The listing is complete.")?,
            Inventory::Partial { reason } => writeln!(out, "  The listing is partial: {reason}.")?,
        }
        writeln!(
            out,
            "  kipple treats an unknown process as possibly running a tool, never as idle."
        )
    }

    fn write_notes(&self, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "\nNotes")?;
        writeln!(
            out,
            "  kipple can't see the options other programs were started with, so a location \
             set only on a tool's command line is not discovered."
        )?;
        if let Some(hint) = self.permission_hint.filter(|_| self.permission_denied()) {
            writeln!(out, "  {hint}")?;
        }
        Ok(())
    }

    /// Whether any probe was refused permission. Hints depend on it.
    fn permission_denied(&self) -> bool {
        let denied = |meta: &Result<EntryMeta, ProbeError>| {
            meta.as_ref()
                .is_err_and(|e| e.io_kind() == Some(io::ErrorKind::PermissionDenied))
        };
        self.roots
            .iter()
            .any(|check| check.status.as_ref().is_ok_and(|(_, meta)| denied(meta)))
            || self
                .config_file
                .as_ref()
                .is_some_and(|(_, meta)| denied(meta))
    }
}

fn dir_line(dir: &KippleDir) -> String {
    match &dir.path {
        Ok(path) => format!("{}  (under {})", path.display(), dir.base),
        Err(unresolved) => format!("unknown: {unresolved}"),
    }
}

/// Where a root is: for a root resolved through a link, the link and where it leads.
fn location(resolved: &ResolvedRoot) -> String {
    let path = resolved.path.display();
    resolved.link.as_ref().map_or_else(
        || path.to_string(),
        |link| format!("{} -> {path}", link.configured.display()),
    )
}

fn source(source: RootSource) -> String {
    match source {
        RootSource::Env(var) => format!("from {var}"),
        RootSource::Default { base, relative } => format!("default: {base}/{relative}"),
        RootSource::KnownFolder(folder) => format!("Known Folder {folder}"),
        RootSource::Setting { file, key } => format!("{key} in {file}"),
        RootSource::Fixed => "the tool's fixed location".to_owned(),
        RootSource::CommandLine => "the command line".to_owned(),
    }
}

fn found(meta: &Result<EntryMeta, ProbeError>) -> String {
    match meta {
        Ok(meta) => match meta.kind {
            EntryKind::Dir => "directory".to_owned(),
            EntryKind::Symlink => "a link, which kipple does not follow".to_owned(),
            EntryKind::File | EntryKind::Other => "not a directory".to_owned(),
        },
        Err(e) if e.io_kind() == Some(io::ErrorKind::NotFound) => "not present".to_owned(),
        Err(e) => format!("can't be read: {e}"),
    }
}

/// `kipple doctor --json`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DoctorOut {
    /// `kipple.doctor/v1`.
    pub schema: &'static str,
    /// kipple's version.
    pub kipple: &'static str,
    /// The platform it runs on.
    pub platform: &'static str,
    /// kipple's own directories: `config`, `data` and `cache`.
    pub dirs: Vec<DirOut>,
    /// The config file, when the config directory resolved.
    pub config_file: Option<EntryOut>,
    /// Every root of this platform, in a fixed order.
    pub roots: Vec<RootOut>,
    /// Every built-in integration and the roots it reads.
    pub integrations: Vec<IntegrationRootsOut>,
    /// Every native tool kipple knows.
    pub tools: Vec<ToolOut>,
    /// What the process listing found.
    pub processes: ProcessesOut,
    /// A hint for a permission failure that was diagnosed.
    pub permission_hint: Option<&'static str>,
}

/// One of kipple's directories.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DirOut {
    /// `config`, `data` or `cache`.
    pub name: &'static str,
    /// The root it lives under.
    pub base: &'static str,
    /// Where it is, if its base resolved.
    pub path: Option<PathOut>,
    /// Why it has no path.
    pub problem: Option<String>,
}

/// A path and what a no-follow probe found there.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EntryOut {
    /// The path.
    pub path: PathOut,
    /// What is there.
    pub state: EntryState,
    /// The OS error, when it can't be read.
    pub problem: Option<String>,
}

/// What a no-follow probe found at a path.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EntryState {
    /// A directory.
    Directory,
    /// A link, which kipple doesn't follow.
    Link,
    /// Something other than a directory.
    NotDirectory,
    /// Nothing.
    NotPresent,
    /// It can't be read.
    Unreadable,
}

/// A root and what is at its path.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RootOut {
    /// The symbolic root.
    pub root: &'static str,
    /// The configured path, when it is a link that was resolved.
    pub configured: Option<PathOut>,
    /// Where the root is and what is there, or `None` when it has no path.
    pub entry: Option<EntryOut>,
    /// Where the path came from.
    pub source: Option<String>,
    /// Why it has no path.
    pub unresolved: Option<String>,
}

/// A built-in integration and the roots it reads.
#[derive(Debug, Serialize, JsonSchema)]
pub struct IntegrationRootsOut {
    /// Its ID.
    pub id: &'static str,
    /// Its lens.
    pub lens: LensOut,
    /// Whether it runs on this platform.
    pub supported: bool,
    /// The roots it reads.
    pub roots: Vec<&'static str>,
}

/// A native tool.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ToolOut {
    /// Its program name.
    pub name: &'static str,
    /// Where it was found on `PATH`.
    pub path: Option<PathOut>,
    /// Its version, when the probe succeeded.
    pub version: Option<String>,
    /// Why the version probe failed.
    pub problem: Option<String>,
}

/// What the process listing found.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ProcessesOut {
    /// Why the processes couldn't be listed.
    pub problem: Option<String>,
    /// Processes listed.
    pub total: usize,
    /// Processes whose program file was identified.
    pub identified: usize,
    /// Processes kipple treats as possibly running a tool.
    pub unknown: usize,
    /// Processes that exited while being listed.
    pub exited: usize,
    /// Processes with no program file, such as kernel threads.
    pub no_executable: usize,
    /// Why the listing is partial, if it is.
    pub partial: Option<String>,
}

impl Report {
    /// The report as `kipple doctor --json` prints it.
    #[must_use]
    pub fn to_json(&self) -> DoctorOut {
        let dirs = [
            ("config", &self.dirs.config),
            ("data", &self.dirs.data),
            ("cache", &self.dirs.cache),
        ]
        .into_iter()
        .map(|(name, dir)| DirOut {
            name,
            base: dir.base.name(),
            path: dir.path.as_ref().ok().map(|p| PathOut::new(p)),
            problem: dir.path.as_ref().err().map(ToString::to_string),
        })
        .collect();
        DoctorOut {
            schema: DOCTOR,
            kipple: VERSION,
            platform: PLATFORM.name(),
            dirs,
            config_file: self
                .config_file
                .as_ref()
                .map(|(path, meta)| entry(path, meta)),
            roots: self.roots.iter().map(root_out).collect(),
            integrations: self.integrations.iter().map(integration_out).collect(),
            tools: self.tools.iter().map(tool_out).collect(),
            processes: processes_out(&self.processes),
            permission_hint: self.permission_hint.filter(|_| self.permission_denied()),
        }
    }
}

fn entry(path: &std::path::Path, meta: &Result<EntryMeta, ProbeError>) -> EntryOut {
    let (state, problem) = match meta {
        Ok(meta) => match meta.kind {
            EntryKind::Dir => (EntryState::Directory, None),
            EntryKind::Symlink => (EntryState::Link, None),
            EntryKind::File | EntryKind::Other => (EntryState::NotDirectory, None),
        },
        Err(e) if e.io_kind() == Some(io::ErrorKind::NotFound) => (EntryState::NotPresent, None),
        Err(e) => (EntryState::Unreadable, Some(e.to_string())),
    };
    EntryOut {
        path: PathOut::new(path),
        state,
        problem,
    }
}

fn root_out(check: &RootCheck) -> RootOut {
    match &check.status {
        Ok((resolved, meta)) => RootOut {
            root: check.root.name(),
            configured: resolved
                .link
                .as_ref()
                .map(|link| PathOut::new(&link.configured)),
            entry: Some(entry(&resolved.path, meta)),
            source: Some(source(resolved.source)),
            unresolved: None,
        },
        Err(unresolved) => RootOut {
            root: check.root.name(),
            configured: None,
            entry: None,
            source: None,
            unresolved: Some(unresolved.to_string()),
        },
    }
}

fn integration_out(integration: &IntegrationRoots) -> IntegrationRootsOut {
    IntegrationRootsOut {
        id: integration.id.0,
        lens: integration.lens.into(),
        supported: integration.supported,
        roots: integration.roots.iter().map(|r| r.name()).collect(),
    }
}

fn tool_out((tool, status): &(NativeTool, ToolStatus)) -> ToolOut {
    let (path, version, problem) = match status {
        ToolStatus::NotFound => (None, None, None),
        ToolStatus::Found { path, version } => (
            Some(PathOut::new(path)),
            version.as_ref().ok().map(ToString::to_string),
            version.as_ref().err().map(ToString::to_string),
        ),
    };
    ToolOut {
        name: tool.program(),
        path,
        version,
        problem,
    }
}

fn processes_out(processes: &Result<ProcessSnapshot, ProbeError>) -> ProcessesOut {
    let snapshot = match processes {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return ProcessesOut {
                problem: Some(error.to_string()),
                total: 0,
                identified: 0,
                unknown: 0,
                exited: 0,
                no_executable: 0,
                partial: None,
            };
        }
    };
    let count = |class| {
        let listed = snapshot.processes.iter();
        listed.filter(|p| p.outcome.class() == class).count()
    };
    ProcessesOut {
        problem: None,
        total: snapshot.processes.len(),
        identified: count(ProcessClass::Identified),
        unknown: count(ProcessClass::Unknown),
        exited: count(ProcessClass::NotRunning),
        no_executable: count(ProcessClass::NoExecutable),
        partial: match &snapshot.inventory {
            Inventory::Complete => None,
            Inventory::Partial { reason } => Some(reason.clone()),
        },
    }
}
