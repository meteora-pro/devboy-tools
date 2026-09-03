//! A daemon with no terminal of its own can still ask a human, when
//! the caller lends one (ADR-024 §7, Ф14).
//!
//! # What was broken
//!
//! §7 requires the daemon to be reparented to init, and a reparented
//! process has no controlling terminal. The prompt channel was the
//! daemon's own `/dev/tty`. So in the configuration the ADR
//! recommends, `vault.request_unlock` could only answer "no prompt
//! surface": interactive unlock did not work at all, and the only way
//! in was an environment variable.
//!
//! # What these tests hold
//!
//! Every one of them runs against a **real pseudo-terminal** with a
//! real passphrase typed into it, because the failure modes here are
//! all in the plumbing — an unopened device, an unrestored echo, a
//! prompt written to the wrong screen — and none of them are visible
//! to a test that stands in a fake.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::fd::AsFd;

use devboy_secrets_agent::client_terminal::{caller_terminal, open_client_terminal};
use devboy_secrets_agent::prompt::TtyPrompt;
use nix::pty::{OpenptyResult, openpty};
use secrecy::ExposeSecret;

/// A pty pair, kept together so neither end closes early.
struct Pty {
    inner: OpenptyResult,
}

impl Pty {
    fn new() -> Self {
        Self {
            inner: openpty(None, None).expect("openpty"),
        }
    }

    /// Path of the slave end — what a client would name.
    fn slave_path(&self) -> std::path::PathBuf {
        nix::unistd::ttyname(self.inner.slave.as_fd()).expect("ttyname")
    }

    /// Block until the reader has turned echo off.
    ///
    /// The prompt disables echo with `TCSAFLUSH`, which discards
    /// anything typed *before* the prompt appeared — deliberate, so
    /// a stray keystroke cannot be read as a passphrase. Typing
    /// before that point is therefore not "early", it is lost: the
    /// flush eats the line and the read waits for a second one that
    /// never comes.
    ///
    /// These tests used to race it — one with a 50 ms sleep, one
    /// with nothing at all — and on macOS they lost. There the read
    /// does not even fail: Linux returns EOF once the last master fd
    /// closes, the BSD side just blocks. Two CI jobs ran for six
    /// hours and were killed with no diagnosis.
    fn wait_until_the_reader_is_listening(&self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while devboy_secrets_agent::prompt::echo_enabled(self.inner.slave.as_fd())
            .expect("read the terminal state")
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the prompt never turned echo off, so it never got as far as reading"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Type `text` as if a human had, and return what was displayed.
    fn answer_with(&self, text: &str) -> String {
        self.wait_until_the_reader_is_listening();
        let mut master =
            std::fs::File::from(self.inner.master.try_clone().expect("clone the master end"));
        master
            .write_all(format!("{text}\n").as_bytes())
            .expect("type the passphrase");
        master.flush().expect("flush");

        // Whatever the daemon printed — the prompt, and the echo the
        // terminal produced before echo was turned off.
        let mut seen = vec![0u8; 256];
        let read = master.read(&mut seen).unwrap_or(0);
        seen.truncate(read);
        String::from_utf8_lossy(&seen).into_owned()
    }
}

/// The property the whole change exists for: a daemon that cannot ask
/// on its own screen asks on the one it was lent, and gets the
/// answer.
#[test]
fn a_lent_terminal_carries_the_prompt_and_the_answer() {
    let pty = Pty::new();
    let path = pty.slave_path();

    let mut prompt =
        TtyPrompt::from_file(open_client_terminal(&path).expect("the lent terminal must open"));

    // Type from another thread, because reading blocks.
    let writer = std::thread::spawn({
        let pty = Pty {
            inner: OpenptyResult {
                master: pty.inner.master.try_clone().expect("clone master"),
                slave: pty.inner.slave.try_clone().expect("clone slave"),
            },
        };
        move || pty.answer_with("correct horse battery staple")
    });

    // On its own thread with a deadline. `read_passphrase` blocks in
    // a syscall, so `tokio::time::timeout` around it does nothing —
    // measured — and a stuck read would otherwise hang the whole job
    // rather than fail it.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(prompt.read_passphrase("Unlock the devboy vault: "));
    });
    let passphrase = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the reader blocked forever on the lent terminal")
        .expect("read the passphrase from the lent terminal");

    let displayed = writer.join().expect("writer thread");

    assert_eq!(
        passphrase.expose_secret(),
        "correct horse battery staple",
        "what the human typed must be what the daemon got"
    );
    assert!(
        displayed.contains("Unlock the devboy vault:"),
        "the prompt must appear on the lent screen, not somewhere else: {displayed:?}"
    );
}

/// Echo is turned off to read a passphrase, and turning it back on is
/// not optional: a user left in a shell that shows nothing they type,
/// with no explanation, is worse off than one who failed to unlock.
#[test]
fn echo_is_restored_on_the_lent_terminal_afterwards() {
    let pty = Pty::new();
    let path = pty.slave_path();

    let before = devboy_secrets_agent::prompt::echo_enabled(pty.inner.slave.as_fd())
        .expect("read the terminal state");
    assert!(before, "a fresh pty should start with echo on");

    let mut prompt =
        TtyPrompt::from_file(open_client_terminal(&path).expect("the lent terminal must open"));

    let writer = std::thread::spawn({
        let pty = Pty {
            inner: OpenptyResult {
                master: pty.inner.master.try_clone().expect("clone master"),
                slave: pty.inner.slave.try_clone().expect("clone slave"),
            },
        };
        move || pty.answer_with("something")
    });

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(prompt.read_passphrase("Unlock: ").is_ok());
    });
    rx.recv_timeout(std::time::Duration::from_secs(30))
        .expect("the reader blocked forever on the lent terminal");
    writer.join().expect("writer thread");

    let after = devboy_secrets_agent::prompt::echo_enabled(pty.inner.slave.as_fd())
        .expect("read the terminal state");
    assert!(
        after,
        "echo must be back on after the prompt, or the user is left typing blind"
    );
}

/// The client half: a process with a terminal resolves it to a
/// concrete path, never to `/dev/tty` — which would name the
/// daemon's own (absent) terminal instead.
#[test]
fn a_resolved_terminal_is_a_concrete_device_that_the_daemon_can_open() {
    let Some(path) = caller_terminal() else {
        // No terminal under `cargo test` in CI; the property is
        // vacuous there and asserting it would be a false green.
        return;
    };

    assert_ne!(path, std::path::Path::new("/dev/tty"));
    open_client_terminal(&path).expect("what a client resolves, a daemon must be able to open");
}
