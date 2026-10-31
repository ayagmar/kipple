//! The wait for an answer (spec M3c pinned 15) at a real terminal put in place of this
//! process's stdin: a pseudo-terminal on Linux and macOS, a console of its own on
//! Windows. Each test runs in a process of its own (nextest), so no other test reads
//! that stdin. Each test names what it catches.
#![expect(
    clippy::unwrap_used,
    reason = "a fixture that fails to build fails the test"
)]

use std::io::{self, BufRead as _};
use std::thread;
use std::time::{Duration, Instant};

use kipple_core::{CancelToken, Waited};
use kipple_platform::wait_for_line;

/// How soon after a cancel the wait has ended.
const PROMPTLY: Duration = Duration::from_millis(200);

/// How long a person takes before they press a key or Ctrl+C.
const LATER: Duration = Duration::from_millis(100);

/// Waits `delay`.
fn pause(delay: Duration) {
    let until = Instant::now() + delay;
    while let Some(left) = until
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
    {
        thread::park_timeout(left);
    }
}

/// Waits for an answer at stdin, as a question does.
fn wait(cancel: &CancelToken) -> io::Result<Waited> {
    wait_for_line(&mut io::stdin().lock(), cancel)
}

/// The next line read from stdin, as the question reads its answer.
fn read_line() -> io::Result<String> {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).map(|_| line)
}

/// Cancels `cancel` from another thread after `delay`; the thread returns when it did.
fn cancel_after(cancel: &CancelToken, delay: Duration) -> thread::JoinHandle<Instant> {
    let cancel = cancel.clone();
    thread::spawn(move || {
        pause(delay);
        let at = Instant::now();
        cancel.cancel();
        at
    })
}

/// Fails unless the wait ended in `waited` and within [`PROMPTLY`] of `cancelled`.
fn cancelled_promptly(waited: io::Result<Waited>, ended: Instant, cancelled: Instant) {
    assert_eq!(waited.unwrap(), Waited::Cancelled);
    let late = ended.saturating_duration_since(cancelled);
    assert!(late < PROMPTLY, "the wait ended {late:?} after the cancel");
}

#[cfg(unix)]
mod unix {
    use std::fs::File;
    use std::io::{self, Write as _};
    use std::os::fd::FromRawFd as _;
    use std::os::unix::thread::JoinHandleExt as _;
    use std::ptr;
    use std::thread;
    use std::time::{Duration, Instant};

    use kipple_core::{CancelToken, Waited};
    use kipple_platform::cancel_on_interrupt;

    use super::{LATER, cancel_after, cancelled_promptly, pause, read_line, wait};

    /// A new pseudo-terminal in place of this process's stdin: returns the side a person
    /// types into.
    #[expect(unsafe_code, reason = "libc's openpty, dup2 and close are raw calls")]
    fn terminal() -> File {
        let (mut keyboard, mut reader) = (-1, -1);
        // SAFETY: both out-pointers are to live values; no name, settings or size. The
        // reader is put on stdin, then its own descriptor closed once.
        unsafe {
            let opened = libc::openpty(
                &raw mut keyboard,
                &raw mut reader,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            assert_eq!(opened, 0, "{}", io::Error::last_os_error());
            assert_eq!(libc::dup2(reader, libc::STDIN_FILENO), libc::STDIN_FILENO);
            libc::close(reader);
            File::from_raw_fd(keyboard)
        }
    }

    /// Turns `flag` of the local modes of stdin's terminal off.
    #[expect(unsafe_code, reason = "libc's tcgetattr and tcsetattr are raw calls")]
    fn turn_off(flag: libc::tcflag_t) {
        let mut settings = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `settings` is writable and filled by `tcgetattr` before it is read.
        unsafe {
            assert_eq!(
                libc::tcgetattr(libc::STDIN_FILENO, settings.as_mut_ptr()),
                0
            );
            let mut settings = settings.assume_init();
            settings.c_lflag &= !flag;
            let set = libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const settings);
            assert_eq!(set, 0);
        }
    }

    /// Catches a question that waits for Enter after Ctrl+C, or takes half a line as an
    /// answer: with `y` typed and no Enter, a cancel ends the wait within 200 ms.
    #[test]
    fn a_cancel_ends_the_wait_within_200ms_while_a_line_is_half_typed() {
        let mut keyboard = terminal();
        keyboard.write_all(b"y").unwrap();
        let cancel = CancelToken::new();
        let canceller = cancel_after(&cancel, LATER);

        let waited = wait(&cancel);

        cancelled_promptly(waited, Instant::now(), canceller.join().unwrap());
    }

    /// Catches an answer typed before the wait missed, or one typed during it never seen:
    /// either way the wait is ready and the line reads whole.
    #[test]
    fn a_line_typed_before_or_during_the_wait_is_ready_and_read_whole() {
        let mut keyboard = terminal();
        let cancel = CancelToken::new();
        keyboard.write_all(b"n\n").unwrap();

        assert_eq!(wait(&cancel).unwrap(), Waited::Ready);
        assert_eq!(read_line().unwrap(), "n\n");

        let typist = thread::spawn(move || {
            pause(LATER);
            keyboard.write_all(b"yes\n").unwrap();
            keyboard
        });
        assert_eq!(wait(&cancel).unwrap(), Waited::Ready);
        assert_eq!(read_line().unwrap(), "yes\n");
        drop(typist.join().unwrap());
    }

    /// Catches a terminal left raw by a crashed full-screen program taking one key as the
    /// answer, or ignoring Ctrl+C: the wait refuses, which answers no.
    #[test]
    fn a_terminal_not_reading_lines_or_taking_ctrl_c_answers_no() {
        let _keyboard = terminal();
        let cancel = CancelToken::new();

        turn_off(libc::ISIG);
        assert!(wait(&cancel).is_err(), "Ctrl+C isn't a signal");
        turn_off(libc::ICANON);
        assert!(wait(&cancel).is_err(), "keys aren't gathered into lines");
    }

    /// Catches the signal Ctrl+C sends taken as a failed wait, which answers no instead
    /// of stopping the command: it interrupts the wait, which then sees its token
    /// cancelled.
    #[test]
    #[expect(unsafe_code, reason = "libc's pthread_kill is a raw call")]
    fn ctrl_c_at_the_question_cancels_the_wait_within_200ms() {
        let _keyboard = terminal();
        let cancel = CancelToken::new();
        cancel_on_interrupt(&cancel).unwrap();
        let question = thread::spawn(move || (wait(&cancel), Instant::now()));
        pause(LATER);

        let sent = Instant::now();
        // SAFETY: the thread is alive until it is joined below.
        let killed = unsafe { libc::pthread_kill(question.as_pthread_t(), libc::SIGINT) };

        assert_eq!(killed, 0);
        let (waited, ended) = question.join().unwrap();
        cancelled_promptly(waited, ended, sent);
    }

    /// Catches a wait that never ends once the terminal is gone: a hang-up is ready, and
    /// the read finds no answer.
    #[test]
    fn a_terminal_that_hangs_up_is_ready_and_gives_no_answer() {
        drop(terminal());
        let cancel = CancelToken::new();
        let _guard = cancel_after(&cancel, Duration::from_secs(5));

        assert_eq!(wait(&cancel).unwrap(), Waited::Ready);
        let read = read_line();
        assert!(
            !matches!(&read, Ok(line) if !line.is_empty()),
            "no answer: {read:?}"
        );
    }
}

#[cfg(windows)]
mod windows {
    use std::fs::OpenOptions;
    use std::io;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
    use std::thread;
    use std::time::{Duration, Instant};

    use kipple_core::{CancelToken, Waited};
    use kipple_platform::cancel_on_interrupt;
    use windows_sys::Win32::Foundation::{FALSE, FILETIME, TRUE};
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    use windows_sys::Win32::System::Console::{
        AllocConsole, COORD, CTRL_C_EVENT, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
        ENABLE_PROCESSED_INPUT, FOCUS_EVENT, FOCUS_EVENT_RECORD, FreeConsole,
        GenerateConsoleCtrlEvent, GetConsoleMode, INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT,
        KEY_EVENT_RECORD, KEY_EVENT_RECORD_0, MOUSE_EVENT, MOUSE_EVENT_RECORD, MOUSE_MOVED,
        ReadConsoleOutputCharacterW, STD_INPUT_HANDLE, SetConsoleCtrlHandler, SetConsoleMode,
        SetStdHandle, WriteConsoleInputW,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};

    use super::{LATER, cancel_after, cancelled_promptly, pause, read_line, wait};

    /// A console of this process's own, its input in place of stdin.
    #[derive(Debug)]
    struct Console {
        input: OwnedHandle,
        output: OwnedHandle,
    }

    /// The console device `name`, opened for reading and writing.
    fn device(name: &str) -> OwnedHandle {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(name)
            .unwrap();
        OwnedHandle::from(file)
    }

    #[expect(
        unsafe_code,
        reason = "windows-sys exposes the console calls as raw calls"
    )]
    fn console() -> Console {
        // SAFETY: plain calls; the handle given to SetStdHandle stays open as long as the
        // returned console, which each test keeps until it ends.
        unsafe {
            FreeConsole();
            assert_ne!(AllocConsole(), 0, "{}", io::Error::last_os_error());
            let console = Console {
                input: device("CONIN$"),
                output: device("CONOUT$"),
            };
            assert_ne!(
                SetStdHandle(STD_INPUT_HANDLE, console.input.as_raw_handle()),
                0
            );
            console
        }
    }

    impl Console {
        /// Puts `records` in the console's input, as a person's keys and mouse do.
        #[expect(
            unsafe_code,
            reason = "windows-sys exposes WriteConsoleInputW as a raw call"
        )]
        fn send(&self, records: &[INPUT_RECORD]) {
            let mut written = 0;
            let count = u32::try_from(records.len()).unwrap();
            // SAFETY: the handle is open and `records` holds `count` records.
            let ok = unsafe {
                WriteConsoleInputW(
                    self.input.as_raw_handle(),
                    records.as_ptr(),
                    count,
                    &raw mut written,
                )
            };
            assert_ne!(ok, 0, "{}", io::Error::last_os_error());
            assert_eq!(written, count);
        }

        /// Types `text`, each character pressed and let go; `\r` is Enter.
        fn type_text(&self, text: &str) {
            let records: Vec<INPUT_RECORD> = text
                .encode_utf16()
                .flat_map(|unit| [key(unit, TRUE), key(unit, FALSE)])
                .collect();
            self.send(&records);
        }

        /// What the console shows, from its top left.
        #[expect(
            unsafe_code,
            reason = "windows-sys exposes ReadConsoleOutputCharacterW as a raw call"
        )]
        fn screen(&self) -> String {
            let mut shown = vec![0_u16; 4096];
            let mut read = 0;
            let length = u32::try_from(shown.len()).unwrap();
            // SAFETY: the handle is open and `shown` is writable for `length` units.
            let ok = unsafe {
                ReadConsoleOutputCharacterW(
                    self.output.as_raw_handle(),
                    shown.as_mut_ptr(),
                    length,
                    COORD { X: 0, Y: 0 },
                    &raw mut read,
                )
            };
            assert_ne!(ok, 0, "{}", io::Error::last_os_error());
            shown.truncate(usize::try_from(read).unwrap());
            String::from_utf16_lossy(&shown)
        }

        /// Turns `flags` of the console's input mode off.
        #[expect(
            unsafe_code,
            reason = "windows-sys exposes the console calls as raw calls"
        )]
        fn turn_off(&self, flags: u32) {
            let mut mode = 0;
            // SAFETY: the handle is open and `mode` is a live value.
            unsafe {
                assert_ne!(GetConsoleMode(self.input.as_raw_handle(), &raw mut mode), 0);
                assert_ne!(SetConsoleMode(self.input.as_raw_handle(), mode & !flags), 0);
            }
        }
    }

    /// The key that types `unit`, pressed or let go.
    fn key(unit: u16, down: i32) -> INPUT_RECORD {
        let enter = unit == u16::from(b'\r');
        INPUT_RECORD {
            EventType: u16::try_from(KEY_EVENT).unwrap(),
            Event: INPUT_RECORD_0 {
                KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: down,
                    wRepeatCount: 1,
                    // VK_RETURN, or none: the character says what was typed.
                    wVirtualKeyCode: if enter { 0x0D } else { 0 },
                    wVirtualScanCode: 0,
                    uChar: KEY_EVENT_RECORD_0 { UnicodeChar: unit },
                    dwControlKeyState: 0,
                },
            },
        }
    }

    /// The window gaining focus, and the mouse moving over it.
    fn focus_and_mouse() -> [INPUT_RECORD; 3] {
        let moved = |x| INPUT_RECORD {
            EventType: u16::try_from(MOUSE_EVENT).unwrap(),
            Event: INPUT_RECORD_0 {
                MouseEvent: MOUSE_EVENT_RECORD {
                    dwMousePosition: COORD { X: x, Y: 1 },
                    dwButtonState: 0,
                    dwControlKeyState: 0,
                    dwEventFlags: MOUSE_MOVED,
                },
            },
        };
        let focus = INPUT_RECORD {
            EventType: u16::try_from(FOCUS_EVENT).unwrap(),
            Event: INPUT_RECORD_0 {
                FocusEvent: FOCUS_EVENT_RECORD { bSetFocus: TRUE },
            },
        };
        [focus, moved(1), moved(2)]
    }

    /// The processor time this thread has used.
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes GetThreadTimes as a raw call"
    )]
    fn busy() -> Duration {
        let mut times = [FILETIME::default(); 4];
        let [created, exited, kernel, user] = &mut times;
        // SAFETY: the pseudo-handle names this thread and every time is writable.
        let ok = unsafe { GetThreadTimes(GetCurrentThread(), created, exited, kernel, user) };
        assert_ne!(ok, 0);
        let ticks =
            |time: &FILETIME| u64::from(time.dwHighDateTime) << 32 | u64::from(time.dwLowDateTime);
        Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
    }

    /// Catches a wait that spins on the focus and mouse events a console gets while a
    /// person decides, or that a cancel doesn't stop within 200 ms.
    #[test]
    fn focus_and_mouse_events_neither_end_the_wait_nor_spin_and_a_cancel_ends_it() {
        let console = console();
        console.send(&focus_and_mouse());
        let cancel = CancelToken::new();
        let canceller = cancel_after(&cancel, LATER);
        let before = busy();

        let waited = wait(&cancel);

        let ended = Instant::now();
        let used = busy().saturating_sub(before);
        cancelled_promptly(waited, ended, canceller.join().unwrap());
        assert!(used < LATER / 2, "the wait used {used:?} of the processor");
    }

    /// Catches an answer that shows only once Enter is pressed (D-070): the key echoes
    /// while the wait goes on, and the line is then ready and read whole.
    #[test]
    fn a_key_echoes_as_it_is_typed_and_the_line_is_ready_and_read_whole() {
        let console = console();
        let cancel = CancelToken::new();
        let typist = thread::spawn(move || {
            pause(LATER);
            console.type_text("y");
            let started = Instant::now();
            let mut echoed = false;
            while !echoed && started.elapsed() < Duration::from_secs(5) {
                pause(Duration::from_millis(10));
                echoed = console.screen().contains('y');
            }
            console.type_text("\r");
            (echoed, console)
        });

        let waited = wait(&cancel);

        let (echoed, _console) = typist.join().unwrap();
        assert_eq!(waited.unwrap(), Waited::Ready);
        assert!(echoed, "the key showed before Enter");
        assert_eq!(read_line().unwrap().trim_end(), "y");
    }

    /// Catches Ctrl+C at the question waiting for Enter, as std reads again after the
    /// read Ctrl+C ended: the console's Ctrl+C cancels the wait within 200 ms.
    #[test]
    #[expect(
        unsafe_code,
        reason = "windows-sys exposes the console calls as raw calls"
    )]
    fn ctrl_c_at_the_question_cancels_the_wait_within_200ms() {
        let _console = console();
        let cancel = CancelToken::new();
        // SAFETY: a plain call: Ctrl+C reaches this process even if its parent turned it
        // off for it.
        assert_ne!(unsafe { SetConsoleCtrlHandler(None, FALSE) }, 0);
        cancel_on_interrupt(&cancel).unwrap();
        let question = thread::spawn(move || (wait(&cancel), Instant::now()));
        pause(LATER);

        let sent = Instant::now();
        // SAFETY: a plain call; this process is the only one on its console.
        let ok = unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) };

        assert_ne!(ok, 0, "{}", io::Error::last_os_error());
        let (waited, ended) = question.join().unwrap();
        cancelled_promptly(waited, ended, sent);
    }

    /// Catches a console left raw by a crashed full-screen program taking one key as the
    /// answer, or ignoring Ctrl+C: the wait refuses, which answers no.
    #[test]
    fn a_console_not_reading_lines_or_taking_ctrl_c_answers_no() {
        let console = console();
        let cancel = CancelToken::new();

        console.turn_off(ENABLE_PROCESSED_INPUT);
        assert!(wait(&cancel).is_err(), "Ctrl+C isn't processed");
        console.turn_off(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT);
        assert!(wait(&cancel).is_err(), "keys aren't gathered into lines");
    }
}
