//! Ctrl+C during a mutating command (spec M3b pinned 17): the first interrupt cancels the
//! command's token, so no new item starts and every finished one stays recorded; a second
//! one ends the process at once, which the next command reconciles like a crash.

use std::sync::OnceLock;

use kipple_core::CancelToken;

/// The token the first interrupt cancels.
static TOKEN: OnceLock<CancelToken> = OnceLock::new();

/// Makes the first interrupt (Ctrl+C: `SIGINT` on Linux and macOS, a console control event
/// on Windows) cancel `token` instead of ending the process. Only the first call in a
/// process takes effect.
///
/// # Errors
/// When the OS refuses the handler: the interrupt then ends the process as it would
/// without one.
pub fn cancel_on_interrupt(token: &CancelToken) -> std::io::Result<()> {
    if TOKEN.set(token.clone()).is_err() {
        return Ok(());
    }
    sys::install()
}

/// Cancels the token; called from the handler, so it only loads and stores atomics.
fn interrupted() {
    if let Some(token) = TOKEN.get() {
        token.cancel();
    }
}

#[cfg(unix)]
mod sys {
    use std::io;

    extern "C" fn handle(_: libc::c_int) {
        super::interrupted();
    }

    /// `SA_RESETHAND` puts the default action back once the handler ran, so a second
    /// interrupt ends the process.
    #[expect(unsafe_code, reason = "libc's sigaction is a raw call")]
    pub(super) fn install() -> io::Result<()> {
        // SAFETY: an all-zero `sigaction` is valid (an empty mask and no flags), the
        // handler only touches atomics, which is async-signal-safe, and both pointers are
        // to live values.
        let installed = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handle as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_flags = libc::SA_RESETHAND | libc::SA_RESTART;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(libc::SIGINT, &raw const action, std::ptr::null_mut())
        };
        if installed == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };
    use windows_sys::core::BOOL;

    /// Whether the handler ran once.
    static SEEN: AtomicBool = AtomicBool::new(false);

    /// The first Ctrl+C or Ctrl+Break cancels; the next one goes to the default handler,
    /// which ends the process.
    extern "system" fn handle(event: u32) -> BOOL {
        if (event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT)
            && !SEEN.swap(true, Ordering::AcqRel)
        {
            super::interrupted();
            TRUE
        } else {
            FALSE
        }
    }

    #[expect(
        unsafe_code,
        reason = "windows-sys exposes SetConsoleCtrlHandler as a raw call"
    )]
    pub(super) fn install() -> io::Result<()> {
        // SAFETY: `handle` lives for the whole process and only touches atomics.
        if unsafe { SetConsoleCtrlHandler(Some(handle), TRUE) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Catches Ctrl+C killing a mutating command midway instead of cancelling it: the
    /// signal sets the token and the process lives on.
    #[test]
    #[expect(unsafe_code, reason = "libc's raise is a raw call")]
    fn an_interrupt_cancels_the_token_and_the_process_goes_on() {
        let token = CancelToken::new();
        cancel_on_interrupt(&token).unwrap();

        // SAFETY: raising a signal whose handler is installed above.
        let raised = unsafe { libc::raise(libc::SIGINT) };

        assert_eq!(raised, 0);
        assert!(token.is_cancelled(), "the interrupt cancelled the token");
    }
}
