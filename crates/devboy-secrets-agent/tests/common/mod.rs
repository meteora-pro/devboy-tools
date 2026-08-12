//! Test harness for driving a real `devboy-secrets-agent` process
//! (ADR-024 §7 test track).
//!
//! The provenance checks are about **process topology** — who
//! started the daemon, and whether a caller sits in its ancestry.
//! None of that can be exercised in-process: it needs a real
//! binary, spawned in a specific relationship to the test, talking
//! over a real socket.
//!
//! Everything is confined to a `tempfile::TempDir`, so a test can
//! never read or corrupt the developer's own vault.
//!
//! # Spawn modes
//!
//! - [`SpawnMode::Child`] — the daemon is a direct child of the
//!   test process, which is exactly the layout check A must
//!   detect.
//! - [`SpawnMode::Orphaned`] — double-forked via `setsid` so it
//!   reparents to init, the layout a correctly installed daemon
//!   has.
//!
//! # Timeouts everywhere
//!
//! Every wait is bounded. A daemon that never becomes ready must
//! fail the test in seconds rather than hang CI until the job
//! times out with no useful output.

#![allow(dead_code)] // Each integration test uses a subset.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// How long to wait for the socket to appear before giving up.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for a single JSON-RPC reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll interval while waiting for readiness.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Relationship between the test process and the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnMode {
    /// Direct child of the test process — the untrusted layout.
    Child,
    /// Reparented to init via `setsid` — the correct layout.
    Orphaned,
}

/// A daemon under test, with its own vault, socket and config.
pub struct DaemonHarness {
    /// Keeps the temp dir alive; dropping it removes everything.
    _dir: TempDir,
    socket_path: PathBuf,
    vault_path: PathBuf,
    child: Option<Child>,
    mode: SpawnMode,
}

impl DaemonHarness {
    /// Path to the built daemon binary.
    ///
    /// Cargo puts integration-test binaries in
    /// `target/<profile>/deps/`, so the daemon is two levels up.
    fn daemon_bin() -> PathBuf {
        let mut path = std::env::current_exe().expect("test binary path");
        path.pop(); // deps/
        path.pop(); // <profile>/
        path.push("devboy-secrets-agent");
        assert!(
            path.exists(),
            "daemon binary not found at {} — run `cargo build -p devboy-secrets-agent` first",
            path.display()
        );
        path
    }

    /// Prepare an isolated environment without starting anything.
    pub fn prepare() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        // Keep the socket path short: `sun_path` is ~108 bytes and
        // a nested temp path can overflow it.
        let socket_path = dir.path().join("a.sock");
        let vault_path = dir.path().join("vault.dvb");

        Self {
            _dir: dir,
            socket_path,
            vault_path,
            child: None,
            mode: SpawnMode::Child,
        }
    }

    /// Start the daemon, returning `Err` with captured output when
    /// it refuses.
    ///
    /// `allow_untrusted` sets the documented override; leaving it
    /// `false` is what exercises the fail-closed path.
    pub fn start(
        &mut self,
        mode: SpawnMode,
        allow_untrusted: bool,
    ) -> Result<(), DaemonStartFailure> {
        self.mode = mode;

        let mut cmd = match mode {
            SpawnMode::Child => Command::new(Self::daemon_bin()),
            SpawnMode::Orphaned => {
                // Background the daemon from a throwaway shell: the
                // shell forks, the daemon keeps running, the shell
                // exits, and the kernel reparents the daemon to
                // init. That is exactly the layout this mode is for
                // — the daemon must not be a direct child of the
                // test process.
                //
                // This used to be `setsid --fork`, which works on
                // Linux and does not exist on macOS at all —
                // `setsid` is util-linux, and the three tests in
                // this mode failed there with a bare
                // "No such file or directory". A shell is present
                // on every platform the daemon supports.
                let mut c = Command::new("sh");
                c.arg("-c").arg(format!(
                    "exec {} &",
                    shell_quote(&Self::daemon_bin().display().to_string())
                ));
                c
            }
        };

        // Output goes to files, not pipes.
        //
        // An orphaned daemon outlives the `setsid` process we
        // waited on, and it inherits the write end of any pipe —
        // so `wait_with_output()` would block until the daemon
        // itself exits, hanging the test forever. Files are
        // readable regardless of who is still holding the fd.
        let stdout_file = std::fs::File::create(self.stdout_path()).expect("stdout file");
        let stderr_file = std::fs::File::create(self.stderr_path()).expect("stderr file");

        cmd.env("DEVBOY_AGENT_SOCKET", &self.socket_path)
            .env("DEVBOY_VAULT_PATH", &self.vault_path)
            // Never touch the developer's real config.
            .env("XDG_CONFIG_HOME", self._dir.path())
            .env("HOME", self._dir.path())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));

        if allow_untrusted {
            cmd.env("DEVBOY_INSECURE_ALLOW_UNTRUSTED_DAEMON", "1");
        } else {
            cmd.env_remove("DEVBOY_INSECURE_ALLOW_UNTRUSTED_DAEMON");
        }

        let child = cmd.spawn().expect("spawn daemon");
        self.child = Some(child);

        match self.wait_until_ready() {
            Ok(()) => Ok(()),
            Err(_) => {
                // Not ready in time: either it refused to start or
                // it is wedged. Either way the captured output is
                // what the test wants to assert on.
                let output = self.take_output();
                Err(DaemonStartFailure {
                    stderr: output.stderr,
                    exit_status: output.exit_status,
                })
            }
        }
    }

    /// Wait for the socket to appear and accept a connection.
    fn wait_until_ready(&mut self) -> Result<(), ()> {
        let deadline = Instant::now() + READY_TIMEOUT;
        while Instant::now() < deadline {
            if self.socket_path.exists() && UnixStream::connect(&self.socket_path).is_ok() {
                return Ok(());
            }

            // Bail out early when the process we launched is gone
            // *and* it left a refusal behind — that is a daemon
            // that decided not to run, not one still starting.
            //
            // For `Orphaned` the handle refers to `setsid`, which
            // exits immediately by design, so its exit alone means
            // nothing; the stderr content is what distinguishes
            // the two cases.
            if let Some(child) = self.child.as_mut()
                && matches!(child.try_wait(), Ok(Some(_)))
                && self.read_stderr().contains("refusing to start")
            {
                return Err(());
            }

            std::thread::sleep(POLL_INTERVAL);
        }
        Err(())
    }

    /// Send one JSON-RPC request and read one reply.
    ///
    /// Bounded by [`REPLY_TIMEOUT`] so a daemon that accepts the
    /// connection and then says nothing — the shape a refused
    /// caller would see if the refusal reply were missing — fails
    /// the test instead of hanging it.
    pub fn rpc(&self, method: &str, params: serde_json::Value) -> std::io::Result<String> {
        let stream = UnixStream::connect(&self.socket_path)?;
        stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
        stream.set_write_timeout(Some(REPLY_TIMEOUT))?;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let mut writer = stream.try_clone()?;
        writeln!(writer, "{request}")?;
        writer.flush()?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        Ok(line)
    }

    /// Connect without sending anything, to observe whether the
    /// daemon accepts or refuses the connection itself.
    pub fn connect_raw(&self) -> std::io::Result<UnixStream> {
        let stream = UnixStream::connect(&self.socket_path)?;
        stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
        Ok(stream)
    }

    /// Stop the daemon and collect whatever it printed.
    ///
    /// Never blocks on the daemon's own lifetime: output is read
    /// from files, and an orphaned daemon is reaped by the unique
    /// socket path it was told to bind rather than through a child
    /// handle it no longer has.
    pub fn take_output(&mut self) -> DaemonOutput {
        let exit_status = match self.child.take() {
            Some(mut child) => {
                let _ = child.kill();
                // For `Orphaned` this waits on `setsid`, which has
                // already exited; for `Child` it waits on the
                // daemon itself. Either way it returns promptly.
                child.wait().ok().and_then(|s| s.code())
            }
            None => None,
        };

        if self.mode == SpawnMode::Orphaned {
            // The real daemon is detached from us, so match on the
            // socket path — unique to this harness instance, so
            // this can never hit another test's daemon or the
            // developer's own.
            let _ = Command::new("pkill")
                .args(["-f", &self.socket_path.display().to_string()])
                .status();
        }

        DaemonOutput {
            stderr: std::fs::read_to_string(self.stderr_path()).unwrap_or_default(),
            stdout: std::fs::read_to_string(self.stdout_path()).unwrap_or_default(),
            exit_status,
        }
    }

    /// Read the daemon's stderr without stopping it.
    pub fn read_stderr(&self) -> String {
        std::fs::read_to_string(self.stderr_path()).unwrap_or_default()
    }

    fn stdout_path(&self) -> PathBuf {
        self._dir.path().join("daemon.out")
    }

    fn stderr_path(&self) -> PathBuf {
        self._dir.path().join("daemon.err")
    }

    /// The socket this daemon was told to bind.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The vault file this daemon was told to use.
    pub fn vault_path(&self) -> &Path {
        &self.vault_path
    }
}

impl Drop for DaemonHarness {
    fn drop(&mut self) {
        // Never leave a daemon behind holding a temp socket.
        let _ = self.take_output();
    }
}

/// What a daemon printed, and how it exited.
#[derive(Debug, Default, Clone)]
pub struct DaemonOutput {
    pub stderr: String,
    pub stdout: String,
    pub exit_status: Option<i32>,
}

/// A daemon that refused to start, with its explanation.
#[derive(Debug, Clone)]
pub struct DaemonStartFailure {
    pub stderr: String,
    pub exit_status: Option<i32>,
}

impl DaemonStartFailure {
    /// Whether the refusal mentions `needle`, for asserting that
    /// the message is actionable rather than merely present.
    pub fn mentions(&self, needle: &str) -> bool {
        self.stderr.contains(needle)
    }
}

/// Single-quote a path for `sh -c`.
///
/// The daemon binary lives under `target/`, which is usually tame —
/// but a checkout under a path with a space would otherwise split
/// into two arguments and fail in a way that looks like the binary
/// is missing, which is precisely the confusion this mode just
/// spent a CI cycle on.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
