//! The platform runner: the one place kipple starts a native command
//! (`docs/08-engineering-standards.md` §2). So far it only runs read-only version probes.

use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use kipple_core::ToolError;

use crate::environment::Environment;
use crate::os;

/// How often the runner checks on a child that has closed its output but not exited.
const POLL: Duration = Duration::from_millis(10);

/// The most output kept from each stream. The rest is read and discarded, so a chatty
/// tool can neither block on a full pipe nor grow kipple's memory.
const OUTPUT_LIMIT: u64 = 64 * 1024;

/// What a finished command printed.
#[derive(Debug)]
pub(crate) struct Output {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
}

/// Runs `program` with `args`, no stdin and a minimal environment (`LC_ALL=C` plus what
/// the OS needs to start a process), and waits at most `deadline`.
///
/// A child that misses its deadline is killed: kipple stops only processes it started
/// itself, never anything else (`docs/04-safety-model.md` §3.8).
pub(crate) fn run(
    program: &Path,
    args: &[&str],
    env: &Environment,
    deadline: Duration,
) -> Result<Output, ToolError> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the platform runner is the one place that starts native commands"
    )]
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("LC_ALL", "C")
        .envs(os::process_env(env))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| ToolError::Start(Arc::new(e)))?;
    let started = Instant::now();
    // `sender` lives until `wait` returns, so `outputs` never disconnects early.
    let (sender, outputs) = mpsc::channel();
    read_in_background(Stream::Stdout, child.stdout.take(), &sender);
    read_in_background(Stream::Stderr, child.stderr.take(), &sender);
    wait(&mut child, &outputs, started, deadline)
}

type Delivered = (Stream, io::Result<Vec<u8>>);

fn read_in_background<R: Read + Send + 'static>(
    stream: Stream,
    pipe: Option<R>,
    sender: &mpsc::Sender<Delivered>,
) {
    let sender = sender.clone();
    thread::spawn(move || sender.send((stream, drain(pipe))));
}

/// Waits for the child to exit and both streams to close, or kills it at the deadline.
fn wait(
    child: &mut Child,
    outputs: &mpsc::Receiver<Delivered>,
    started: Instant,
    deadline: Duration,
) -> Result<Output, ToolError> {
    let wait_error = |e| ToolError::Wait(Arc::new(e));
    let (mut status, mut stdout, mut stderr) = (None, None, None);
    loop {
        if status.is_none() {
            status = child.try_wait().map_err(wait_error)?;
        }
        if let (Some(status), Some(stdout), Some(stderr)) = (status, &mut stdout, &mut stderr) {
            return Ok(Output {
                status,
                stdout: std::mem::take(stdout),
                stderr: std::mem::take(stderr),
            });
        }
        let Some(left) = deadline.checked_sub(started.elapsed()) else {
            child
                .kill()
                .and_then(|()| child.wait())
                .map_err(wait_error)?;
            return Err(ToolError::TimedOut { deadline });
        };
        match outputs.recv_timeout(left.min(POLL)) {
            Ok((Stream::Stdout, bytes)) => stdout = Some(bytes.map_err(wait_error)?),
            Ok((Stream::Stderr, bytes)) => stderr = Some(bytes.map_err(wait_error)?),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
    }
}

/// Reads a stream to its end, keeping the first [`OUTPUT_LIMIT`] bytes.
fn drain(pipe: Option<impl Read>) -> io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    if let Some(mut pipe) = pipe {
        pipe.by_ref().take(OUTPUT_LIMIT).read_to_end(&mut kept)?;
        io::copy(&mut pipe, &mut io::sink())?;
    }
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not a test: the child the deadline test starts. It waits until it is killed.
    #[test]
    #[ignore = "started by a_tool_that_hangs_is_stopped_at_its_deadline"]
    fn hanging_child() {
        loop {
            thread::park();
        }
    }

    #[test]
    fn a_tool_that_hangs_is_stopped_at_its_deadline() {
        let this_test_binary = std::env::current_exe().unwrap();
        let args = ["--exact", "runner::tests::hanging_child", "--ignored"];
        let deadline = Duration::from_millis(300);
        let started = Instant::now();

        let result = run(&this_test_binary, &args, &Environment::default(), deadline);

        assert!(
            matches!(result, Err(ToolError::TimedOut { .. })),
            "{result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
