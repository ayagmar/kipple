//! Running-executable identity from direct OS APIs (D-035).

use kipple_core::{ProbeError, ProcessProbe, ProcessRecord, ProcessSnapshot};

use crate::os;

/// [`ProcessProbe`] on the running system. Every listed process gets one outcome, and
/// only positive evidence makes a process "gone".
#[derive(Debug, Default, Clone, Copy)]
pub struct OsProcesses;

impl ProcessProbe for OsProcesses {
    fn snapshot(&self) -> Result<ProcessSnapshot, ProbeError> {
        let inventory = os::inventory();
        let processes = os::list_pids()?
            .into_iter()
            .map(|pid| ProcessRecord {
                pid,
                outcome: os::probe_pid(pid),
            })
            .collect();
        Ok(ProcessSnapshot {
            processes,
            inventory,
        })
    }
}
