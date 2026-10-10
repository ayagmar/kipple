//! `kipple doctor` (`docs/03-functional-spec.md` §2): why a scan might be empty or partial.
//! It only reads: roots, tool versions and the process table.

use std::io::{self, Write};
use std::path::PathBuf;

use kipple_core::{
    EntryKind, EntryMeta, FsProbe as _, Inventory, NativeTool, ProbeError, ProcessClass,
    ProcessProbe as _, ProcessSnapshot, ResolvedRoot, RootSource, SymbolicRoot, ToolStatus,
    UnresolvedRoot,
};
use kipple_platform::{
    CONFIG_FILE, Environment, KippleDir, KippleDirs, NoFollowFs, OsProcesses, kipple_dirs,
    permission_hint, probe_tools, resolve_roots,
};

/// Everything `doctor` found.
#[derive(Debug)]
pub struct Report {
    /// kipple's own directories.
    pub dirs: KippleDirs,
    /// The config file and what is there, when the config directory resolved.
    pub config_file: Option<(PathBuf, Result<EntryMeta, ProbeError>)>,
    /// Every root on this platform.
    pub roots: Vec<RootCheck>,
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

/// Probes everything `doctor` reports.
#[must_use]
pub fn gather(env: &Environment) -> Report {
    let roots = resolve_roots(env);
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
        tools: probe_tools(env),
        processes: OsProcesses.snapshot(),
        permission_hint: permission_hint(),
    }
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
                    resolved.path.display(),
                    source(resolved.source),
                    found(meta)
                ),
            };
            writeln!(out, "  {:<22}{}", check.root.name(), escaped(&line))?;
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

fn source(source: RootSource) -> String {
    match source {
        RootSource::Env(var) => format!("from {var}"),
        RootSource::Default { base, relative } => format!("default: {base}/{relative}"),
        RootSource::KnownFolder(folder) => format!("Known Folder {folder}"),
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

/// `text` for the terminal: paths and OS messages may hold control characters.
fn escaped(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            let shown: Vec<char> = if c.is_control() {
                c.escape_default().collect()
            } else {
                vec![c]
            };
            shown
        })
        .collect()
}
