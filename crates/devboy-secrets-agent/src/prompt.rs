//! Asking the user for a passphrase on a channel the agent does not
//! own (ADR-024 §7, Ф5b).
//!
//! # Why the daemon has to do the asking
//!
//! An agent that can run shell commands as the user can replace the
//! `devboy` binary, prepend to `.bashrc`, or set `LD_PRELOAD` — so a
//! passphrase typed into a process the agent controls is a
//! passphrase the agent can read. Moving the prompt into the daemon
//! only helps if the daemon reads from a terminal the agent is not
//! attached to; otherwise the move is theatre.
//!
//! That is what this module provides: a handle on the **daemon's
//! own** controlling terminal, opened via `/dev/tty`, which resolves
//! per-process and cannot be redirected by whoever launched the
//! client.
//!
//! # The failure mode this code is mostly about
//!
//! Reading a passphrase means turning off terminal echo. If the
//! restore is skipped — an error path, an early return, a panic
//! between the two calls — the user is left in a shell that shows
//! nothing they type, with no indication why. That is far worse than
//! failing to read the passphrase.
//!
//! So the restore is a [`Drop`] guard rather than a line at the end
//! of the happy path. Every exit from [`TtyPrompt::read_passphrase`]
//! runs it, including an unwind.

#![cfg(unix)]

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use secrecy::SecretString;
use zeroize::Zeroizing;

/// Longest passphrase accepted, in bytes.
///
/// The buffer is allocated once at this size and never grown, so a
/// reallocation cannot leave a copy of the passphrase behind in
/// freed heap. A limit is the price of that guarantee; 1 KiB is far
/// past any real passphrase.
const MAX_PASSPHRASE_LEN: usize = 1024;

/// A handle on the daemon's own controlling terminal.
#[derive(Debug)]
pub struct TtyPrompt {
    tty: File,
}

impl TtyPrompt {
    /// Open the calling process's controlling terminal.
    ///
    /// Returns `None` when there is none — a daemon started by
    /// systemd or launchd has no terminal, which is a fact to route
    /// around rather than an error to report. Callers turn it into
    /// the "no prompt surface" branch.
    pub fn open() -> Option<Self> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()
            .map(|tty| Self { tty })
    }

    /// Build a prompt on an already-open terminal.
    ///
    /// Exists so tests can drive the real read/echo path over a
    /// `openpty` slave instead of the process's actual terminal.
    pub fn from_file(tty: File) -> Self {
        Self { tty }
    }

    /// Identify the terminal, so a caller can check it is not the
    /// same one the client is attached to.
    ///
    /// Two processes sharing a terminal makes the whole trusted-path
    /// argument collapse: whoever else has it open can read what is
    /// typed. `(device, inode)` is stable for this purpose and
    /// cheaper than resolving the tty name.
    pub fn identity(&self) -> std::io::Result<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let meta = self.tty.metadata()?;
        Ok((meta.rdev(), meta.ino()))
    }

    /// Print `prompt` and read one line with echo disabled.
    ///
    /// The trailing newline is stripped. Echo is restored on every
    /// path out of this function, including a panic.
    pub fn read_passphrase(&mut self, prompt: &str) -> std::io::Result<SecretString> {
        write!(self.tty, "{prompt}")?;
        self.tty.flush()?;

        // Disabling echo and restoring it are two halves of one
        // operation; the guard is what keeps them together.
        let _echo = EchoGuard::disable(self.tty.as_fd())?;

        let mut buf = Zeroizing::new(Vec::with_capacity(MAX_PASSPHRASE_LEN));
        let read = BufReader::new(self.tty.try_clone()?)
            .take(MAX_PASSPHRASE_LEN as u64)
            .read_until(b'\n', &mut buf)?;

        // The user's Return never echoed, so the cursor is still on
        // the prompt line. Move it down or the next output lands
        // beside the prompt.
        writeln!(self.tty)?;
        self.tty.flush()?;

        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the terminal closed before a passphrase was entered",
            ));
        }
        if !buf.ends_with(b"\n") && read >= MAX_PASSPHRASE_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("passphrase longer than {MAX_PASSPHRASE_LEN} bytes"),
            ));
        }

        while buf.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            buf.pop();
        }

        let text = std::str::from_utf8(&buf).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "passphrase was not valid UTF-8",
            )
        })?;

        Ok(SecretString::from(text.to_owned()))
    }
}

/// Restores terminal echo when dropped.
///
/// Holding an [`OwnedFd`] duplicate rather than a borrow means the
/// guard can still reach the terminal during an unwind, when the
/// borrow it was built from may already be gone.
struct EchoGuard {
    fd: OwnedFd,
    original: nix::sys::termios::Termios,
}

impl EchoGuard {
    /// Turn echo off, remembering how to put it back.
    fn disable(fd: BorrowedFd<'_>) -> std::io::Result<Self> {
        use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};

        let owned = fd.try_clone_to_owned()?;
        let original = tcgetattr(&owned).map_err(std::io::Error::from)?;

        let mut quiet = original.clone();
        quiet.local_flags.remove(LocalFlags::ECHO);
        // Keep ECHONL so the terminal still advances a line on
        // Return; without it some terminals swallow the newline and
        // the display looks frozen.
        quiet.local_flags.insert(LocalFlags::ECHONL);
        // TCSAFLUSH: apply after pending output drains and discard
        // pending input, so anything typed ahead of the prompt is
        // not silently accepted as part of the passphrase.
        tcsetattr(&owned, SetArg::TCSAFLUSH, &quiet).map_err(std::io::Error::from)?;

        Ok(Self {
            fd: owned,
            original,
        })
    }
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        use nix::sys::termios::{SetArg, tcsetattr};
        // Nothing useful to do with a failure here: the read is
        // over, and the alternative to a best-effort restore is
        // leaving the terminal mute.
        let _ = tcsetattr(&self.fd, SetArg::TCSANOW, &self.original);
    }
}

/// Whether `fd` currently echoes input.
///
/// Public for the tests that assert the guard put things back; also
/// useful to a caller that wants to sanity-check a terminal before
/// prompting on it.
pub fn echo_enabled(fd: impl AsFd) -> std::io::Result<bool> {
    use nix::sys::termios::{LocalFlags, tcgetattr};
    let attrs = tcgetattr(fd.as_fd()).map_err(std::io::Error::from)?;
    Ok(attrs.local_flags.contains(LocalFlags::ECHO))
}

#[cfg(test)]
mod tests {
    use super::*;

    use nix::pty::openpty;
    use secrecy::ExposeSecret;

    /// A real terminal pair, so the tests exercise termios rather
    /// than a stand-in. A mock would prove nothing here: the whole
    /// module is terminal manipulation.
    struct Pty {
        controller: File,
        device: File,
    }

    fn pty() -> Pty {
        // `openpty` hands back owned descriptors, so this converts
        // without `unsafe` — which the crate forbids outright.
        let pair = openpty(None, None).expect("openpty");
        Pty {
            controller: File::from(pair.master),
            device: File::from(pair.slave),
        }
    }

    impl Pty {
        fn device(&self) -> File {
            self.device.try_clone().expect("clone device")
        }
    }

    /// Drive one `read_passphrase` the way a user would: wait until
    /// the prompt has actually disabled echo, then type.
    ///
    /// Two details are load-bearing and neither is politeness.
    ///
    /// The wait: `TCSAFLUSH` discards whatever is already in the
    /// input queue, so anything typed before that point is
    /// deliberately thrown away. Spinning on the observable echo
    /// state gives a sync point with no sleeps and no race.
    ///
    /// The drain thread: `TCSAFLUSH` also waits for pending *output*
    /// to drain, and the prompt has just been written. With nobody
    /// reading the controller side, that wait never completes on
    /// macOS and the prompt hangs before it can disable echo — which
    /// is what made every test here fail there while passing on
    /// Linux, where the buffer happened to be large enough. A real
    /// terminal is always being drained by its emulator, so draining
    /// is also the more faithful simulation.
    fn read_with_input(
        pty: &Pty,
        typed: impl AsRef<[u8]>,
        prompt_text: &str,
    ) -> (std::io::Result<SecretString>, Vec<u8>) {
        use std::sync::{Arc, Mutex};

        let device = pty.device();
        let observer = pty.device();
        let mut controller = pty.controller.try_clone().expect("clone controller");
        let typed = typed.as_ref().to_vec();
        let prompt_text = prompt_text.to_owned();

        // Collected so a test can still assert on what the terminal
        // displayed, even though the bytes are consumed as they
        // arrive.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_writer = Arc::clone(&seen);
        let mut drain_source = pty.controller.try_clone().expect("clone for drain");
        let drain = std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            while let Ok(n) = drain_source.read(&mut buf) {
                if n == 0 {
                    break;
                }
                seen_writer
                    .lock()
                    .expect("lock")
                    .extend_from_slice(&buf[..n]);
            }
        });

        let reader =
            std::thread::spawn(move || TtyPrompt::from_file(device).read_passphrase(&prompt_text));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while echo_enabled(&observer).expect("read echo state") {
            assert!(
                std::time::Instant::now() < deadline,
                "the prompt never disabled echo, so the test would have typed into the void"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        controller.write_all(&typed).expect("write to controller");
        controller.flush().expect("flush");

        let result = reader.join().expect("reader thread");

        // Give the drain a moment to pick up the trailing newline,
        // then take what it collected. The thread is left to end
        // when the Pty's own controller handle drops with the
        // fixture; joining it here would block until then.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let displayed = seen.lock().expect("lock").clone();
        drop(controller);
        let _ = drain;

        (result, displayed)
    }

    #[test]
    fn a_passphrase_is_read_from_the_terminal() {
        let pty = pty();
        let (result, _) = read_with_input(&pty, "correct horse battery staple\n", "Passphrase: ");
        let secret = result.expect("read succeeds");

        assert_eq!(secret.expose_secret(), "correct horse battery staple");
    }

    /// The type-ahead discard is a deliberate property, not an
    /// accident of the test: anything typed while echo was still on
    /// was displayed on screen, so accepting it would mean part of
    /// the passphrase had been shown.
    #[test]
    fn input_typed_before_the_prompt_is_discarded() {
        let pty = pty();
        let mut controller = pty.controller.try_clone().expect("clone");

        // Typed while echo is still on — and therefore echoed.
        writeln!(controller, "shoulder-surfed").expect("write");
        controller.flush().expect("flush");

        let (result, _) = read_with_input(&pty, "typed-at-the-prompt\n", "> ");
        let secret = result.expect("read");
        assert_eq!(
            secret.expose_secret(),
            "typed-at-the-prompt",
            "input from before the prompt must not become part of the passphrase"
        );
    }

    /// The prompt has to actually appear, or the user is staring at
    /// a blank line wondering whether anything is waiting on them.
    /// The prompt has to actually appear, or the user is staring at
    /// a blank line wondering whether anything is waiting on them.
    #[test]
    fn the_prompt_text_reaches_the_terminal() {
        let pty = pty();
        let (result, displayed) = read_with_input(&pty, "pw\n", "Unlock vault: ");
        result.expect("read");
        let text = String::from_utf8_lossy(&displayed);
        assert!(
            text.contains("Unlock vault:"),
            "the prompt should be visible on the terminal, saw {text:?}"
        );
    }

    /// The one that matters most: a terminal left without echo is a
    /// terminal the user has to fix by hand, usually without knowing
    /// how.
    #[test]
    fn echo_is_restored_after_a_successful_read() {
        let pty = pty();
        let observer = pty.device();
        assert!(
            echo_enabled(&observer).expect("initial state"),
            "a fresh pty should echo"
        );

        read_with_input(&pty, "pw\n", "> ").0.expect("read");

        assert!(
            echo_enabled(&observer).expect("state after"),
            "echo must be restored once the passphrase has been read"
        );
    }

    /// ...and restored on the error path too, which is exactly where
    /// a hand-written restore would have been forgotten.
    ///
    /// The error is invalid UTF-8 rather than a closed terminal: a
    /// closed one returns `EIO` from `tcgetattr` as well, so the
    /// test could not see the state it is asking about. This way the
    /// read genuinely fails while the terminal stays observable.
    #[test]
    fn echo_is_restored_when_the_read_fails() {
        let pty = pty();
        let observer = pty.device();

        // A lone 0xff byte can never appear in valid UTF-8.
        let (result, _) = read_with_input(&pty, [b'p', b'w', 0xff, b'\n'], "> ");
        assert!(
            result.is_err(),
            "a non-UTF-8 passphrase should fail rather than be silently mangled"
        );

        assert!(
            echo_enabled(&observer).expect("state after failure"),
            "echo must be restored even when the read fails"
        );
    }

    /// And on an unwind, which is the path a hand-written restore
    /// cannot cover at all.
    #[test]
    fn echo_is_restored_when_the_reader_panics() {
        let pty = pty();
        let device = pty.device();

        let device_for_panic = pty.device();
        let outcome = std::panic::catch_unwind(move || {
            let _guard = EchoGuard::disable(device_for_panic.as_fd()).expect("disable");
            panic!("something went wrong mid-read");
        });

        assert!(outcome.is_err(), "the panic should propagate");
        assert!(
            echo_enabled(&device).expect("state after panic"),
            "echo must be restored during an unwind"
        );
    }

    /// Echo is genuinely off *while* reading — otherwise the
    /// passphrase is displayed and every other guarantee here is
    /// pointless.
    #[test]
    fn echo_is_off_during_the_read() {
        let pty = pty();
        let device = pty.device();

        let guard = EchoGuard::disable(device.as_fd()).expect("disable");
        assert!(
            !echo_enabled(&device).expect("state during"),
            "echo must be off while the passphrase is being typed"
        );

        drop(guard);
        assert!(echo_enabled(&device).expect("state after"));
    }

    #[test]
    fn a_trailing_carriage_return_is_stripped() {
        let pty = pty();
        let (result, _) = read_with_input(&pty, "windows-style\r\n", "> ");
        let secret = result.expect("read");

        assert_eq!(secret.expose_secret(), "windows-style");
    }

    /// An empty line is a legitimate read that yields an empty
    /// passphrase — rejecting it belongs to the vault, which knows
    /// whether an empty passphrase is acceptable. Silently treating
    /// it as a failure here would hide a user's actual input.
    #[test]
    fn an_empty_line_reads_as_an_empty_passphrase() {
        let pty = pty();
        let (result, _) = read_with_input(&pty, "\n", "> ");
        let secret = result.expect("read");

        assert_eq!(secret.expose_secret(), "");
    }

    /// Two prompts on different terminals must be distinguishable,
    /// since that comparison is what stops the daemon prompting on
    /// the terminal the client is already watching.
    #[test]
    fn identity_distinguishes_two_terminals() {
        let first = pty();
        let second = pty();

        let a = TtyPrompt::from_file(first.device()).identity().expect("a");
        let b = TtyPrompt::from_file(second.device()).identity().expect("b");

        assert_ne!(
            a, b,
            "two separate ptys must not look like the same terminal"
        );

        // ...and the same terminal must compare equal to itself,
        // or the check would reject every legitimate case.
        let a_again = TtyPrompt::from_file(first.device()).identity().expect("a2");
        assert_eq!(a, a_again);
    }
}
