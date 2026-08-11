//! UNIX-domain-socket binding for the secret-store agent per
//! [ADR-023] §3.3.
//!
//! UNIX-only: the whole module is gated `#![cfg(unix)]`. The
//! agent lib provides a stub `socket_stub` module on non-unix
//! targets so callers (e.g. `local-vault`) can still import
//! `AgentError` / `default_socket_path` cross-platform.
//!
//! This module is the *transport* layer of the daemon: it owns the
//! [`tokio::net::UnixListener`] backing `~/.devboy/secrets/agent.sock`,
//! tightens the filesystem permissions on the socket file (and its
//! parent directory) to user-only, and authenticates every accepted
//! connection by reading the peer's credentials. The JSON-RPC server
//! that runs on top (P4.2) consumes already-authenticated streams
//! and never has to re-check the UID.
//!
//! # Security boundary
//!
//! - **Filesystem ACL.** The socket file is created with mode `0600`
//!   and the parent directory with `0700`. Both settings are applied
//!   *after* `bind` (Tokio's bind does not let us pass a mode), with
//!   a tiny but real race window. The window is tolerable because
//!   the parent directory is `~/.devboy/secrets/` which already has
//!   user-only permissions and because the threat model
//!   (ADR-023 §3.3 *Security boundary*) explicitly does not claim
//!   isolation against a malicious local process running as the same
//!   user.
//! - **Peer-credential check.** Every accepted connection runs
//!   [`tokio::net::UnixStream::peer_cred`] and compares the UID to
//!   the daemon's effective UID. A mismatched UID is a hard reject:
//!   the connection is closed and a `tracing::warn` is emitted.
//!   This is defense-in-depth on top of the filesystem ACL — it
//!   catches misconfigurations where the socket happens to be
//!   accessible to another user.
//! - **Stale-socket recovery.** If a previous daemon crashed without
//!   removing the socket file, [`AgentListener::bind`] removes it
//!   before binding. Two daemons racing to bind is **not** handled
//!   here; the long-term solution is a PID file (P4.4) that proves
//!   liveness before deletion.
//!
//! # Cross-platform shape
//!
//! Windows is out of scope for the v1 daemon per ADR-023 §3.3. On
//! non-Unix targets the public functions return
//! [`AgentError::Unsupported`]. Everything else stays compileable
//! across the workspace.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

#![cfg(unix)]

use std::path::{Path, PathBuf};

use thiserror::Error;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

/// Conventional filename of the agent socket.
pub const SOCKET_FILENAME: &str = "agent.sock";

/// Conventional subdirectory under the user's config directory that
/// holds both the agent socket and the secret-framework files.
/// Matches the value used by `devboy-storage` so every secret-related
/// file lives in one place.
pub const SECRETS_SUBDIR: &str = "secrets";

/// Environment variable that overrides [`default_socket_path`].
/// Useful for tests, CI, and side-by-side daemon installations.
pub const SOCKET_PATH_ENV: &str = "DEVBOY_AGENT_SOCKET";

/// Filesystem mode for the socket file (`0600` — owner read/write only).
pub const SOCKET_MODE: u32 = 0o600;

/// Filesystem mode for the parent directory (`0700` — owner traversal +
/// read/write only).
pub const SOCKET_PARENT_DIR_MODE: u32 = 0o700;

// =============================================================================
// Errors
// =============================================================================

/// Failure modes for the agent's UNIX socket layer.
#[derive(Debug, Error)]
pub enum AgentError {
    /// Underlying I/O error — bind / mkdir / chmod / accept all
    /// surface through here.
    #[error("agent socket I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// `dirs::config_dir()` returned `None` — extremely unusual on
    /// supported platforms but worth a typed variant rather than a
    /// panic.
    #[error("could not resolve the user's config directory")]
    NoConfigDir,

    /// Peer's UID does not match the daemon's effective UID. The
    /// connection has already been closed; the variant exists so
    /// `accept_authenticated` callers can log the reject if they
    /// want.
    #[error(
        "peer credential mismatch: peer uid {peer} ≠ daemon uid {expected} (connection closed)"
    )]
    PeerUidMismatch {
        /// The daemon's effective UID.
        expected: u32,
        /// The connecting peer's UID as reported by the kernel.
        peer: u32,
    },

    /// ADR-024 §7 check A: the connecting client is an ancestor of
    /// this daemon and can therefore `ptrace` it.
    ///
    /// The stream is carried out of the accept path **still open**
    /// so the caller can send a `DaemonUntrusted` reply with its
    /// remediation before closing. Handing a client a dropped
    /// socket teaches it only that something broke; telling it why
    /// lets it stop and relay a specific command to the user.
    #[error("caller (pid {peer_pid}) is an ancestor of this daemon and could ptrace it")]
    CallerIsAncestor {
        /// The connecting client's PID.
        peer_pid: u32,
        /// The still-open stream, for sending the refusal.
        stream: UnixStream,
    },

    /// Non-Unix targets (currently Windows). The v1 daemon only
    /// supports Unix per ADR-023 §3.3; a future named-pipe transport
    /// will surface through a different module.
    #[error("the agent socket is only supported on Unix targets")]
    Unsupported,
}

// =============================================================================
// Path resolution
// =============================================================================

/// Resolve the canonical on-disk path to the agent socket.
///
/// Order of resolution:
///
/// 1. If [`SOCKET_PATH_ENV`] is set in the environment, use that
///    verbatim. Lets tests and CI point the agent at an arbitrary
///    location without touching the user's real config dir.
/// 2. Otherwise, `<config_dir>/devboy-tools/<SECRETS_SUBDIR>/<SOCKET_FILENAME>`.
pub fn default_socket_path() -> Result<PathBuf, AgentError> {
    if let Ok(s) = std::env::var(SOCKET_PATH_ENV) {
        if !s.is_empty() {
            return Ok(PathBuf::from(s));
        }
    }
    let dir = dirs::config_dir().ok_or(AgentError::NoConfigDir)?;
    Ok(dir
        .join("devboy-tools")
        .join(SECRETS_SUBDIR)
        .join(SOCKET_FILENAME))
}

// =============================================================================
// Listener — Unix
// =============================================================================

/// Owning handle to a bound, authenticated agent socket.
///
/// Drops the socket file on drop so a clean shutdown leaves no stale
/// path on disk. Crashes do not run the destructor; the next bind
/// in [`AgentListener::bind`] handles stale paths by unlinking them
/// before binding.
#[cfg(unix)]
#[derive(Debug)]
pub struct AgentListener {
    listener: UnixListener,
    path: PathBuf,
    expected_uid: u32,
}

#[cfg(unix)]
impl AgentListener {
    /// Bind a UNIX socket at `path`, tighten its permissions, and
    /// remember the daemon's effective UID for later peer-credential
    /// checks.
    ///
    /// Side effects:
    ///
    /// 1. Creates `path.parent()` with `mkdir -p` semantics.
    /// 2. Sets the parent directory's mode to [`SOCKET_PARENT_DIR_MODE`]
    ///    (`0700`).
    /// 3. Removes any stale `path` file (from a previous crashed
    ///    daemon).
    /// 4. Binds the listener.
    /// 5. Sets the socket file's mode to [`SOCKET_MODE`] (`0600`).
    pub async fn bind(path: &Path) -> Result<Self, AgentError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            let mut perms = std::fs::metadata(parent)?.permissions();
            perms.set_mode(SOCKET_PARENT_DIR_MODE);
            std::fs::set_permissions(parent, perms)?;
        }

        // Stale socket from a crashed daemon: unlink so bind succeeds.
        // Two daemons racing to bind is intentionally not handled in
        // v1 — the long-term solution is a PID-file lock (see
        // ADR-023 §3.3 lifecycle and epic phase P4.4).
        if path.exists() {
            std::fs::remove_file(path)?;
        }

        let listener = UnixListener::bind(path)?;

        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(SOCKET_MODE);
        std::fs::set_permissions(path, perms)?;

        let expected_uid = nix::unistd::geteuid().as_raw();

        Ok(Self {
            listener,
            path: path.to_path_buf(),
            expected_uid,
        })
    }

    /// Accept a new connection and verify the peer's UID matches the
    /// daemon's effective UID.
    ///
    /// Returns the authenticated [`UnixStream`] on success. On UID
    /// mismatch the stream is dropped (closing the connection) and
    /// an [`AgentError::PeerUidMismatch`] is returned so the caller
    /// can log the event; subsequent accepts proceed normally.
    pub async fn accept_authenticated(&self) -> Result<UnixStream, AgentError> {
        let (stream, _addr) = self.listener.accept().await?;
        let cred = stream.peer_cred()?;
        let peer_uid = cred.uid();
        if peer_uid != self.expected_uid {
            // Drop the stream explicitly so the FIN reaches the peer
            // before we return the error.
            drop(stream);
            return Err(AgentError::PeerUidMismatch {
                expected: self.expected_uid,
                peer: peer_uid,
            });
        }

        // ADR-024 §7 check A: a caller that is an ancestor of this
        // daemon can `ptrace` it, so every guarantee resting on
        // daemon memory being private is void for that caller.
        //
        // The caller is NOT dropped here. It gets a
        // `DaemonUntrusted` reply with its remediation first — a
        // client handed a dropped socket learns only that
        // something broke and will retry or improvise, whereas one
        // told why can stop and relay a specific command to the
        // user.
        if let Some(client_pid) = cred.pid() {
            let verdict = crate::provenance::check_connection(client_pid as u32);
            if verdict.should_refuse(crate::provenance::insecure_override_active()) {
                return Err(AgentError::CallerIsAncestor {
                    peer_pid: client_pid as u32,
                    stream,
                });
            }
        }

        Ok(stream)
    }

    /// Borrow the on-disk path for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The daemon's effective UID, captured at bind time. Exposed for
    /// `doctor`-style introspection.
    pub fn expected_uid(&self) -> u32 {
        self.expected_uid
    }
}

#[cfg(unix)]
impl Drop for AgentListener {
    fn drop(&mut self) {
        // Best-effort cleanup. If removing the file fails (it may
        // already be gone), there's nothing useful the daemon can
        // do during teardown.
        let _ = std::fs::remove_file(&self.path);
    }
}

// =============================================================================
// Listener — non-Unix stub
// =============================================================================

/// Non-Unix stub. Constructing this on Windows always fails with
/// [`AgentError::Unsupported`]; the type exists so cross-platform
/// callers can compile against the same API.
#[cfg(not(unix))]
pub struct AgentListener {
    _phantom: std::marker::PhantomData<()>,
}

#[cfg(not(unix))]
impl AgentListener {
    /// Always returns [`AgentError::Unsupported`].
    pub async fn bind(_path: &Path) -> Result<Self, AgentError> {
        Err(AgentError::Unsupported)
    }

    /// Always returns [`AgentError::Unsupported`].
    pub async fn accept_authenticated(&self) -> Result<(), AgentError> {
        Err(AgentError::Unsupported)
    }

    /// Stub.
    pub fn path(&self) -> &Path {
        Path::new("")
    }

    /// Stub.
    pub fn expected_uid(&self) -> u32 {
        0
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- default_socket_path ------------------------------------------------

    #[test]
    fn default_socket_path_uses_env_override() {
        // temp-env scopes the env-var mutation to the closure and
        // restores the previous value on drop, sidestepping the
        // edition-2024 `unsafe fn set_var` requirement.
        temp_env::with_var(SOCKET_PATH_ENV, Some("/tmp/devboy-test.sock"), || {
            let p = default_socket_path().unwrap();
            assert_eq!(p, PathBuf::from("/tmp/devboy-test.sock"));
        });
    }

    #[test]
    fn default_socket_path_uses_config_dir_when_env_unset() {
        temp_env::with_var::<&str, &str, _, _>(SOCKET_PATH_ENV, None, || {
            let p = default_socket_path().unwrap();
            let s = p.to_string_lossy();
            assert!(
                s.ends_with(&format!("/{SECRETS_SUBDIR}/{SOCKET_FILENAME}"))
                    || s.ends_with(&format!("\\{SECRETS_SUBDIR}\\{SOCKET_FILENAME}")),
                "unexpected default path: {s}"
            );
            assert!(s.contains("devboy-tools"));
        });
    }

    // -- Unix bind / permissions / connect / drop ---------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn bind_creates_socket_with_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        let listener = AgentListener::bind(&path).await.unwrap();

        let meta = std::fs::metadata(listener.path()).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, SOCKET_MODE);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bind_creates_parent_dir_with_mode_0700() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested-secrets-dir");
        let path = nested.join("agent.sock");

        let _listener = AgentListener::bind(&path).await.unwrap();

        let meta = std::fs::metadata(&nested).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, SOCKET_PARENT_DIR_MODE);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bind_replaces_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        std::fs::write(&path, b"stale").unwrap();
        // The "stale" file is a regular file rather than a socket;
        // bind must remove it and succeed.
        let _listener = AgentListener::bind(&path).await.unwrap();
        // After bind the file exists as a socket — its mode is 0600.
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, SOCKET_MODE);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn drop_removes_socket_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        {
            let _listener = AgentListener::bind(&path).await.unwrap();
            assert!(path.exists(), "socket file present while listener alive");
        }
        // Drop ran — file should be gone.
        assert!(!path.exists(), "socket file removed on drop");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn accept_authenticates_same_uid_connection() {
        // The peer connecting from the same process is, by
        // construction, the daemon's UID.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        let listener = AgentListener::bind(&path).await.unwrap();

        // Connect from this same process so peer_cred reports our
        // own UID.
        let connect = tokio::net::UnixStream::connect(&path);
        let accept = listener.accept_authenticated();
        let (client, server) = tokio::join!(connect, accept);
        let _client = client.unwrap();
        let server = server.unwrap();

        // Verify the server side actually got a peer-cred with the
        // expected UID.
        let cred = server.peer_cred().unwrap();
        assert_eq!(cred.uid(), listener.expected_uid());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn accept_returns_a_usable_stream() {
        // End-to-end: write a payload from the client, read on the
        // server side. Smoke-tests that the accepted stream is fully
        // operational, not just a UID-passed handle.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        let listener = AgentListener::bind(&path).await.unwrap();

        let payload = b"hello daemon";
        let server_task = tokio::spawn({
            async move {
                let mut s = listener.accept_authenticated().await.unwrap();
                let mut buf = vec![0u8; payload.len()];
                s.read_exact(&mut buf).await.unwrap();
                buf
            }
        });

        let mut client = tokio::net::UnixStream::connect(&path).await.unwrap();
        client.write_all(payload).await.unwrap();
        client.shutdown().await.unwrap();

        let received = server_task.await.unwrap();
        assert_eq!(&received, payload);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn expected_uid_matches_geteuid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        let listener = AgentListener::bind(&path).await.unwrap();
        assert_eq!(listener.expected_uid(), nix::unistd::geteuid().as_raw());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bind_fails_when_parent_cannot_be_created() {
        // /dev/null/foo is impossible to mkdir; bind must surface
        // the I/O error rather than panicking.
        let path = std::path::Path::new("/dev/null/no-can-do/agent.sock");
        let err = AgentListener::bind(path).await.unwrap_err();
        assert!(matches!(err, AgentError::Io(_)));
    }
}
