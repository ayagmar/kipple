//! Running a tool's own command (`docs/04-safety-model.md` §6). It starts through the
//! runner, from a checked directory, never through a shell. Until a native operation is
//! implemented here, every start is refused, so nothing runs.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use kipple_core::{ApplyError, ErrorCode, NativeArg, NativeExit, NativeOp, NativeRunner};

use crate::environment::Environment;
use crate::runner::Var;

/// A file that is always empty and that no one can replace: the global config, the
/// hooks directory and, on Linux and macOS, the XDG config directory all point here, so
/// git finds nothing in them.
#[cfg(unix)]
const NULL_DEVICE: &str = "/dev/null";
#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";

/// The variables git gets beside the runner's, for a query and a removal alike: no system
/// or global config, no XDG config directory, no optional locks, no pager and no lazy
/// fetch. Only some releases honour that last one and no version number says which, so it
/// backs the repository config gate (`git/worktree.rs`) and never replaces it.
pub(crate) fn git_isolation() -> Vec<Var<'static>> {
    let var = |name, value| Var {
        name,
        value: OsStr::new(value),
    };
    Vec::from([
        var("GIT_CONFIG_NOSYSTEM", "1"),
        var("GIT_CONFIG_GLOBAL", NULL_DEVICE),
        var("GIT_OPTIONAL_LOCKS", "0"),
        var("GIT_PAGER", "cat"),
        var("GIT_NO_LAZY_FETCH", "1"),
        // Both of these are second lines. What holds is that a promisor is refused, so no
        // fetch starts: no version number says which gits honour the line above, and a
        // fetch that has started can reach what this list never sees, as `fetch.bundleURI`
        // does. Git can't be told to list nothing, so it lists one name no alias can hold:
        // empty or `:` would list the empty name, which `::x` as a remote's URL picks.
        var("GIT_ALLOW_PROTOCOL", "\n"),
        // Windows has no directory that can't hold a file: its XDG directory stays unset,
        // as `HOME` is, so git looks for none.
        #[cfg(unix)]
        var("XDG_CONFIG_HOME", NULL_DEVICE),
    ])
}

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
