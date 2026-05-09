//! On-demand spawn of the `devboy-secrets-agent` daemon per
//! [ADR-023] §3.3.
//!
//! The CLI is the *client* of the agent socket. When a `secret.get`
//! is needed and no daemon is running, the CLI spawns one detached
//! and waits for the socket to come up before issuing JSON-RPC. This
//! module is the spawn helper; the actual `secret.get` call sites
//! land in the source-router phase (P5 / P8).
//!
//! ## API surface
//!
//! - [`is_socket_live`] — async, non-blocking liveness probe. Treats
//!   stale socket files (left over from a crashed daemon) as
//!   *not* live.
//! - [`find_agent_binary`] — locate the `devboy-secrets-agent`
//!   binary (env override → sibling of `current_exe` → `PATH`).
//! - [`spawn_detached_agent`] — fire-and-forget detached spawn with
//!   stdio redirected to `/dev/null`. The agent itself calls
//!   `setsid()` inside its own `main` so it survives a `Ctrl-C` to
//!   the spawning shell.
//! - [`ensure_agent_running`] — idempotent: probe → spawn → poll
//!   until the socket accepts.
//!
//! ## Design notes
//!
//! - The CLI cannot do `Command::pre_exec(setsid)` itself because
//!   `pre_exec` is `unsafe` and the workspace forbids `unsafe_code`.
//!   So the agent self-detaches; the CLI just spawns and forgets.
//! - `is_socket_live` connects with a 200 ms timeout rather than a
//!   bare `path.exists()` check. A leftover regular file from a
//!   crashed daemon would falsely report `true` otherwise.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

/// Conventional name of the agent binary.
pub const AGENT_BIN_NAME: &str = "devboy-secrets-agent";

/// Env var the CLI consults when looking for the agent binary.
/// An absolute path here overrides the sibling-of-current_exe and
/// `PATH`-walk fallbacks; useful for tests, CI, and side-by-side
/// installations.
pub const AGENT_BIN_ENV: &str = "DEVBOY_AGENT_BIN";

/// Env var passed to the spawned agent so it knows which vault file
/// to operate on. Mirrors the agent's own resolver.
pub const VAULT_PATH_ENV: &str = "DEVBOY_VAULT_PATH";

/// Env var passed to the spawned agent so it knows which socket
/// path to bind to. Mirrors `devboy_secrets_agent::SOCKET_PATH_ENV`.
pub const AGENT_SOCKET_ENV: &str = "DEVBOY_AGENT_SOCKET";

/// How often [`ensure_agent_running`] polls the socket while waiting
/// for the spawned agent to start accepting.
pub const SPAWN_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Default cap on the wait-for-socket loop in [`ensure_agent_running`].
pub const DEFAULT_SPAWN_TIMEOUT: Duration = Duration::from_secs(5);

const CONNECT_PROBE_TIMEOUT: Duration = Duration::from_millis(200);

// =============================================================================
// Liveness probe
// =============================================================================

/// Async-aware liveness probe. Returns `true` only if the socket file
/// exists *and* a peer accepts a UNIX-domain connection within
/// [`CONNECT_PROBE_TIMEOUT`].
///
/// A bare `path.exists()` check would incorrectly report `true` for
/// stale socket files (or regular files) left behind by a crashed
/// daemon, so an actual `connect()` is required to be honest.
#[cfg(unix)]
pub async fn is_socket_live(socket_path: &Path) -> bool {
    if !socket_path.exists() {
        return false;
    }
    matches!(
        tokio::time::timeout(
            CONNECT_PROBE_TIMEOUT,
            tokio::net::UnixStream::connect(socket_path),
        )
        .await,
        Ok(Ok(_))
    )
}

#[cfg(not(unix))]
pub async fn is_socket_live(_socket_path: &Path) -> bool {
    false
}

// =============================================================================
// Binary discovery
// =============================================================================

/// Locate the agent binary on disk.
///
/// Resolution order:
///
/// 1. The [`AGENT_BIN_ENV`] env var (absolute path).
/// 2. Sibling of the current executable, same directory, name
///    [`AGENT_BIN_NAME`]. This is the "installed alongside the CLI"
///    case (npm package, `cargo install`, release tarball).
/// 3. `PATH` walk — supports `cargo run`/development workflows where
///    `current_exe()` resolves to `target/debug/deps/...` and the
///    sibling check would miss the colocated binary in `target/debug`.
pub fn find_agent_binary() -> Result<PathBuf> {
    if let Ok(s) = std::env::var(AGENT_BIN_ENV)
        && !s.is_empty()
    {
        return Ok(PathBuf::from(s));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(AGENT_BIN_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
        #[cfg(windows)]
        {
            let with_ext = dir.join(format!("{AGENT_BIN_NAME}.exe"));
            if with_ext.is_file() {
                return Ok(with_ext);
            }
        }
    }
    if let Ok(path_value) = std::env::var("PATH") {
        for entry in std::env::split_paths(&path_value) {
            let candidate = entry.join(AGENT_BIN_NAME);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    bail!(
        "could not locate the `{AGENT_BIN_NAME}` binary; \
         set {AGENT_BIN_ENV} to its absolute path or install it alongside `devboy`"
    );
}

// =============================================================================
// Detached spawn
// =============================================================================

/// Spawn the agent as a detached background process.
///
/// `stdin`/`stdout`/`stderr` are nulled so the daemon's diagnostic
/// output does not bleed into the user's terminal. The CLI process
/// drops the returned `Child` immediately — the agent itself calls
/// `setsid()` inside its own `main` to detach from the controlling
/// terminal, so it survives the CLI exiting (or being interrupted
/// with `Ctrl-C`).
///
/// `vault_path` and `socket_path`, when supplied, are forwarded as
/// the [`VAULT_PATH_ENV`] and [`AGENT_SOCKET_ENV`] env vars
/// respectively. `None` lets the agent fall back to its own defaults.
pub fn spawn_detached_agent(
    binary: &Path,
    vault_path: Option<&Path>,
    socket_path: Option<&Path>,
) -> Result<()> {
    let mut cmd = Command::new(binary);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(p) = vault_path {
        cmd.env(VAULT_PATH_ENV, p);
    }
    if let Some(p) = socket_path {
        cmd.env(AGENT_SOCKET_ENV, p);
    }
    let _child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {AGENT_BIN_NAME} ({})", binary.display()))?;
    Ok(())
}

// =============================================================================
// Idempotent ensure-running
// =============================================================================

/// Idempotent: ensure a live agent is running on `socket_path`. If
/// the socket is already accepting connections, this is a no-op.
/// Otherwise: locate the binary, spawn it detached, and poll the
/// socket every [`SPAWN_POLL_INTERVAL`] until it accepts or
/// `timeout` elapses.
///
/// `vault_path` is forwarded to the spawned agent. Pass `None` to
/// use the agent's own default
/// (`<config_dir>/devboy-tools/secrets/vault.dvb`).
///
/// Returns an error if the binary cannot be located or if the
/// spawned daemon never starts accepting within `timeout`.
pub async fn ensure_agent_running(
    socket_path: &Path,
    vault_path: Option<&Path>,
    timeout: Duration,
) -> Result<()> {
    if is_socket_live(socket_path).await {
        return Ok(());
    }
    let binary = find_agent_binary()?;
    spawn_detached_agent(&binary, vault_path, Some(socket_path))?;

    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if is_socket_live(socket_path).await {
            return Ok(());
        }
        tokio::time::sleep(SPAWN_POLL_INTERVAL).await;
    }
    bail!(
        "spawned {AGENT_BIN_NAME} did not start accepting connections within {timeout:?}; \
         check the daemon's logs (run `{AGENT_BIN_NAME}` directly to see them)"
    )
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -- is_socket_live -------------------------------------------------

    #[tokio::test]
    async fn is_socket_live_returns_false_for_missing_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nope.sock");
        assert!(!is_socket_live(&path).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn is_socket_live_returns_false_for_stale_regular_file() {
        // Crashed-daemon scenario: a regular file at the socket path
        // would falsely pass a bare `path.exists()` check. Verify the
        // probe actually attempts a `connect()` and fails honestly.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.sock");
        std::fs::write(&path, b"stale-leftover").unwrap();
        assert!(!is_socket_live(&path).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn is_socket_live_returns_true_when_someone_listens() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.sock");
        let _listener = tokio::net::UnixListener::bind(&path).unwrap();
        assert!(is_socket_live(&path).await);
    }

    // -- find_agent_binary ----------------------------------------------

    #[test]
    fn find_agent_binary_honours_env_override() {
        // Even if the env points to a non-existent path, the override
        // is returned verbatim — verifying real existence is the
        // caller's job (it would happen at `Command::spawn` time).
        temp_env::with_var(AGENT_BIN_ENV, Some("/tmp/devboy-agent-test-binary"), || {
            let p = find_agent_binary().unwrap();
            assert_eq!(p, PathBuf::from("/tmp/devboy-agent-test-binary"));
        });
    }

    #[test]
    fn find_agent_binary_treats_empty_env_as_unset() {
        // Empty string in the env var must NOT be returned as an
        // absolute path — it would short-circuit to a useless ""
        // binary path. Empty == unset.
        let dir = TempDir::new().unwrap();
        temp_env::with_vars(
            [
                (AGENT_BIN_ENV, Some("")),
                ("PATH", Some(dir.path().to_str().unwrap())),
            ],
            || {
                // Without env override and without sibling/PATH match,
                // we expect a clean error rather than `Ok("")`.
                let r = find_agent_binary();
                if let Ok(p) = &r {
                    // The only way this could succeed is if the
                    // sibling lookup found a real binary next to the
                    // test runner — that's still a *valid* outcome.
                    // Just ensure it isn't the empty path.
                    assert_ne!(p, &PathBuf::new());
                }
            },
        );
    }

    // -- spawn_detached_agent -------------------------------------------

    #[cfg(unix)]
    #[test]
    fn spawn_detached_agent_propagates_binary_not_found() {
        // Use a path that definitely does not exist. The spawn must
        // surface a typed error rather than panicking or hanging.
        let nonexistent = std::path::Path::new("/nope/devboy-secrets-agent-does-not-exist");
        let err = spawn_detached_agent(nonexistent, None, None).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("failed to spawn devboy-secrets-agent"),
            "unexpected error message: {msg}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_detached_agent_succeeds_with_a_real_binary() {
        // Use `/bin/sh -c true` as a stand-in: it accepts the env vars
        // we set, exits immediately, and proves the spawn machinery
        // runs end-to-end without depending on the agent binary
        // existing in the test environment.
        let sh = std::path::Path::new("/bin/sh");
        if !sh.is_file() {
            // CI containers without /bin/sh — skip.
            return;
        }
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("agent.sock");
        let vault_path = dir.path().join("vault.dvb");

        // We can't pass `-c true` through this API (it takes a single
        // binary path with no positional args), so test the env-only
        // path: spawn /bin/sh and let it read stdin from /dev/null,
        // which makes it exit immediately.
        spawn_detached_agent(sh, Some(&vault_path), Some(&socket_path)).unwrap();
    }

    // -- ensure_agent_running -------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_agent_running_is_noop_when_socket_already_live() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.sock");
        let _listener = tokio::net::UnixListener::bind(&path).unwrap();
        // Even though the env points at a nonexistent binary,
        // `is_socket_live` returns true first so we never touch the
        // spawn path.
        temp_env::async_with_vars(
            [(AGENT_BIN_ENV, Some("/totally-nonexistent-path"))],
            async {
                let result = ensure_agent_running(&path, None, Duration::from_millis(100)).await;
                assert!(
                    result.is_ok(),
                    "expected idempotent no-op when socket is already live: {result:?}"
                );
            },
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_agent_running_errors_when_binary_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.sock");
        let empty_path_dir = TempDir::new().unwrap();
        // Remove the env override (empty == unset) AND make PATH
        // contain only an empty directory so neither lookup
        // mechanism finds the binary.
        temp_env::async_with_vars(
            [
                (AGENT_BIN_ENV, Some("")),
                ("PATH", Some(empty_path_dir.path().to_str().unwrap())),
            ],
            async {
                let result = ensure_agent_running(&path, None, Duration::from_millis(50)).await;
                // The sibling-of-current_exe lookup might still find
                // the agent if cargo has built it next to the test
                // binary — accept either a binary-missing error or a
                // socket-never-came-up error. The point is that we
                // do not panic.
                if let Err(e) = &result {
                    let msg = format!("{e:#}");
                    assert!(
                        msg.contains("could not locate")
                            || msg.contains("did not start accepting")
                            || msg.contains("failed to spawn"),
                        "unexpected error: {msg}"
                    );
                }
            },
        )
        .await;
    }
}
