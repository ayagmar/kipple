//! A question that changes the disk is answered by what is typed after it (spec M3b
//! pinned 1: a person authorizes exactly the plan shown). A line typed before it, during
//! a long scan, is thrown away first.

use std::io::{self, IsTerminal as _};

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

#[cfg(unix)]
mod sys {
    use std::io;
    use std::os::fd::{AsFd, AsRawFd as _};

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
}

#[cfg(windows)]
mod sys {
    use std::io;
    use std::os::windows::io::{AsHandle, AsRawHandle as _};

    use windows_sys::Win32::System::Console::{FlushConsoleInputBuffer, GetConsoleMode};

    /// Discards the input `console` received and nobody read. A terminal that isn't a
    /// console, such as mintty's pipe, which std also counts as a terminal, can't be
    /// flushed this way and is left as it is.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes the console calls as raw calls"
    )]
    pub(super) fn discard(console: &impl AsHandle) -> io::Result<()> {
        let handle = console.as_handle().as_raw_handle();
        let mut mode = 0;
        // SAFETY: the handle is borrowed, so it stays open for both calls, and `mode` is
        // a live value.
        unsafe {
            if GetConsoleMode(handle, &raw mut mode) == 0 {
                return Ok(());
            }
            if FlushConsoleInputBuffer(handle) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
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
