//! Lending the daemon a terminal to ask the passphrase on
//! (ADR-024 §7, Ф14).
//!
//! # The dead end this opens up
//!
//! §7 moves the passphrase prompt out of the client, because an agent
//! that can run commands as the user can replace the client binary
//! and read anything typed into it. The daemon asks instead.
//!
//! Except a daemon started the way a daemon should be started — by
//! systemd or launchd — has no controlling terminal. `/dev/tty`
//! resolves to nothing. It knows how to ask and has nobody to ask.
//! Until this module, `vault.request_unlock` answered "no prompt
//! surface" and the only way through was putting the passphrase in an
//! environment variable: fine for a server, useless for a person at a
//! laptop. Interactive unlock did not work in the configuration we
//! recommend.
//!
//! # What happens instead
//!
//! The client has a terminal, because a human just typed a command
//! into it. It resolves that terminal to a concrete path —
//! `/dev/pts/3`, not the per-process `/dev/tty` — and names it in the
//! request. The daemon opens it and asks there. The passphrase is
//! read by the daemon from the user's real screen and never passes
//! through the client's memory.
//!
//! # Why a path and not the descriptor itself
//!
//! Passing the descriptor over the socket (`SCM_RIGHTS`) is the more
//! obvious mechanism, and it was the plan. It needs
//! `OwnedFd::from_raw_fd` to adopt what arrives, which is `unsafe`,
//! and this workspace sets `unsafe_code = "forbid"` — not `deny`, so
//! no local exception is possible. The established way around that
//! here is a crate that encapsulates it; none of the fd-passing
//! crates return an owned descriptor either, so each would only move
//! the same `unsafe` somewhere less visible.
//!
//! Naming the terminal achieves the same thing with an ordinary
//! `File::open`. The trust properties are identical, because in both
//! designs the *client* decides which terminal is used — see below.
//!
//! The one real difference: a daemon in a different mount namespace
//! from the client (a container) may not have that path. `SCM_RIGHTS`
//! would still work there. That is a genuine limitation of this
//! approach and the reason to revisit it if namespaces ever come up.
//!
//! # Does letting the client choose the terminal defeat §7?
//!
//! It is the obvious objection, and worked through, no.
//!
//! An agent that names a terminal it controls gains nothing, because
//! nobody types into it: the passphrase comes from a human looking at
//! their own screen, and a pty the agent made is one the agent is
//! alone with. Guessing is no better — `vault.unlock` already takes a
//! passphrase outright, so that oracle always existed. And an agent
//! that wants to trick a human into typing a passphrase somewhere it
//! can read never needed any of this; it can print its own prompt.
//!
//! What the daemon actually rests on is provenance: who started it,
//! and whether the caller is an ancestor that could read its memory
//! ([`crate::provenance`], and the ancestor check in
//! [`crate::socket`]). Neither is affected by which terminal is
//! named. The path decides where the question is printed, not whether
//! the answer can be trusted.
//!
//! # What is refused
//!
//! The daemon opens a caller-supplied path read-write, so two things
//! are checked before it is used:
//!
//! - **It must be under `/dev`.** Naming an arbitrary file would have
//!   the daemon open it for writing, and the prompt text would land
//!   in it.
//! - **It must be a terminal**, checked after opening. A pipe would
//!   mean a script is answering the prompt, and the whole arrangement
//!   is built on a human having been present.

#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Directory a lent terminal must live under.
pub const TERMINAL_DIR: &str = "/dev/";

/// Why a named terminal was refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TerminalError {
    /// The path is not under [`TERMINAL_DIR`].
    #[error(
        "refusing to use `{path}` as a terminal: only paths under {TERMINAL_DIR} are accepted, \
         and opening an arbitrary file would write the prompt into it"
    )]
    NotADevicePath {
        /// The path as supplied.
        path: String,
    },

    /// The path could not be opened.
    #[error("could not open `{path}` as a terminal: {reason}")]
    Unopenable {
        /// The path as supplied.
        path: String,
        /// Underlying I/O failure.
        reason: String,
    },

    /// It opened, but is not a terminal.
    #[error(
        "`{path}` is not a terminal. A passphrase prompt has to be answered by a person at a \
         keyboard, so a pipe or a file is refused here"
    )]
    NotATerminal {
        /// The path as supplied.
        path: String,
    },
}

/// The terminal this process is attached to, as a concrete path.
///
/// `/dev/tty` is deliberately **not** returned: it resolves per
/// process, so handing it to the daemon would name the daemon's own
/// (absent) terminal rather than this one.
///
/// Tries the three standard descriptors in turn, because any of them
/// may be redirected while the others are still the terminal.
pub fn caller_terminal() -> Option<PathBuf> {
    use std::io::{stderr, stdin, stdout};

    nix::unistd::ttyname(stdin())
        .or_else(|_| nix::unistd::ttyname(stdout()))
        .or_else(|_| nix::unistd::ttyname(stderr()))
        .ok()
}

/// Check a caller-supplied path before anything opens it.
///
/// Separated from the opening so the rule can be tested without a
/// terminal, on any platform, and so it is readable as a rule rather
/// than as a step in a function.
pub fn validate_terminal_path(path: &Path) -> Result<(), TerminalError> {
    if !path.starts_with(TERMINAL_DIR) {
        return Err(TerminalError::NotADevicePath {
            path: path.display().to_string(),
        });
    }
    Ok(())
}

/// Open a terminal the client named, refusing anything that is not
/// one.
pub fn open_client_terminal(path: &Path) -> Result<File, TerminalError> {
    validate_terminal_path(path)?;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| TerminalError::Unopenable {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;

    // After opening, not before: a path can name anything, and the
    // only reliable answer comes from the descriptor. Nothing has
    // been written to it yet, so a refusal here costs nothing.
    if !nix::unistd::isatty(&file).unwrap_or(false) {
        return Err(TerminalError::NotATerminal {
            path: path.display().to_string(),
        });
    }

    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;

    /// The refusal that matters: the daemon opens this path
    /// read-write, so an arbitrary file would receive the prompt.
    #[test]
    fn a_path_outside_dev_is_refused_before_anything_opens_it() {
        let err = validate_terminal_path(Path::new("/etc/passwd")).expect_err("must refuse");

        assert!(matches!(err, TerminalError::NotADevicePath { .. }));
        assert!(
            err.to_string().contains("write the prompt into it"),
            "the refusal must say what the risk is: {err}"
        );
    }

    #[test]
    fn a_relative_path_is_refused() {
        assert!(validate_terminal_path(Path::new("pts/3")).is_err());
        assert!(validate_terminal_path(Path::new("../../dev/pts/3")).is_err());
    }

    #[test]
    fn a_device_path_passes_validation() {
        assert!(validate_terminal_path(Path::new("/dev/pts/3")).is_ok());
        assert!(validate_terminal_path(Path::new("/dev/ttys002")).is_ok());
    }

    /// A real pty opens and is recognised — the happy path, checked
    /// against an actual terminal rather than a stand-in.
    #[test]
    fn a_real_pty_opens_as_a_terminal() {
        let pty = nix::pty::openpty(None, None).expect("openpty");
        let name = nix::unistd::ttyname(pty.slave.as_fd()).expect("ttyname");

        let mut opened = open_client_terminal(&name).expect("a pty must be usable");
        // Writable, which is what the prompt needs.
        opened.write_all(b"").expect("write");
    }

    /// A file inside /dev that is not a terminal must still be
    /// refused: the directory check is a guard, not the answer.
    #[test]
    fn a_non_terminal_device_is_refused() {
        let err = open_client_terminal(Path::new("/dev/null")).expect_err("must refuse");

        assert!(matches!(err, TerminalError::NotATerminal { .. }), "{err:?}");
        assert!(
            err.to_string().contains("person at a keyboard"),
            "the refusal must say why: {err}"
        );
    }

    #[test]
    fn a_missing_device_reports_the_open_failure() {
        let err = open_client_terminal(Path::new("/dev/definitely-not-here")).expect_err("refuse");
        assert!(matches!(err, TerminalError::Unopenable { .. }), "{err:?}");
    }

    /// The test process may or may not have a terminal depending on
    /// how it was started, so this asserts the *shape* of the answer
    /// rather than its presence.
    #[test]
    fn the_callers_terminal_is_a_device_path_when_there_is_one() {
        if let Some(path) = caller_terminal() {
            assert!(
                validate_terminal_path(&path).is_ok(),
                "a terminal we resolved ourselves must pass our own check: {}",
                path.display()
            );
            assert_ne!(
                path,
                Path::new("/dev/tty"),
                "/dev/tty resolves per process and would name the daemon's own terminal"
            );
        }
    }
}
