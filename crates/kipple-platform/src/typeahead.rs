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
/// - Windows console: line input and Ctrl+C processing must be on. It reads nothing: it
///   looks at the console's input every 50 ms, or as soon as input arrives, until a
///   pressed Enter (or Ctrl+Z) is there, so the read that follows doesn't block, and
///   checks `cancel` between turns. Focus, mouse, menu and size events at the front of
///   the input are dropped. Keys show when Enter is pressed, since the console echoes
///   only during a read. A cancelled wait throws away what was typed (D-070).
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
    use std::os::windows::io::{AsHandle, AsRawHandle as _};
    use std::thread;

    use kipple_core::{CancelToken, Waited};
    use windows_sys::Win32::Foundation::{HANDLE, WAIT_FAILED};
    use windows_sys::Win32::System::Console::{
        ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, FlushConsoleInputBuffer, GetConsoleMode,
        GetNumberOfConsoleInputEvents, INPUT_RECORD, KEY_EVENT, PeekConsoleInputW,
        ReadConsoleInputW,
    };
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    /// The characters whose key ends a line read of the console as std asks for it:
    /// Enter, and Ctrl+Z, which std wakes on too.
    const LINE_ENDS: [u16; 2] = [0x0D, 0x1A];

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

    /// Waits until `console` holds a whole line for the read that follows, or `cancel`,
    /// without reading anything: std reads the console again after a read Ctrl+C ended,
    /// so no cancel can end a read it owns. The read that follows a ready wait finds the
    /// line's end already there and doesn't block. Cancelled, what was typed is thrown
    /// away, so it neither answers a later question nor reaches the shell.
    pub(super) fn wait(
        console: &impl AsHandle,
        _: &mut dyn BufRead,
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
        let handle = console.as_handle().as_raw_handle();
        loop {
            if cancel.is_cancelled() {
                flush(handle);
                return Ok(Waited::Cancelled);
            }
            match held(handle)? {
                Held::Line => return Ok(Waited::Ready),
                Held::Nothing => until_input(handle)?,
                // Input is there, so the console handle stays signalled: wait a turn.
                Held::Part => thread::park_timeout(super::TURN),
            }
        }
    }

    /// What a console's input holds.
    enum Held {
        Nothing,
        /// Keys, but no line's end yet.
        Part,
        Line,
    }

    /// What `console`'s input holds, once the records a line read passes over (focus,
    /// mouse, menu and size events) are dropped from its front.
    fn held(console: HANDLE) -> io::Result<Held> {
        let records = peek(console)?;
        let passed = records
            .iter()
            .take_while(|record| u32::from(record.EventType) != KEY_EVENT)
            .count();
        if passed > 0 {
            drop_front(console, passed)?;
        }
        let keys = records.get(passed..).unwrap_or_default();
        Ok(if keys.iter().any(ends_line) {
            Held::Line
        } else if keys.is_empty() {
            Held::Nothing
        } else {
            Held::Part
        })
    }

    /// Whether `record` is a key pressed that ends a line read.
    #[expect(unsafe_code, reason = "an input record's event is a union")]
    fn ends_line(record: &INPUT_RECORD) -> bool {
        if u32::from(record.EventType) != KEY_EVENT {
            return false;
        }
        // SAFETY: a record of a key event holds a key event, and its character is read
        // as the UTF-16 unit the wide calls give.
        let (down, unit) = unsafe {
            let key = record.Event.KeyEvent;
            (key.bKeyDown != 0, key.uChar.UnicodeChar)
        };
        down && LINE_ENDS.contains(&unit)
    }

    /// The records waiting in `console`'s input, without taking them.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes the console calls as raw calls"
    )]
    fn peek(console: HANDLE) -> io::Result<Vec<INPUT_RECORD>> {
        let mut count = 0;
        // SAFETY: the handle is open for the call and `count` is a live value.
        if unsafe { GetNumberOfConsoleInputEvents(console, &raw mut count) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut records = vec![INPUT_RECORD::default(); usize::try_from(count).unwrap_or(0)];
        if records.is_empty() {
            return Ok(records);
        }
        let mut read = 0;
        // SAFETY: the handle is open and `records` holds `count` writable records.
        if unsafe { PeekConsoleInputW(console, records.as_mut_ptr(), count, &raw mut read) } == 0 {
            return Err(io::Error::last_os_error());
        }
        records.truncate(usize::try_from(read).unwrap_or(0));
        Ok(records)
    }

    /// Takes the first `count` records of `console`'s input. They are there, so it doesn't
    /// block, and nothing else reads them meanwhile; what arrives goes after them.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes ReadConsoleInputW as a raw call"
    )]
    fn drop_front(console: HANDLE, count: usize) -> io::Result<()> {
        let mut records = vec![INPUT_RECORD::default(); count];
        let length = u32::try_from(count).map_err(|_| io::ErrorKind::InvalidInput)?;
        let mut read = 0;
        // SAFETY: the handle is open and `records` holds `length` writable records.
        if unsafe { ReadConsoleInputW(console, records.as_mut_ptr(), length, &raw mut read) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Waits at most a turn for input to arrive at `console`, whose handle is signalled
    /// while its input holds anything.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes WaitForSingleObject as a raw call"
    )]
    fn until_input(console: HANDLE) -> io::Result<()> {
        let turn = u32::try_from(super::TURN.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: the handle is open for the call.
        if unsafe { WaitForSingleObject(console, turn) } == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Throws away what `console`'s input holds. A flush that fails leaves it, which only
    /// the shell may then read: the wait is cancelled either way.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes FlushConsoleInputBuffer as a raw call"
    )]
    fn flush(console: HANDLE) {
        // SAFETY: the handle is open for the call.
        unsafe { FlushConsoleInputBuffer(console) };
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
