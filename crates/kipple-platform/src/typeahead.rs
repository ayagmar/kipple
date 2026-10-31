//! A question that changes the disk is answered by what is typed after it (spec M3b
//! pinned 1: a person authorizes exactly the plan shown). A line typed before it, during
//! a long scan, is thrown away first. Waiting for the answer stops at once on Ctrl+C
//! (spec M3c pinned 15).

use std::io::{self, BufRead, IsTerminal as _};
use std::time::Duration;

use kipple_core::{CancelToken, Waited};

/// Throws away what was typed at stdin's terminal and not read yet.
///
/// The next answer read was then typed after this call. When stdin isn't a terminal,
/// nothing was typed there, and it does nothing.
///
/// # Errors
/// When the terminal refuses.
pub fn discard_typeahead() -> io::Result<()> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return Ok(());
    }
    sys::discard(&stdin)
}

/// Waits until a line or the end of input is there to read from stdin, or `cancel`.
///
/// Called after a question and before reading its answer through `input`, so Ctrl+C at
/// the question stops the command at once instead of after Enter (spec M3c pinned 15).
///
/// When stdin isn't a terminal, or on Windows is one that isn't a console (mintty's
/// pipe), it doesn't wait: the read that follows blocks, as the typeahead exception
/// documents.
///
/// - Linux and macOS: the terminal must hand over whole lines and turn Ctrl+C into a
///   signal (`ICANON` and `ISIG`); one left raw by a crashed full-screen program is
///   refused. It then waits 50 ms at a time, `poll` on Linux and `select` on macOS,
///   whose `poll` doesn't take terminals, checking `cancel` between turns. The terminal
///   echoes what is typed.
/// - Windows console: line input and Ctrl+C processing must be on. The line is read into
///   `input`'s buffer, so the console echoes each key as it is typed, while a watcher
///   thread stops that read with `CancelSynchronousIo` within 50 ms of `cancel`
///   (D-070).
///
/// # Errors
/// When the terminal refuses the wait or the read, or isn't reading whole lines: the
/// question then counts as answered no.
pub fn wait_for_line(input: &mut dyn BufRead, cancel: &CancelToken) -> io::Result<Waited> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return Ok(Waited::Ready);
    }
    sys::wait(&stdin, input, cancel)
}

/// How long a wait goes before it looks at its token again.
const TURN: Duration = Duration::from_millis(50);

#[cfg(unix)]
mod sys {
    use std::io::{self, BufRead};
    use std::mem::MaybeUninit;
    use std::os::fd::{AsFd, AsRawFd as _, RawFd};

    use kipple_core::{CancelToken, Waited};

    /// Discards the input `terminal` received and nobody read.
    #[expect(unsafe_code, reason = "libc's tcflush is a raw call")]
    pub(super) fn discard(terminal: &impl AsFd) -> io::Result<()> {
        // SAFETY: the descriptor is borrowed, so it stays open for the call.
        if unsafe { libc::tcflush(terminal.as_fd().as_raw_fd(), libc::TCIFLUSH) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Waits until `terminal` has a whole line or the end of input to read, or `cancel`.
    /// A terminal reading whole lines hands over one per read, so `input` holds none yet.
    pub(super) fn wait(
        terminal: &impl AsFd,
        _: &mut dyn BufRead,
        cancel: &CancelToken,
    ) -> io::Result<Waited> {
        let fd = terminal.as_fd().as_raw_fd();
        let lines = libc::ICANON | libc::ISIG;
        if settings(fd)?.c_lflag & lines != lines {
            return Err(io::Error::other(
                "the terminal isn't reading whole lines or taking Ctrl+C",
            ));
        }
        loop {
            if cancel.is_cancelled() {
                return Ok(Waited::Cancelled);
            }
            if readable(fd)? {
                return Ok(Waited::Ready);
            }
        }
    }

    /// The terminal settings of `fd`.
    #[expect(unsafe_code, reason = "libc's tcgetattr is a raw call")]
    fn settings(fd: RawFd) -> io::Result<libc::termios> {
        let mut settings = MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `fd` is borrowed for the call and `settings` is writable.
        if unsafe { libc::tcgetattr(fd, settings.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `tcgetattr` succeeded, so it filled `settings`.
        Ok(unsafe { settings.assume_init() })
    }

    /// Whether `fd` has a line or the end of input to read within a turn. A signal ends
    /// the turn early. A hang-up is the end of input; an error on the terminal is an error.
    #[cfg(target_os = "linux")]
    #[expect(unsafe_code, reason = "libc's poll is a raw call")]
    fn readable(fd: RawFd) -> io::Result<bool> {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let turn = libc::c_int::try_from(super::TURN.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: one live `pollfd`, and the descriptor is borrowed for the call.
        if unsafe { libc::poll(&raw mut poll, 1, turn) } < 0 {
            return interrupted();
        }
        if poll.revents & libc::POLLHUP != 0 {
            return Ok(true);
        }
        if poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(io::Error::other("the terminal failed"));
        }
        Ok(poll.revents & libc::POLLIN != 0)
    }

    /// Whether `fd` has a line or the end of input to read within a turn. A signal ends
    /// the turn early. A hang-up reads as the end of input.
    #[cfg(target_os = "macos")]
    #[expect(unsafe_code, reason = "libc's select and its set macros are raw calls")]
    fn readable(fd: RawFd) -> io::Result<bool> {
        if usize::try_from(fd).map_or(true, |fd| fd >= libc::FD_SETSIZE) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut set = MaybeUninit::<libc::fd_set>::uninit();
        let micros = libc::suseconds_t::try_from(super::TURN.as_micros()).unwrap_or(0);
        let mut turn = libc::timeval {
            tv_sec: 0,
            tv_usec: micros,
        };
        // SAFETY: `FD_ZERO` fills the set, `fd` is below `FD_SETSIZE`, the set and the
        // time are live values, and the descriptor is borrowed for the call.
        unsafe {
            libc::FD_ZERO(set.as_mut_ptr());
            libc::FD_SET(fd, set.as_mut_ptr());
            let ready = libc::select(
                fd + 1,
                set.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut turn,
            );
            if ready < 0 {
                return interrupted();
            }
            Ok(libc::FD_ISSET(fd, set.as_ptr()))
        }
    }

    /// A wait that failed: a signal, which ends the turn, or an error.
    fn interrupted() -> io::Result<bool> {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::io::{self, BufRead};
    use std::os::windows::io::{AsHandle, AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use std::ptr;
    use std::sync::mpsc;
    use std::thread;

    use kipple_core::{CancelToken, Waited};
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, FALSE};
    use windows_sys::Win32::System::Console::{
        ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, FlushConsoleInputBuffer, GetConsoleMode,
    };
    use windows_sys::Win32::System::IO::CancelSynchronousIo;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread};

    /// Discards the input `console` received and nobody read. A terminal that isn't a
    /// console, such as mintty's pipe, which std also counts as a terminal, can't be
    /// flushed this way and is left as it is.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes the console calls as raw calls"
    )]
    pub(super) fn discard(console: &impl AsHandle) -> io::Result<()> {
        let Some(_) = mode(console) else {
            return Ok(());
        };
        // SAFETY: the handle is borrowed, so it stays open for the call.
        if unsafe { FlushConsoleInputBuffer(console.as_handle().as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The console mode of `console`, `None` when it isn't a console.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes GetConsoleMode as a raw call"
    )]
    fn mode(console: &impl AsHandle) -> Option<u32> {
        let mut mode = 0;
        // SAFETY: the handle is borrowed, so it stays open for the call, and `mode` is a
        // live value.
        let known = unsafe { GetConsoleMode(console.as_handle().as_raw_handle(), &raw mut mode) };
        (known != 0).then_some(mode)
    }

    /// Reads a line into `input`'s buffer from `console`, which echoes each key as it is
    /// typed, until it is there or `cancel` stops the read.
    pub(super) fn wait(
        console: &impl AsHandle,
        input: &mut dyn BufRead,
        cancel: &CancelToken,
    ) -> io::Result<Waited> {
        let Some(mode) = mode(console) else {
            return Ok(Waited::Ready);
        };
        let lines = ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT;
        if mode & lines != lines {
            return Err(io::Error::other(
                "the console isn't reading whole lines or taking Ctrl+C",
            ));
        }
        if cancel.is_cancelled() {
            return Ok(Waited::Cancelled);
        }
        let watcher = Watcher::start(cancel.clone())?;
        let filled = input.fill_buf().map(|_| ());
        watcher.stop();
        if cancel.is_cancelled() {
            return Ok(Waited::Cancelled);
        }
        filled.map(|()| Waited::Ready)
    }

    /// A thread that, once the token is cancelled, stops the console read of the thread
    /// that started it, every turn until it is stopped: std reads again after a read
    /// Ctrl+C ended, and a stop that comes between two reads stops neither.
    #[derive(Debug)]
    struct Watcher {
        stop: mpsc::Sender<()>,
        thread: thread::JoinHandle<()>,
    }

    impl Watcher {
        fn start(cancel: CancelToken) -> io::Result<Self> {
            let reader = this_thread()?;
            let (stop, stopped) = mpsc::channel::<()>();
            let thread = thread::Builder::new()
                .name("kipple-answer-watcher".to_owned())
                .spawn(move || watch(&reader, &stopped, &cancel))?;
            Ok(Self { stop, thread })
        }

        fn stop(self) {
            drop(self.stop);
            self.thread.join().ok();
        }
    }

    /// Stops `reader`'s read every turn once `cancel` is cancelled, until `stopped`.
    fn watch(reader: &OwnedHandle, stopped: &mpsc::Receiver<()>, cancel: &CancelToken) {
        while stopped.recv_timeout(super::TURN) == Err(mpsc::RecvTimeoutError::Timeout) {
            if cancel.is_cancelled() {
                stop_read(reader);
            }
        }
    }

    /// A handle to the calling thread that another thread can use.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes DuplicateHandle as a raw call"
    )]
    fn this_thread() -> io::Result<OwnedHandle> {
        let mut handle = ptr::null_mut();
        // SAFETY: the pseudo-handles are always valid in the calling process, and
        // `handle` is a live value.
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                GetCurrentThread(),
                GetCurrentProcess(),
                &raw mut handle,
                0,
                FALSE,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if duplicated == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: DuplicateHandle made a new handle nothing else owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    /// Stops the read `thread` is blocked in. When it is between reads there is nothing
    /// to stop, and the next turn tries again.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes CancelSynchronousIo as a raw call"
    )]
    fn stop_read(thread: &OwnedHandle) {
        // SAFETY: the handle is open for the call.
        unsafe { CancelSynchronousIo(thread.as_raw_handle()) };
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs::File;
    use std::io::Write as _;
    use std::os::fd::{AsFd, AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::ptr;

    use super::*;

    /// A new pseudo-terminal: the side a person types into, and the side kipple reads.
    #[expect(unsafe_code, reason = "libc's openpty is a raw call")]
    fn terminal() -> (File, OwnedFd) {
        let (mut keyboard, mut reader) = (-1, -1);
        // SAFETY: both out-pointers are to live values; no name, settings or size.
        let opened = unsafe {
            libc::openpty(
                &raw mut keyboard,
                &raw mut reader,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "{}", io::Error::last_os_error());
        // SAFETY: openpty opened both and nothing else owns them.
        unsafe { (File::from_raw_fd(keyboard), OwnedFd::from_raw_fd(reader)) }
    }

    /// Whether `terminal` has input to read within `millis`.
    #[expect(unsafe_code, reason = "libc's poll is a raw call")]
    fn waiting(terminal: &impl AsFd, millis: i32) -> bool {
        let mut poll = libc::pollfd {
            fd: terminal.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live `pollfd`, and the descriptor is borrowed for the call.
        let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
        ready > 0 && poll.revents & libc::POLLIN != 0
    }

    /// Catches a `y` typed during the scan answering the question that comes after it:
    /// the line waiting on the terminal is gone once it is discarded.
    #[test]
    fn a_line_typed_before_the_question_is_thrown_away() {
        let (mut keyboard, reader) = terminal();
        keyboard.write_all(b"y\n").unwrap();
        assert!(waiting(&reader, 5_000), "the typed line waits to be read");

        sys::discard(&reader).unwrap();

        assert!(!waiting(&reader, 0), "nothing typed is left to read");
    }
}
