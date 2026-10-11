//! The clock and the process table the engine reads, as the composition root gets them.

use std::time::SystemTime;

use kipple_core::{Clock, ProcessProbe};

use crate::environment::Environment;
use crate::process::OsProcesses;

/// The system's clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// The clock: the system's, unless the test-roots override fixes it.
#[must_use]
pub fn clock(env: &Environment) -> Box<dyn Clock> {
    #[cfg(feature = "test-roots")]
    if let Some(now) = crate::test_host::file(env).and_then(crate::test_host::now) {
        return Box::new(crate::test_host::FixedClock(now));
    }
    let _: &Environment = env;
    Box::new(SystemClock)
}

/// The running processes: the system's, unless the test-roots override stands in for a
/// host that runs nothing.
#[must_use]
pub fn processes(env: &Environment) -> Box<dyn ProcessProbe> {
    #[cfg(feature = "test-roots")]
    if crate::test_host::file(env).is_some() {
        return Box::new(crate::test_host::NoProcesses);
    }
    let _: &Environment = env;
    Box::new(OsProcesses)
}
