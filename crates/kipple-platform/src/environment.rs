//! The environment snapshot the composition root captures once (`docs/05-architecture.md`
//! §1). Nothing below the binary reads the process environment itself.

use std::ffi::{OsStr, OsString};

use crate::os;

/// Environment variables as they were when kipple started.
#[derive(Debug, Clone, Default)]
pub struct Environment(Vec<(OsString, OsString)>);

impl Environment {
    /// A snapshot of `vars`, such as `std::env::vars_os()`, or a test's fixture values.
    pub fn from_vars(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Self(vars.into_iter().collect())
    }

    /// The value of `name`, or `None` when it is unset or empty. Names compare the way
    /// the OS compares them (case-insensitively on Windows).
    pub(crate) fn get(&self, name: &str) -> Option<&OsStr> {
        self.0
            .iter()
            .find(|(key, _)| os::env_name_matches(key, name))
            .map(|(_, value)| value.as_os_str())
            .filter(|value| !value.is_empty())
    }
}
