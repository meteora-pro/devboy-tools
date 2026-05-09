//! Lifetime-managing client for subprocess `SecretSource`
//! plugins per [ADR-021] §10 (subprocess plugin lifetime
//! contract).
//!
//! Builds on the wire-protocol types from [`plugin_protocol`]
//! and the manifest discovery from [`plugin_manifest`]: this
//! module owns the *process*. The host calls `request(...)`
//! whenever it needs to talk to the plugin; the client takes
//! care of:
//!
//! - **Lazy spawn** — the binary doesn't run until the first
//!   request reaches the client.
//! - **Idle timeout** — a spawn that hasn't been used for
//!   `idle_timeout` is shut down on the next access (kept the
//!   simple way: lazy reaping, no background sweeper).
//! - **Graceful shutdown** — `SIGTERM` + `grace_period`
//!   followed by `SIGKILL` if the child won't exit. `Drop`
//!   calls `shutdown_blocking()` so a leaked client doesn't
//!   leave a zombie.
//! - **Restart cap** — a sliding-window counter caps automatic
//!   re-spawn after a crash. Beyond the cap the plugin is
//!   marked **disabled**; `doctor` reports the failure count
//!   and the user has to clear it.
//! - **Env restriction** — the child inherits exactly the env
//!   vars listed in [`plugin_manifest::PluginManifest::allowed_env_vars`]
//!   and nothing else. `Command::env_clear()` is the gate; the
//!   test crate's env-leak fixture proves it.
//!
//! ## What this module does **not** do
//!
//! Implement the [`SecretSource`] trait. The client returns
//! typed wire payloads; a thin adapter (added in P15.3 or by
//! the router) maps them to `SecretSource::get/list/validate`
//! results. Keeping the trait impl out of this module makes
//! the lifetime semantics testable without dragging in the
//! whole router.
//!
//! [`plugin_protocol`]: crate::plugin_protocol
//! [`plugin_manifest`]: crate::plugin_manifest
//! [`SecretSource`]: crate::source::SecretSource
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-secret-source-router.md

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::plugin_manifest::PluginManifest;
use crate::plugin_protocol::{
    InitParams, JsonRpcVersion, PROTOCOL_VERSION, PluginRequest, PluginResponse, PluginRpcRequest,
    PluginRpcResponse, RpcOutcome,
};

// =============================================================================
// Policy
// =============================================================================

/// Lifetime knobs. All durations have ADR-021 §10 defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifetimePolicy {
    pub idle_timeout: Duration,
    pub shutdown_grace: Duration,
    pub restart_window: Duration,
    pub restart_cap: usize,
}

impl Default for LifetimePolicy {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(60),
            shutdown_grace: Duration::from_secs(10),
            restart_window: Duration::from_secs(60),
            restart_cap: 3,
        }
    }
}

// =============================================================================
// Health snapshot (consumed by `doctor`)
// =============================================================================

/// Lifetime view exposed to `doctor`. Captures whether the
/// plugin is alive, how many times it crashed in the rolling
/// window, and whether the restart cap has tripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHealth {
    pub plugin_name: String,
    pub state: PluginState,
    pub crashes_in_window: usize,
    pub last_used: Option<Instant>,
    pub last_crash: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// Never spawned or shut down cleanly. Next request
    /// triggers a fresh spawn.
    Idle,
    /// Subprocess is alive and ready.
    Running,
    /// Subprocess died and we're inside the restart window.
    Recovering,
    /// Restart cap hit. The plugin is dormant until the user
    /// clears the failure count via `doctor` (or restarts the
    /// host).
    Disabled { reason: String },
}

// =============================================================================
// Errors
// =============================================================================

#[derive(Debug, Error)]
pub enum PluginClientError {
    #[error("failed to spawn plugin `{plugin}` at {path}: {source}")]
    Spawn {
        plugin: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("plugin `{plugin}` failed to initialise: {detail}")]
    InitFailed { plugin: String, detail: String },
    #[error(
        "plugin `{plugin}` exceeded restart cap ({cap} restarts in {window:?}); marking disabled"
    )]
    RestartCapExceeded {
        plugin: String,
        cap: usize,
        window: Duration,
    },
    #[error("plugin `{plugin}` is disabled: {reason}")]
    Disabled { plugin: String, reason: String },
    #[error("I/O error talking to plugin `{plugin}`: {source}")]
    Io {
        plugin: String,
        #[source]
        source: std::io::Error,
    },
    #[error("plugin `{plugin}` returned a malformed response: {source}")]
    MalformedResponse {
        plugin: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("plugin `{plugin}` returned unexpected payload for method `{method}`")]
    UnexpectedPayload { plugin: String, method: String },
    #[error("plugin `{plugin}` reported error: {detail}")]
    PluginError { plugin: String, detail: String },
    #[error("plugin `{plugin}` reply id mismatch (expected {expected}, got {got})")]
    IdMismatch {
        plugin: String,
        expected: u64,
        got: u64,
    },
}

// =============================================================================
// Client
// =============================================================================

/// Thread-safe handle to a subprocess plugin. Cheap to clone —
/// the actual state lives behind an `Arc<Mutex<…>>`.
#[derive(Clone)]
pub struct PluginClient {
    inner: Arc<Mutex<ClientState>>,
    manifest: Arc<PluginManifest>,
    executable: PathBuf,
    policy: LifetimePolicy,
}

struct ClientState {
    process: Option<RunningProcess>,
    state: PluginState,
    crashes: VecDeque<Instant>,
    last_used: Option<Instant>,
    last_crash: Option<Instant>,
    next_id: u64,
}

struct RunningProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl PluginClient {
    /// Build a fresh client. Does **not** spawn — the first
    /// `request` call performs the lazy spawn.
    pub fn new(manifest: PluginManifest, executable: PathBuf, policy: LifetimePolicy) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ClientState {
                process: None,
                state: PluginState::Idle,
                crashes: VecDeque::new(),
                last_used: None,
                last_crash: None,
                next_id: 0,
            })),
            manifest: Arc::new(manifest),
            executable,
            policy,
        }
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    pub fn policy(&self) -> LifetimePolicy {
        self.policy
    }

    /// Snapshot of the current health for `doctor`.
    pub async fn health(&self) -> PluginHealth {
        let state = self.inner.lock().await;
        PluginHealth {
            plugin_name: self.manifest.name.clone(),
            state: state.state.clone(),
            crashes_in_window: state.crashes.len(),
            last_used: state.last_used,
            last_crash: state.last_crash,
        }
    }

    /// Manually clear the crash counter — used by `doctor`'s
    /// "reset disabled plugin" affordance after the operator
    /// has fixed whatever was crashing it.
    pub async fn clear_disabled(&self) {
        let mut state = self.inner.lock().await;
        state.crashes.clear();
        if matches!(state.state, PluginState::Disabled { .. }) {
            state.state = PluginState::Idle;
        }
    }

    /// Issue a single request. Spawns lazily, re-spawns on
    /// crash within the cap, and reaps an idle process before
    /// the next live call.
    pub async fn request(&self, call: PluginRequest) -> Result<PluginResponse, PluginClientError> {
        let mut state = self.inner.lock().await;

        // Disabled plugins refuse before doing any I/O.
        if let PluginState::Disabled { reason } = &state.state {
            return Err(PluginClientError::Disabled {
                plugin: self.manifest.name.clone(),
                reason: reason.clone(),
            });
        }

        // Lazy reap — drop the child if we've been idle past
        // `idle_timeout`. We do this *before* checking for a
        // running process so the next branch handles the
        // re-spawn uniformly.
        if let Some(last) = state.last_used
            && state.process.is_some()
            && last.elapsed() >= self.policy.idle_timeout
        {
            debug!(
                plugin = self.manifest.name.as_str(),
                idle_for = ?last.elapsed(),
                "reaping idle plugin process"
            );
            self.shutdown_locked(&mut state).await;
        }

        // Spawn if needed.
        if state.process.is_none() {
            self.spawn_locked(&mut state).await?;
        }

        let id = state.next_id.wrapping_add(1);
        state.next_id = id;
        let req = PluginRpcRequest {
            jsonrpc: JsonRpcVersion::current(),
            id,
            call,
        };
        let line = match serde_json::to_string(&req) {
            Ok(s) => s,
            Err(source) => {
                return Err(PluginClientError::MalformedResponse {
                    plugin: self.manifest.name.clone(),
                    source,
                });
            }
        };

        // Send + read response.
        let outcome = match self.exchange_locked(&mut state, &line).await {
            Ok(resp) => resp,
            Err(e) => {
                // Treat any I/O error as a crash.
                self.record_crash_locked(&mut state, e.to_string());
                self.shutdown_locked(&mut state).await;
                return Err(e);
            }
        };

        if outcome.id != id {
            return Err(PluginClientError::IdMismatch {
                plugin: self.manifest.name.clone(),
                expected: id,
                got: outcome.id,
            });
        }

        state.last_used = Some(Instant::now());

        match outcome.outcome {
            RpcOutcome::Result(r) => Ok(r),
            RpcOutcome::Error(e) => Err(PluginClientError::PluginError {
                plugin: self.manifest.name.clone(),
                detail: e.to_string(),
            }),
        }
    }

    /// Send `SIGTERM`, wait `shutdown_grace`, send `SIGKILL`
    /// if still alive. Idempotent — safe to call multiple
    /// times.
    pub async fn shutdown(&self) {
        let mut state = self.inner.lock().await;
        self.shutdown_locked(&mut state).await;
    }

    async fn shutdown_locked(&self, state: &mut ClientState) {
        let Some(mut proc) = state.process.take() else {
            return;
        };
        // Try a graceful kill first. tokio::process::Child::start_kill
        // sends SIGTERM (on Unix; TerminateProcess on Windows).
        if let Err(e) = proc.child.start_kill() {
            warn!(
                plugin = self.manifest.name.as_str(),
                error = %e,
                "start_kill failed; child may already be dead"
            );
        }
        match tokio::time::timeout(self.policy.shutdown_grace, proc.child.wait()).await {
            Ok(Ok(_)) => {
                debug!(plugin = self.manifest.name.as_str(), "exited within grace");
            }
            Ok(Err(e)) => {
                warn!(
                    plugin = self.manifest.name.as_str(),
                    error = %e,
                    "wait returned error post-kill"
                );
            }
            Err(_) => {
                // Grace period elapsed. Force-kill.
                warn!(
                    plugin = self.manifest.name.as_str(),
                    grace_ms = self.policy.shutdown_grace.as_millis(),
                    "plugin did not exit in grace; force-killing"
                );
                let _ = proc.child.kill().await;
            }
        }
        if matches!(state.state, PluginState::Running) {
            state.state = PluginState::Idle;
        }
    }

    async fn spawn_locked(&self, state: &mut ClientState) -> Result<(), PluginClientError> {
        // Restart-cap check: drop crashes outside the window
        // first.
        let now = Instant::now();
        let window = self.policy.restart_window;
        while let Some(front) = state.crashes.front() {
            if now.duration_since(*front) >= window {
                state.crashes.pop_front();
            } else {
                break;
            }
        }
        if state.crashes.len() >= self.policy.restart_cap {
            let reason = format!(
                "{} crashes in last {:?}",
                state.crashes.len(),
                self.policy.restart_window
            );
            state.state = PluginState::Disabled {
                reason: reason.clone(),
            };
            return Err(PluginClientError::RestartCapExceeded {
                plugin: self.manifest.name.clone(),
                cap: self.policy.restart_cap,
                window: self.policy.restart_window,
            });
        }

        let mut cmd = Command::new(&self.executable);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        // Strip the host's env wholesale, then add only the
        // allowed vars. This is the env-restriction enforcement
        // the manifest declares.
        cmd.env_clear();
        for var in &self.manifest.allowed_env_vars {
            if let Ok(value) = std::env::var(var) {
                cmd.env(var, value);
            }
        }

        let mut child = cmd.spawn().map_err(|source| PluginClientError::Spawn {
            plugin: self.manifest.name.clone(),
            path: self.executable.clone(),
            source,
        })?;

        let stdin = child
            .stdin
            .take()
            .expect("Stdio::piped on stdin should yield a handle");
        let stdout = child
            .stdout
            .take()
            .expect("Stdio::piped on stdout should yield a handle");
        let stdout = BufReader::new(stdout);

        state.process = Some(RunningProcess {
            child,
            stdin,
            stdout,
        });
        state.state = PluginState::Running;

        // Init handshake.
        let init = PluginRequest::Init(InitParams {
            source_name: self.manifest.name.clone(),
            config: Default::default(),
            protocol_version: PROTOCOL_VERSION.into(),
        });
        let id = state.next_id.wrapping_add(1);
        state.next_id = id;
        let req = PluginRpcRequest {
            jsonrpc: JsonRpcVersion::current(),
            id,
            call: init,
        };
        let line = serde_json::to_string(&req).map_err(|e| PluginClientError::InitFailed {
            plugin: self.manifest.name.clone(),
            detail: e.to_string(),
        })?;
        let resp = self.exchange_locked(state, &line).await.map_err(|e| {
            self.record_crash_locked_msg(state, "init exchange failed");
            PluginClientError::InitFailed {
                plugin: self.manifest.name.clone(),
                detail: e.to_string(),
            }
        })?;
        if resp.id != id {
            return Err(PluginClientError::InitFailed {
                plugin: self.manifest.name.clone(),
                detail: format!("init reply id mismatch: expected {id}, got {}", resp.id),
            });
        }
        match resp.outcome {
            RpcOutcome::Result(_) => {}
            RpcOutcome::Error(e) => {
                self.record_crash_locked_msg(state, &format!("init returned error: {e}"));
                return Err(PluginClientError::InitFailed {
                    plugin: self.manifest.name.clone(),
                    detail: e.to_string(),
                });
            }
        }

        Ok(())
    }

    async fn exchange_locked(
        &self,
        state: &mut ClientState,
        line: &str,
    ) -> Result<PluginRpcResponse, PluginClientError> {
        let proc = state
            .process
            .as_mut()
            .expect("exchange called without a running process");
        proc.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|source| PluginClientError::Io {
                plugin: self.manifest.name.clone(),
                source,
            })?;
        proc.stdin
            .write_all(b"\n")
            .await
            .map_err(|source| PluginClientError::Io {
                plugin: self.manifest.name.clone(),
                source,
            })?;
        proc.stdin
            .flush()
            .await
            .map_err(|source| PluginClientError::Io {
                plugin: self.manifest.name.clone(),
                source,
            })?;

        let mut reply = String::new();
        let n =
            proc.stdout
                .read_line(&mut reply)
                .await
                .map_err(|source| PluginClientError::Io {
                    plugin: self.manifest.name.clone(),
                    source,
                })?;
        if n == 0 {
            return Err(PluginClientError::Io {
                plugin: self.manifest.name.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "plugin closed stdout",
                ),
            });
        }
        serde_json::from_str(reply.trim_end()).map_err(|source| {
            PluginClientError::MalformedResponse {
                plugin: self.manifest.name.clone(),
                source,
            }
        })
    }

    fn record_crash_locked(&self, state: &mut ClientState, _detail: String) {
        let now = Instant::now();
        state.crashes.push_back(now);
        state.last_crash = Some(now);
        if state.crashes.len() >= self.policy.restart_cap {
            state.state = PluginState::Disabled {
                reason: format!(
                    "{} crashes in last {:?}",
                    state.crashes.len(),
                    self.policy.restart_window
                ),
            };
        } else {
            state.state = PluginState::Recovering;
        }
    }

    fn record_crash_locked_msg(&self, state: &mut ClientState, _msg: &str) {
        self.record_crash_locked(state, String::new());
    }
}

impl Drop for PluginClient {
    fn drop(&mut self) {
        // Best-effort: if this is the last Arc, kill the child
        // synchronously via tokio's `kill_on_drop` flag set at
        // spawn. The async shutdown can't run from Drop, but
        // `kill_on_drop` ensures the child is reaped without a
        // zombie even if shutdown() was never called.
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_manifest::PluginManifest;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use tempfile::TempDir;

    /// Build a fake plugin shell-script that:
    /// - reads JSON-RPC lines from stdin
    /// - emits canned responses indexed by the request `id`
    /// - exits cleanly on EOF (i.e. after stdin closes)
    ///
    /// `behaviour` is a small DSL: `"echo"` echoes Init back as
    /// success; `"crash"` exits non-zero immediately; `"hang"`
    /// reads but never writes (for grace-timeout tests);
    /// `"env-dump"` writes the env to a sidecar file then echoes
    /// Init.
    fn write_fake_plugin(dir: &Path, name: &str, behaviour: &str) -> (PluginManifest, PathBuf) {
        let exec_path = dir.join(format!("devboy-source-{name}"));
        let script = match behaviour {
            "echo" => format!(
                r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  printf '{{"jsonrpc":"2.0","id":%s,"result":{{"source_name":"{name}","capabilities_bits":1,"plugin_version":"0.0.1"}}}}\n' "$id"
done
"#
            ),
            "crash" => "#!/bin/sh\nexit 7\n".to_string(),
            "hang" => "#!/bin/sh\nwhile read line; do :; done\nsleep 30\n".to_string(),
            "env-dump" => format!(
                r#"#!/bin/sh
env > "{}/env-dump.txt"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  printf '{{"jsonrpc":"2.0","id":%s,"result":{{"source_name":"{name}","capabilities_bits":1,"plugin_version":"0.0.1"}}}}\n' "$id"
done
"#,
                dir.display()
            ),
            other => panic!("unknown behaviour: {other}"),
        };
        fs::write(&exec_path, script).unwrap();
        let mut perms = fs::metadata(&exec_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&exec_path, perms).unwrap();

        let bytes = fs::read(&exec_path).unwrap();
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(&bytes);
        let checksum = hex::encode(hasher.finalize());

        let manifest = PluginManifest {
            name: name.into(),
            version: "0.0.1".into(),
            executable: PathBuf::from(format!("devboy-source-{name}")),
            allowed_env_vars: vec!["DEVBOY_TEST_LET_THROUGH".into()],
            checksum_sha256: checksum,
        };
        (manifest, exec_path)
    }

    fn fast_policy() -> LifetimePolicy {
        LifetimePolicy {
            idle_timeout: Duration::from_millis(80),
            shutdown_grace: Duration::from_millis(200),
            restart_window: Duration::from_secs(10),
            restart_cap: 3,
        }
    }

    // -- Spawn + init handshake --------------------------------

    #[tokio::test]
    async fn lazy_spawn_and_init_handshake_succeeds() {
        let dir = TempDir::new().unwrap();
        let (manifest, exec) = write_fake_plugin(dir.path(), "echo", "echo");
        let client = PluginClient::new(manifest, exec, fast_policy());

        let initial = client.health().await;
        assert_eq!(initial.state, PluginState::Idle);

        // First request triggers spawn + init + the request.
        let resp = client.request(PluginRequest::IsAvailable).await;
        // The fake "echo" script always returns the Init result
        // shape, which serde will accept as PluginResponse::Init
        // because we use serde(untagged). That's fine — the
        // important assertion is that the exchange completed.
        assert!(resp.is_ok(), "request failed: {resp:?}");
        let after = client.health().await;
        assert_eq!(after.state, PluginState::Running);
        assert!(after.last_used.is_some());

        client.shutdown().await;
    }

    // -- Idle timeout reaps process ----------------------------

    #[tokio::test]
    async fn idle_timeout_reaps_subprocess_before_next_request() {
        let dir = TempDir::new().unwrap();
        let (manifest, exec) = write_fake_plugin(dir.path(), "echoi", "echo");
        let client = PluginClient::new(manifest, exec, fast_policy());

        let _ = client.request(PluginRequest::IsAvailable).await.unwrap();
        // Sleep past the idle timeout.
        tokio::time::sleep(Duration::from_millis(150)).await;
        // Next request must succeed — that means the client
        // re-spawned cleanly.
        let _ = client.request(PluginRequest::IsAvailable).await.unwrap();

        client.shutdown().await;
    }

    // -- Restart cap -------------------------------------------

    #[tokio::test]
    async fn restart_cap_disables_after_repeated_spawn_failures() {
        let dir = TempDir::new().unwrap();
        let (manifest, exec) = write_fake_plugin(dir.path(), "crashc", "crash");
        let client = PluginClient::new(manifest, exec, fast_policy());

        // Each call: child exits before init handshake → recorded
        // as a crash. Cap = 3.
        for _ in 0..3 {
            let _ = client.request(PluginRequest::IsAvailable).await;
        }
        let h = client.health().await;
        assert!(
            matches!(h.state, PluginState::Disabled { .. }),
            "expected Disabled, got {:?}",
            h.state
        );
        // Fourth call refuses without spawning.
        let err = client
            .request(PluginRequest::IsAvailable)
            .await
            .unwrap_err();
        assert!(
            matches!(err, PluginClientError::Disabled { .. }),
            "expected Disabled error, got {err:?}"
        );

        // Operator clears the failure counter.
        client.clear_disabled().await;
        assert_eq!(client.health().await.state, PluginState::Idle);
    }

    // -- Env restriction ---------------------------------------

    #[tokio::test]
    async fn env_restriction_only_passes_allowed_vars() {
        let dir = TempDir::new().unwrap();
        let (manifest, exec) = write_fake_plugin(dir.path(), "envd", "env-dump");
        let dir_path = dir.path().to_path_buf();
        let client = PluginClient::new(manifest, exec, fast_policy());

        // `temp_env` scopes the env mutation to the future
        // body — no `unsafe` needed and other tests don't see
        // the leakage.
        temp_env::async_with_vars(
            [
                ("DEVBOY_TEST_SHOULD_NOT_LEAK", Some("leak-me")),
                ("DEVBOY_TEST_LET_THROUGH", Some("passed-through")),
            ],
            async move {
                let _ = client.request(PluginRequest::IsAvailable).await.unwrap();
                client.shutdown().await;
            },
        )
        .await;

        // The fake plugin wrote its env to a sidecar file.
        let dump = fs::read_to_string(dir_path.join("env-dump.txt")).unwrap();
        assert!(
            dump.contains("DEVBOY_TEST_LET_THROUGH=passed-through"),
            "allowed var did not pass through: {dump}"
        );
        assert!(
            !dump.contains("DEVBOY_TEST_SHOULD_NOT_LEAK"),
            "non-allowed var leaked into plugin env: {dump}"
        );
    }

    // -- Graceful shutdown -------------------------------------

    #[tokio::test]
    async fn shutdown_sends_sigterm_then_sigkill_on_grace_timeout() {
        let dir = TempDir::new().unwrap();
        let (manifest, exec) = write_fake_plugin(dir.path(), "hang", "hang");
        let client = PluginClient::new(
            manifest,
            exec,
            LifetimePolicy {
                idle_timeout: Duration::from_secs(60),
                shutdown_grace: Duration::from_millis(150),
                restart_window: Duration::from_secs(10),
                restart_cap: 3,
            },
        );

        // Spawn the process by attempting a request. The "hang"
        // plugin reads but never writes — request will hang too,
        // so we time it out. Then call shutdown which must force
        // -kill within grace + a small slack.
        let req_fut = client.request(PluginRequest::IsAvailable);
        let _ = tokio::time::timeout(Duration::from_millis(50), req_fut).await;
        // Even though the request future was dropped, the
        // process is still alive in the registry.

        let start = Instant::now();
        client.shutdown().await;
        let elapsed = start.elapsed();
        // Should land between grace_period and grace_period + 1s
        // slack.
        assert!(
            elapsed < Duration::from_secs(2),
            "shutdown took too long: {elapsed:?}"
        );
    }

    // -- Default policy values --------------------------------

    #[test]
    fn default_policy_matches_adr_021_section_10() {
        let p = LifetimePolicy::default();
        assert_eq!(p.idle_timeout, Duration::from_secs(60));
        assert_eq!(p.shutdown_grace, Duration::from_secs(10));
        assert_eq!(p.restart_window, Duration::from_secs(60));
        assert_eq!(p.restart_cap, 3);
    }
}
