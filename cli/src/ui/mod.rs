//! Which surface an invocation gets: the interface, or the command.
//!
//! `drop` has two audiences and they want opposite things. A person typing
//! `drop send` wants to be shown their files; a harness running
//! `drop send ./fixture --transport relay` wants exactly what it asked for and
//! nothing drawn over it. This module is the one place that decides which of
//! the two is in front of the program, so the answer cannot drift between the
//! `send` and `recv` arms of the binary.
//!
//! The rule is deliberately conservative: **the interface has to be asked for
//! by silence.** Any argument, any flag, any sign that something is reading
//! the output, and the invocation is a command. Being wrong in that direction
//! costs a person one extra word to type. Being wrong in the other direction
//! hangs a CI job on a screen nobody can see.
//!
//! See `docs/plans/interactive-terminal-ui-plan-2026-09-10.md`, phase 0.

pub mod app;
pub mod terminal;

use std::io::IsTerminal;

/// What the program should do with this invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// Draw the interface. Nobody is reading this but a person.
    Interface,
    /// Run as a command, exactly as `drop` always has.
    Command,
}

/// Everything the decision depends on, passed in rather than read here.
///
/// Tests construct this directly for the same reason
/// [`crate::send::AskTheTerminal`] has an unattended constructor: `cargo test`
/// inherits whatever terminal it was started from, so a decision that asked
/// the process would pass or fail depending on how the suite was launched.
#[derive(Clone, Copy, Debug)]
pub struct Invocation {
    /// A path for `send`, a code for `recv`. Naming one is asking for the
    /// command.
    pub has_positional: bool,
    /// Any flag at all. Flags are the program-facing surface, so using one
    /// says which audience this is.
    pub has_flags: bool,
    /// `--status` or `DROP_STATUS`. This one is not configuration — it exists
    /// so a program can parse the output, which means a program is reading.
    pub status_requested: bool,
    pub stdin_is_terminal: bool,
    pub stdout_is_terminal: bool,
    pub stderr_is_terminal: bool,
}

impl Invocation {
    /// Reads the three streams from the process. The caller supplies what it
    /// parsed from the command line.
    pub fn from_process(has_positional: bool, has_flags: bool, status_requested: bool) -> Self {
        Self {
            has_positional,
            has_flags,
            status_requested,
            stdin_is_terminal: std::io::stdin().is_terminal(),
            stdout_is_terminal: std::io::stdout().is_terminal(),
            stderr_is_terminal: std::io::stderr().is_terminal(),
        }
    }

    /// Which surface this invocation gets.
    ///
    /// All three streams are required to be terminals, including stdout, which
    /// the interface never writes to. That is not an oversight: the sender
    /// puts the transfer code on stdout precisely so it survives a pipe, and
    /// `netlab/runner.py` reads it from there. A redirected stdout is somebody
    /// collecting that code, and they should get it rather than a screen.
    pub fn surface(&self) -> Surface {
        let asked_for_something = self.has_positional || self.has_flags || self.status_requested;

        let nobody_but_a_person =
            self.stdin_is_terminal && self.stdout_is_terminal && self.stderr_is_terminal;

        if asked_for_something || !nobody_but_a_person {
            Surface::Command
        } else {
            Surface::Interface
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Invocation, Surface};

    /// A bare invocation with a person in front of it, which every case below
    /// varies one field of.
    fn bare() -> Invocation {
        Invocation {
            has_positional: false,
            has_flags: false,
            status_requested: false,
            stdin_is_terminal: true,
            stdout_is_terminal: true,
            stderr_is_terminal: true,
        }
    }

    #[test]
    fn silence_on_a_terminal_is_the_only_way_in() {
        assert_eq!(bare().surface(), Surface::Interface);
    }

    #[test]
    fn naming_a_path_or_a_code_is_asking_for_the_command() {
        let named = Invocation {
            has_positional: true,
            ..bare()
        };

        assert_eq!(named.surface(), Surface::Command);
    }

    #[test]
    fn any_flag_means_the_program_facing_surface() {
        let flagged = Invocation {
            has_flags: true,
            ..bare()
        };

        assert_eq!(flagged.surface(), Surface::Command);
    }

    /// `DROP_STATUS` is exported once by a harness and inherited by every
    /// `drop` it spawns, so it has to count even when the command line is bare.
    #[test]
    fn a_status_request_means_a_program_is_reading() {
        let watched = Invocation {
            status_requested: true,
            ..bare()
        };

        assert_eq!(watched.surface(), Surface::Command);
    }

    /// The netlab case: spawned under `ip netns exec` with pipes on every
    /// stream. It must reach the command however few arguments it carries.
    #[test]
    fn no_terminal_anywhere_is_a_command() {
        let piped = Invocation {
            stdin_is_terminal: false,
            stdout_is_terminal: false,
            stderr_is_terminal: false,
            ..bare()
        };

        assert_eq!(piped.surface(), Surface::Command);
    }

    /// `drop send | head -1`, or anything else collecting the code. The person
    /// is real and the terminal is real, and they still want the code.
    #[test]
    fn a_redirected_stdout_wants_the_code_not_a_screen() {
        let piped = Invocation {
            stdout_is_terminal: false,
            ..bare()
        };

        assert_eq!(piped.surface(), Surface::Command);
    }

    /// Input from a file or a heredoc means the keystrokes are not a person's.
    #[test]
    fn a_redirected_stdin_cannot_drive_an_interface() {
        let scripted = Invocation {
            stdin_is_terminal: false,
            ..bare()
        };

        assert_eq!(scripted.surface(), Surface::Command);
    }

    /// The interface draws here, so this one is the most obviously fatal.
    #[test]
    fn a_redirected_stderr_has_nowhere_to_draw() {
        let captured = Invocation {
            stderr_is_terminal: false,
            ..bare()
        };

        assert_eq!(captured.surface(), Surface::Command);
    }
}
