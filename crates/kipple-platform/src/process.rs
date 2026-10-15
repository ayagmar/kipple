//! Running processes from direct OS APIs (D-035): what each one runs, who it runs as, and
//! what it can be seen using.

use std::ffi::OsString;
use std::path::PathBuf;

use kipple_core::{
    Capabilities, EnvPath, FileIdentity, MountNamespace, ProbeError, Probed, ProcessOwner,
    ProcessProbe, ProcessRecord, ProcessSnapshot,
};

use crate::os;

/// [`ProcessProbe`] on the running system. Every listed process gets one outcome, and
/// only positive evidence makes a process "gone".
#[derive(Debug, Default, Clone, Copy)]
pub struct OsProcesses;

impl ProcessProbe for OsProcesses {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        let inventory = os::inventory();
        let pids = os::list_pids()?;
        let owners = os::Owners::read();
        let processes = pids
            .into_iter()
            .map(|pid| {
                let context = os::context(pid, &owners);
                ProcessRecord {
                    pid,
                    outcome: os::probe_pid(pid),
                    owner: context.owner,
                    cwd: context.cwd,
                    argv: context.argv,
                    env_paths: context.env_paths,
                    mapped_files: context.mapped_files,
                    mount_namespace: context.mount_namespace,
                    root_dir: context.root_dir,
                    root_path: context.root_path,
                    root_entry: context.root_entry,
                    capabilities: context.capabilities,
                }
            })
            .collect();
        Ok(ProcessSnapshot {
            processes,
            inventory,
            observer: os::observer(),
        })
    }
}

/// Everything about a process besides its executable, each fact read on its own.
pub(crate) struct Context {
    pub(crate) owner: ProcessOwner,
    pub(crate) cwd: Probed<PathBuf>,
    pub(crate) argv: Probed<Vec<OsString>>,
    pub(crate) env_paths: Probed<Vec<EnvPath>>,
    pub(crate) mapped_files: Probed<Vec<PathBuf>>,
    pub(crate) mount_namespace: Probed<MountNamespace>,
    pub(crate) root_dir: Probed<FileIdentity>,
    pub(crate) root_path: Probed<PathBuf>,
    pub(crate) root_entry: Probed<PathBuf>,
    pub(crate) capabilities: Probed<Capabilities>,
}
