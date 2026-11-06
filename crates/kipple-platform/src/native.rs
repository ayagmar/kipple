//! Running a tool's own command (`docs/04-safety-model.md` §6). It starts through the
//! runner, from a checked directory, never through a shell. Until a native operation is
//! implemented here, every start is refused, so nothing runs.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use kipple_core::{ApplyError, ErrorCode, NativeArg, NativeExit, NativeOp, NativeRunner};

use crate::environment::Environment;

/// A file that is always empty and that no one can replace: the global config, the
/// hooks directory and, on Linux and macOS, the XDG config directory all point here, so
/// git finds nothing in them.
#[cfg(unix)]
pub(crate) const NULL_DEVICE: &str = "/dev/null";
#[cfg(windows)]
pub(crate) const NULL_DEVICE: &str = "NUL";

/// `args` as a process gets them, with `data` for kipple's own paths. `None` when one
/// needs `data` and there is none, so nothing runs without its boundary.
pub(crate) fn rendered(args: &[NativeArg], data: Option<&Path>) -> Option<Vec<OsString>> {
    args.iter()
        .map(|arg| match arg {
            NativeArg::Fixed(text) => Some(OsString::from(text)),
            NativeArg::NullDevice(key) => {
                let mut text = OsString::from(key);
                text.push(NULL_DEVICE);
                Some(text)
            }
            NativeArg::Data(key, below) => {
                let mut text = OsString::from(key);
                text.push(data?.join(below));
                Some(text)
            }
        })
        .collect()
}

/// The platform's [`NativeRunner`].
#[derive(Debug)]
pub struct NativeCommands {
    #[expect(dead_code, reason = "the command's environment, once one can start")]
    env: Environment,
    #[expect(dead_code, reason = "npm's configuration boundary, once it can start")]
    data: Option<PathBuf>,
}

impl NativeCommands {
    /// The runner for the machine `env` describes, with kipple's data directory `data`
    /// when it has one.
    #[must_use]
    pub fn new(env: &Environment, data: Option<PathBuf>) -> Self {
        Self {
            env: env.clone(),
            data,
        }
    }
}

impl NativeRunner for NativeCommands {
    fn run(&self, _: &NativeOp) -> Result<NativeExit, ApplyError> {
        Err(ApplyError::new(ErrorCode::Unsupported))
    }
}
