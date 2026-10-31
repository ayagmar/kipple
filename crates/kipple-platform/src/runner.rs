//! The platform runner: the one place kipple starts a native command
//! (`docs/08-engineering-standards.md` §2). It runs read-only version probes and git's
//! tracked-file query (D-066).

use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use kipple_core::{CancelToken, ToolError};

use crate::environment::Environment;
use crate::os;

/// How often the runner checks on a child that has closed its output but not exited.
const POLL: Duration = Duration::from_millis(10);

/// The most output a version probe keeps from each stream. The rest is read and
/// discarded, so a chatty tool can neither block on a full pipe nor grow kipple's memory.
const OUTPUT_LIMIT: u64 = 64 * 1024;

/// A command to run: the program, its arguments, the variables it gets beyond the
/// runner's own, how long it may take and how much of its stdout is kept.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Invocation<'command> {
    pub(crate) program: &'command Path,
    pub(crate) args: &'command [&'command OsStr],
    pub(crate) vars: &'command [Var<'command>],
    pub(crate) deadline: Duration,
    /// The most bytes kept from stdout. Stderr keeps [`OUTPUT_LIMIT`].
    pub(crate) limit: u64,
    /// Stops the command when set, as missing its deadline does.
    pub(crate) cancel: Option<&'command CancelToken>,
}

/// One environment variable a command is started with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Var<'value> {
    pub(crate) name: &'value str,
    pub(crate) value: &'value OsStr,
}

/// What a finished command printed.
#[derive(Debug)]
pub(crate) struct Output {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    /// Whether stdout went past the limit, so `stdout` is only its start.
    pub(crate) truncated: bool,
}

/// One stream as read: what was kept, and whether there was more.
#[derive(Debug, Default)]
struct Captured {
    kept: Vec<u8>,
    truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
}

/// Runs `program` with `args`, no stdin and a minimal environment (`LC_ALL=C` plus what
/// the OS needs to start a process), out of reach of the terminal's Ctrl+C, and waits at
/// most `deadline`.
///
/// A child that misses its deadline is killed: kipple stops only processes it started
/// itself, never anything else (`docs/04-safety-model.md` §3.8).
pub(crate) fn run(
    program: &Path,
    args: &[&str],
    env: &Environment,
    deadline: Duration,
) -> Result<Output, ToolError> {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let invocation = Invocation {
        program,
        args: &args,
        vars: &[],
        deadline,
        limit: OUTPUT_LIMIT,
        cancel: None,
    };
    invoke(&invocation, env)
}

/// Runs `invocation` as [`run`] does, with its own variables, output limit and cancel
/// token. A cancelled command is stopped and fails as an interrupted wait.
pub(crate) fn invoke(invocation: &Invocation<'_>, env: &Environment) -> Result<Output, ToolError> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the platform runner is the one place that starts native commands"
    )]
    let mut command = Command::new(invocation.program);
    command
        .args(invocation.args)
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in os::INHERITED_VARS {
        if let Some(value) = env.get(name) {
            command.env(name, value);
        }
    }
    for var in invocation.vars {
        command.env(var.name, var.value);
    }
    os::own_process_group(&mut command);
    let mut child = command.spawn().map_err(|e| ToolError::Start(Arc::new(e)))?;
    let started = Instant::now();
    // `sender` lives until `wait` returns, so `outputs` never disconnects early.
    let (sender, outputs) = mpsc::channel();
    read_in_background(
        Stream::Stdout,
        child.stdout.take(),
        invocation.limit,
        &sender,
    );
    read_in_background(Stream::Stderr, child.stderr.take(), OUTPUT_LIMIT, &sender);
    wait(&mut child, &outputs, started, invocation)
}

type Delivered = (Stream, io::Result<Captured>);

fn read_in_background<R: Read + Send + 'static>(
    stream: Stream,
    pipe: Option<R>,
    limit: u64,
    sender: &mpsc::Sender<Delivered>,
) {
    let sender = sender.clone();
    thread::spawn(move || sender.send((stream, drain(pipe, limit))));
}

/// Waits for the child to exit and both streams to close, or kills it at the deadline or
/// once it is cancelled.
fn wait(
    child: &mut Child,
    outputs: &mpsc::Receiver<Delivered>,
    started: Instant,
    invocation: &Invocation<'_>,
) -> Result<Output, ToolError> {
    let wait_error = |e| ToolError::Wait(Arc::new(e));
    let deadline = invocation.deadline;
    let (mut status, mut stdout, mut stderr): (_, Option<Captured>, Option<Captured>) =
        (None, None, None);
    loop {
        if status.is_none() {
            status = child.try_wait().map_err(wait_error)?;
        }
        if let (Some(status), Some(stdout), Some(stderr)) = (status, &mut stdout, &mut stderr) {
            let stdout = std::mem::take(stdout);
            return Ok(Output {
                status,
                stdout: stdout.kept,
                stderr: std::mem::take(stderr).kept,
                truncated: stdout.truncated,
            });
        }
        let left = deadline.checked_sub(started.elapsed());
        let cancelled = invocation.cancel.is_some_and(CancelToken::is_cancelled);
        let Some(left) = left.filter(|_| !cancelled) else {
            child
                .kill()
                .and_then(|()| child.wait())
                .map_err(wait_error)?;
            return Err(if cancelled {
                wait_error(io::ErrorKind::Interrupted.into())
            } else {
                ToolError::TimedOut { deadline }
            });
        };
        match outputs.recv_timeout(left.min(POLL)) {
            Ok((Stream::Stdout, bytes)) => stdout = Some(bytes.map_err(wait_error)?),
            Ok((Stream::Stderr, bytes)) => stderr = Some(bytes.map_err(wait_error)?),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
    }
}

/// Reads a stream to its end, keeping its first `limit` bytes. One byte more is read to
/// tell output exactly at the limit from output past it.
fn drain(pipe: Option<impl Read>, limit: u64) -> io::Result<Captured> {
    let mut captured = Captured::default();
    if let Some(mut pipe) = pipe {
        let read = pipe
            .by_ref()
            .take(limit.saturating_add(1))
            .read_to_end(&mut captured.kept)?;
        if u64::try_from(read).map_or(true, |read| read > limit) {
            captured.truncated = true;
            captured
                .kept
                .truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        }
        io::copy(&mut pipe, &mut io::sink())?;
    }
    Ok(captured)
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

    /// Not a test: the child the process group test starts. It passes when it leads a
    /// process group of its own.
    #[cfg(unix)]
    #[test]
    #[ignore = "started by a_tool_is_out_of_reach_of_the_terminals_interrupt"]
    #[expect(unsafe_code, reason = "libc's getpgrp is a raw call")]
    fn leads_its_own_process_group() {
        // SAFETY: getpgrp takes nothing and can't fail.
        let group = unsafe { libc::getpgrp() };
        assert_eq!(u32::try_from(group).ok(), Some(std::process::id()));
    }

    /// Catches a terminal Ctrl+C killing git midway, which turns a cancel into an item
    /// skipped for unknown git: the terminal interrupts its foreground process group,
    /// and each tool kipple starts leads a group of its own.
    #[cfg(unix)]
    #[test]
    fn a_tool_is_out_of_reach_of_the_terminals_interrupt() {
        let this_test_binary = std::env::current_exe().unwrap();
        let args = [
            "--exact",
            "runner::tests::leads_its_own_process_group",
            "--ignored",
        ];

        let output = run(
            &this_test_binary,
            &args,
            &Environment::default(),
            Duration::from_secs(60),
        )
        .unwrap();

        assert!(
            output.status.success(),
            "the tool leads its own group: {}",
            String::from_utf8_lossy(&output.stdout)
        );
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
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the hung tool is stopped well before 5 seconds"
        );
    }

    /// Catches a cancelled scan or apply left waiting on a native command: the command is
    /// stopped, and fails rather than answer.
    #[test]
    fn a_cancelled_tool_is_stopped() {
        let this_test_binary = std::env::current_exe().unwrap();
        let args = ["--exact", "runner::tests::hanging_child", "--ignored"].map(OsStr::new);
        let cancel = CancelToken::new();
        cancel.cancel();
        let invocation = Invocation {
            program: &this_test_binary,
            args: &args,
            vars: &[],
            deadline: Duration::from_secs(60),
            limit: OUTPUT_LIMIT,
            cancel: Some(&cancel),
        };
        let started = Instant::now();

        let result = invoke(&invocation, &Environment::default());

        assert!(
            matches!(&result, Err(ToolError::Wait(error)) if error.kind() == io::ErrorKind::Interrupted),
            "a cancelled tool fails as interrupted: {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the cancelled tool is stopped well before its deadline"
        );
    }
}
