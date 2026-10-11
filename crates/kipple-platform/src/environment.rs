//! The environment snapshot the composition root captures once (`docs/05-architecture.md`
//! §1). Nothing below the binary reads the process environment itself.

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use crate::os;

/// Environment variables as they were when kipple started, and where the system's own
/// fixed paths (`/etc`, `/var`) are.
#[derive(Debug, Clone)]
pub struct Environment {
    vars: Vec<(OsString, OsString)>,
    system_root: PathBuf,
}

impl Default for Environment {
    fn default() -> Self {
        Self::from_vars([])
    }
}

impl Environment {
    /// A snapshot of `vars`, such as `std::env::vars_os()`, or a test's fixture values,
    /// with the system's fixed paths where they are.
    pub fn from_vars(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Self {
            vars: vars.into_iter().collect(),
            system_root: PathBuf::from("/"),
        }
    }

    /// The same, with the system's fixed paths (such as `/etc/pacman.conf`) read below
    /// `root` instead, so a test never reads the machine's own.
    #[must_use]
    pub fn with_system_root(self, root: PathBuf) -> Self {
        Self {
            system_root: root,
            ..self
        }
    }

    /// Where the system's fixed, absolute `path` is.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(dead_code, reason = "only Linux roots live at fixed system paths")
    )]
    pub(crate) fn system_path(&self, path: &Path) -> PathBuf {
        let relative = path
            .components()
            .filter(|c| matches!(c, Component::Normal(_) | Component::ParentDir));
        relative.fold(self.system_root.clone(), |joined, part| joined.join(part))
    }

    /// The value of `name`, or `None` when it is unset or empty. Names compare the way
    /// the OS compares them (case-insensitively on Windows).
    pub(crate) fn get(&self, name: &str) -> Option<&OsStr> {
        self.vars
            .iter()
            .find(|(key, _)| os::env_name_matches(key, name))
            .map(|(_, value)| value.as_os_str())
            .filter(|value| !value.is_empty())
    }
}
