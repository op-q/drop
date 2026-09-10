//! Owning the terminal, and giving it back on every path out.
//!
//! # Why this is a module and not four lines in the interface
//!
//! A terminal in raw mode with the alternate screen showing is a *process-wide*
//! change that outlives the value that made it. If `drop` exits without undoing
//! it, the person is left in a shell that does not echo what they type and does
//! not show their scrollback, and their only way out is `reset` — assuming they
//! can type it blind.
//!
//! There are three ways out of this program and a destructor covers only one:
//!
//! 1. **Returning**, which [`Guard`]'s `Drop` handles.
//! 2. **Panicking**, which unwinds — but the default hook prints the panic to a
//!    raw-mode stderr, which mangles it — so [`install_panic_hook`] restores
//!    first and then prints.
//! 3. **A signal.** [`crate::send::run`] answers SIGINT and SIGTERM by calling
//!    `std::process::exit(130)` *without unwinding*, deliberately, because the
//!    compressed spool file has to be deleted by hand rather than by a
//!    destructor that a signal will never run. No `Drop` fires on that path, so
//!    the handler has to call [`restore`] itself.
//!
//! That third case is why this exists as a free function over a static rather
//! than as a method: the signal handler owns no [`Guard`] and cannot be handed
//! one.

use std::{
    io::{self, IsTerminal, Write},
    sync::atomic::{AtomicBool, Ordering},
};

use crossterm::{
    cursor,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

/// Whether this process currently owes the terminal a restoration.
///
/// Not derived from [`is_raw_mode_enabled`], which answers a different
/// question: this also tracks the alternate screen, and it has to be readable
/// from a signal handler that must not fail.
static OWED: AtomicBool = AtomicBool::new(false);

/// The interface draws here, never to stdout.
///
/// `docs/plans/interactive-terminal-ui-plan-2026-09-10.md` finding 6: the
/// sender puts the transfer code on stdout so it survives a pipe, and
/// `netlab/runner.py` reads it from there. Drawing a screen into that stream
/// would corrupt the one output another program depends on.
fn screen() -> io::Stderr {
    io::stderr()
}

/// Takes the terminal, and gives it back when dropped.
pub struct Guard {
    _private: (),
}

impl Guard {
    /// Enters raw mode and the alternate screen.
    ///
    /// Fails rather than half-entering: if the alternate screen cannot be
    /// entered, raw mode is undone before returning, because a raw terminal on
    /// the *normal* screen is the state that looks like a hung shell.
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;

        if let Err(error) = crossterm::execute!(screen(), EnterAlternateScreen, cursor::Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }

        OWED.store(true, Ordering::SeqCst);

        Ok(Self { _private: () })
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}

/// Gives the terminal back, if this process has taken it.
///
/// Safe to call from anywhere, any number of times, including when the
/// interface never started: the flag makes every call after the first a no-op,
/// so the signal handler does not have to know whether a [`Guard`] exists.
///
/// Every error is swallowed on purpose. This runs on the way out — often on the
/// way out of a panic or a signal — and there is nothing useful to do with a
/// failure except leave the remaining steps untried, which is worse.
pub fn restore() {
    if !OWED.swap(false, Ordering::SeqCst) {
        return;
    }

    let mut out = screen();

    let _ = crossterm::execute!(out, cursor::Show, LeaveAlternateScreen);
    let _ = disable_raw_mode();
    let _ = out.flush();
}

/// Restores the terminal before a panic message is printed.
///
/// Without this the message goes to a raw-mode stderr, where every newline is a
/// bare line feed: the text walks diagonally off the screen and the backtrace
/// is unreadable. Installed once, by the interface, and it chains to whatever
/// hook was already there rather than replacing the behaviour.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}

/// Whether a terminal is present to draw on at all.
///
/// Checked by the caller before [`Guard::enter`]; entering raw mode on a pipe
/// succeeds on some platforms and produces a program nobody can quit.
pub fn is_available() -> bool {
    io::stdin().is_terminal() && screen().is_terminal()
}

/// Whether the terminal is currently owed a restoration. Tests and the signal
/// path use this; nothing else should need it.
pub fn is_owed() -> bool {
    OWED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    /// Restoring without having entered must not touch the terminal, because
    /// the signal handler calls it unconditionally on every `drop send` —
    /// including the overwhelming majority that never open an interface.
    #[test]
    fn restoring_without_entering_is_a_no_op() {
        assert!(!super::is_owed());

        super::restore();
        super::restore();

        assert!(!super::is_owed());

        // `is_raw_mode_enabled` needs a terminal to answer, and under
        // `cargo test` there may be none; absent one the question is simply
        // "no", which is the answer being asserted anyway.
        assert!(
            !crossterm::terminal::is_raw_mode_enabled().unwrap_or(false),
            "restore enabled raw mode from nothing"
        );
    }
}
