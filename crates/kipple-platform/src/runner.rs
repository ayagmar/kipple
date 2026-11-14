//! The platform runner: the one place kipple starts a native command
//! (`docs/08-engineering-standards.md` §2). It runs read-only version probes, git's
//! queries (D-066) and a tool's own reviewed command (`native.rs`).

use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use kipple_core::{Anchor, ApplyError, CancelToken, ErrorCode, Kept, Printed, ToolError};

use crate::environment::Environment;
use crate::executor::{self, sys};
use crate::interrupt;
use crate::os;

#[cfg(test)]
pub(crate) mod faults;

/// How often the runner checks on a child that has closed its output but not exited.
const POLL: Duration = Duration::from_millis(10);

/// The most output a version probe or a tool's command keeps from each stream. The rest
/// is read and discarded, so a chatty tool can neither block on a full pipe nor grow
/// kipple's memory.
pub(crate) const OUTPUT_LIMIT: u64 = 64 * 1024;

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
    /// Where the child starts, whatever the directory's ancestors have become. Without
    /// one it starts where kipple runs.
    pub(crate) dir: Option<&'command CheckedDir>,
}

/// A directory reached by the confined walk (D-061): every component from a verified root
/// opened without following a link, its identity compared. A child started here holds that
/// inode as its working directory, so no swap of a path can rebind it.
#[derive(Debug)]
pub(crate) struct CheckedDir(sys::Dir);

impl CheckedDir {
    /// The directory at `anchor`, which must be what it records.
    pub(crate) fn of(anchor: &Anchor) -> Result<Self, ApplyError> {
        let reached = executor::reach(anchor)?;
        let Some(last) = anchor.relative.components().last() else {
            return Ok(Self(reached.root));
        };
        Self::below(&reached, last)
    }

    /// The directory `last` in the directory `reached` holds a target in, on its volume.
    pub(crate) fn below(
        reached: &executor::Reached,
        last: &kipple_core::Component,
    ) -> Result<Self, ApplyError> {
        let name = sys::name(last.as_os_str()).map_err(|e| ApplyError::io(ErrorCode::Io, e))?;
        let dir = reached.holder().open_dir(&name).map_err(|error| {
            let code = if sys::is_link_or_file(&error) {
                ErrorCode::TargetChanged
            } else {
                ErrorCode::TargetMissing
            };
            ApplyError::io(code, error)
        })?;
        let seen = dir.seen().map_err(|e| ApplyError::io(ErrorCode::Io, e))?;
        if !seen.on(reached.volume) {
            return Err(ApplyError::new(ErrorCode::MountBoundary));
        }
        Ok(Self(dir))
    }

    /// What a child is told to start in, where this OS has a way to name the open handle.
    pub(crate) fn start_path(&self) -> Option<PathBuf> {
        os::working_dir(&self.0)
    }

    /// The open directory.
    pub(crate) const fn dir(&self) -> &sys::Dir {
        &self.0
    }
}

/// One environment variable a command is started with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Var<'value> {
    pub(crate) name: &'value str,
    pub(crate) value: &'value OsStr,
}

/// How a command that started ended, as far as kipple saw.
#[derive(Debug)]
pub(crate) enum Finish {
    /// It exited and closed its output by itself.
    Exited(ExitStatus),
    /// It missed its deadline, and kipple killed it.
    MissedDeadline(ExitStatus),
    /// Its cancel token was set, and kipple killed it.
    Cancelled(ExitStatus),
    /// Its wait, or a read of its output, failed after it started, so kipple stopped it.
    Lost(Lost),
}

/// A command kipple lost sight of after it started.
#[derive(Debug)]
pub(crate) struct Lost {
    /// What failed.
    pub(crate) error: io::Error,
    /// How the child ended once kipple killed and reaped it, or why that failed too, when
    /// it may still run.
    pub(crate) stopped: io::Result<ExitStatus>,
}

/// A command waited for to its end, however that came.
#[derive(Debug)]
pub(crate) struct Ran {
    pub(crate) finish: Finish,
    /// What it printed before it ended: nothing of a stream it hadn't closed when it was
    /// killed, or whose read failed.
    pub(crate) stdout: Printed,
    pub(crate) stderr: Printed,
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
        dir: None,
    };
    invoke(&invocation, env)
}

/// Runs `invocation` as [`run`] does, with its own variables, output limit and cancel
/// token. A cancelled command is stopped and fails as an interrupted wait.
pub(crate) fn invoke(invocation: &Invocation<'_>, env: &Environment) -> Result<Output, ToolError> {
    let ran = invoke_to_end(invocation, env)?;
    match ran.finish {
        Finish::Exited(status) => Ok(Output {
            status,
            truncated: ran.stdout.kept == Kept::Start,
            stdout: ran.stdout.bytes,
            stderr: ran.stderr.bytes,
        }),
        Finish::MissedDeadline(_) => Err(ToolError::TimedOut {
            deadline: invocation.deadline,
        }),
        Finish::Cancelled(_) => Err(ToolError::Wait(Arc::new(io::ErrorKind::Interrupted.into()))),
        Finish::Lost(lost) => Err(ToolError::Wait(Arc::new(lost.error))),
    }
}

/// Runs `invocation` and waits for it as [`invoke`] does, giving how a command stopped at
/// its deadline or cancel ended too. Fails only when it couldn't start: once it has, any
/// failure to wait for it or read it stops it and is [`Finish::Lost`].
pub(crate) fn invoke_to_end(
    invocation: &Invocation<'_>,
    env: &Environment,
) -> Result<Ran, ToolError> {
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
    if let Some(dir) = invocation.dir {
        // No way to name the handle is a failed start: nothing runs from a path instead.
        let path = dir
            .start_path()
            .ok_or_else(|| ToolError::Start(Arc::new(io::ErrorKind::Unsupported.into())))?;
        command.current_dir(path);
    }
    if interrupt::armed() {
        os::own_process_group(&mut command);
    }
    #[cfg(test)]
    faults::spawning();
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
    Ok(wait(&mut child, &outputs, started, invocation))
}

type Delivered = (Stream, io::Result<Printed>);

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
/// once it is cancelled. Only the child is killed: what it started itself runs on. A wait
/// or a read that fails stops the child too, and the run is [`Finish::Lost`].
fn wait(
    child: &mut Child,
    outputs: &mpsc::Receiver<Delivered>,
    started: Instant,
    invocation: &Invocation<'_>,
) -> Ran {
    let mut streams = Streams::default();
    let mut status = None;
    loop {
        if status.is_none() {
            match try_wait(child) {
                Ok(now) => status = now,
                Err(error) => return streams.ended(lost(child, error)),
            }
        }
        if let Some(status) = status
            && streams.closed()
        {
            return streams.ended(Finish::Exited(status));
        }
        let left = invocation.deadline.checked_sub(started.elapsed());
        let cancelled = invocation.cancel.is_some_and(CancelToken::is_cancelled);
        let Some(left) = left.filter(|_| !cancelled) else {
            let finish = match child.kill().and_then(|()| child.wait()) {
                Ok(killed) if cancelled => Finish::Cancelled(killed),
                Ok(killed) => Finish::MissedDeadline(killed),
                Err(error) => lost(child, error),
            };
            return streams.ended(finish);
        };
        match outputs.recv_timeout(left.min(POLL)) {
            Ok((stream, Ok(printed))) => streams.take(stream, printed),
            Ok((_, Err(error))) => return streams.ended(lost(child, error)),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
    }
}

/// The streams a child has closed, as read.
#[derive(Debug, Default)]
struct Streams {
    stdout: Option<Printed>,
    stderr: Option<Printed>,
}

impl Streams {
    fn take(&mut self, stream: Stream, printed: Printed) {
        match stream {
            Stream::Stdout => self.stdout = Some(printed),
            Stream::Stderr => self.stderr = Some(printed),
        }
    }

    const fn closed(&self) -> bool {
        self.stdout.is_some() && self.stderr.is_some()
    }

    /// The run, ended as `finish` says, with what each stream printed: a stream not read
    /// to its end shows as cut.
    fn ended(self, finish: Finish) -> Ran {
        let read = |printed: Option<Printed>| {
            printed.unwrap_or(Printed {
                bytes: Vec::new(),
                kept: Kept::Start,
            })
        };
        Ran {
            finish,
            stdout: read(self.stdout),
            stderr: read(self.stderr),
        }
    }
}

/// Polls the child, failing as a test set it to.
fn try_wait(child: &mut Child) -> io::Result<Option<ExitStatus>> {
    #[cfg(test)]
    faults::wait()?;
    child.try_wait()
}

/// The run of a child kipple lost sight of through `error`: it is killed and reaped, and
/// how that went is kept. Killing a child already reaped kills nothing.
fn lost(child: &mut Child, error: io::Error) -> Finish {
    Finish::Lost(Lost {
        error,
        stopped: child.kill().and_then(|()| child.wait()),
    })
}

/// Reads a stream to its end, keeping its first `limit` bytes. One byte more is read to
/// tell output exactly at the limit from output past it.
fn drain(pipe: Option<impl Read>, limit: u64) -> io::Result<Printed> {
    let mut captured = Printed::default();
    if let Some(mut pipe) = pipe {
        let read = pipe
            .by_ref()
            .take(limit.saturating_add(1))
            .read_to_end(&mut captured.bytes)?;
        if u64::try_from(read).map_or(true, |read| read > limit) {
            captured.kept = Kept::Start;
            captured
                .bytes
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

    /// Not a test: the child the process group tests start. It passes when it leads a
    /// process group of its own.
    #[cfg(unix)]
    #[test]
    #[ignore = "started by the process group tests"]
    #[expect(unsafe_code, reason = "libc's getpgrp is a raw call")]
    fn leads_its_own_process_group() {
        // SAFETY: getpgrp takes nothing and can't fail.
        let group = unsafe { libc::getpgrp() };
        assert_eq!(u32::try_from(group).ok(), Some(std::process::id()));
    }

    #[cfg(unix)]
    fn child_leads_its_own_group() -> bool {
        let this_test_binary = std::env::current_exe().unwrap();
        let args = [
            "--exact",
            "runner::tests::leads_its_own_process_group",
            "--ignored",
        ];
        run(
            &this_test_binary,
            &args,
            &Environment::default(),
            Duration::from_secs(60),
        )
        .unwrap()
        .status
        .success()
    }

    /// Catches a terminal Ctrl+C killing git midway, which turns a cancel into an item
    /// skipped for unknown git: the terminal interrupts its foreground process group,
    /// and each tool kipple starts leads a group of its own.
    #[cfg(unix)]
    #[test]
    fn a_tool_is_out_of_reach_of_the_terminals_interrupt() {
        crate::cancel_on_interrupt(&CancelToken::new()).unwrap();

        assert!(child_leads_its_own_group(), "the tool leads its own group");
    }

    /// Catches a read-only command leaving a hung probe running after Ctrl+C ends kipple:
    /// with no interrupt handler, each tool stays in kipple's group, where the terminal's
    /// interrupt reaches it too.
    #[cfg(unix)]
    #[test]
    fn without_a_handler_a_tool_shares_the_terminals_interrupt() {
        assert!(
            !child_leads_its_own_group(),
            "the tool is in kipple's group"
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
            dir: None,
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

    /// Catches a wait that fails after the start read as a start that failed, with the
    /// child left running unwatched: the run is lost, its child killed and reaped, and
    /// only a caller that needs an answer turns that into an error.
    #[test]
    fn a_wait_that_fails_after_the_start_stops_the_child() {
        let this_test_binary = std::env::current_exe().unwrap();
        let args = ["--exact", "runner::tests::hanging_child", "--ignored"].map(OsStr::new);
        let invocation = Invocation {
            program: &this_test_binary,
            args: &args,
            vars: &[],
            deadline: Duration::from_secs(60),
            limit: OUTPUT_LIMIT,
            cancel: None,
            dir: None,
        };
        let _failing = faults::fail_waits();
        let started = Instant::now();

        let ran = invoke_to_end(&invocation, &Environment::default()).unwrap();

        assert!(
            matches!(&ran.finish, Finish::Lost(Lost { stopped: Ok(_), .. })),
            "lost, and stopped: {ran:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the child was stopped, not waited for"
        );
        let answer = invoke(&invocation, &Environment::default());
        assert!(
            matches!(answer, Err(ToolError::Wait(_))),
            "a probe gets no answer: {answer:?}"
        );
    }
}
