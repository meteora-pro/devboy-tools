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
//!   vars listed in [`crate::plugin_manifest::PluginManifest::allowed_env_vars`]
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

use crate::source::Capabilities;

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

/// Which capability a call needs, or `None` for the two that are
/// part of the session rather than of the backend.
///
/// A free function so the mapping is testable on its own: it is
/// the rule the host enforces before spending a round trip, and a
/// wrong entry here would either block a working call or let an
/// unsupported one through.
pub fn required_capability(call: &PluginRequest) -> Option<Capabilities> {
    match call {
        PluginRequest::Init(_) | PluginRequest::IsAvailable => None,
        PluginRequest::Get(_) => Some(Capabilities::READ),
        PluginRequest::List => Some(Capabilities::LIST),
        PluginRequest::Validate(_) => Some(Capabilities::VALIDATE),
        PluginRequest::Put(_) => Some(Capabilities::WRITE),
        PluginRequest::Delete(_) => Some(Capabilities::DELETE),
        PluginRequest::KeyMaterial(_) => Some(Capabilities::KEY_SOURCE),
    }
}

/// Human name for the capability, for the refusal message.
fn capability_label(cap: Capabilities) -> &'static str {
    match cap {
        Capabilities::READ => "read",
        Capabilities::LIST => "list",
        Capabilities::VALIDATE => "validate",
        Capabilities::WRITE => "write",
        Capabilities::DELETE => "delete",
        Capabilities::KEY_SOURCE => "key_source",
        _ => "unknown",
    }
}

#[derive(Debug, Error)]
pub enum PluginClientError {
    #[error("failed to spawn plugin `{plugin}` at {path}: {source}")]
    Spawn {
        plugin: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "plugin `{plugin}` did not declare the `{capability}` capability at handshake, so this \
         call was not attempted"
    )]
    UnsupportedCapability { plugin: String, capability: String },
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
    /// What the plugin said it could do at handshake.
    ///
    /// Was thrown away before: `init` negotiated a capability
    /// bitset that nothing kept, so the host could not tell a
    /// backend that refuses writes from one that never claimed to
    /// support them until a call came back with an error.
    negotiated: Option<Capabilities>,
}

struct RunningProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// The diagnostic for a handshake answered with the wrong reply.
///
/// A named function rather than an inline `format!` because it
/// prints a plugin-controlled value into an error string that ends
/// up in logs, and that deserves somewhere to hang a test. The
/// safety now rests on [`GetResult`]'s redacting `Debug`; before it
/// had one, a plugin that answered `init` with a queued `get`
/// reply — the exact desync the id check upstream exists to catch —
/// wrote the user's secret here in plaintext.
fn init_reply_mismatch_detail(other: &PluginResponse) -> String {
    format!("expected an init result, got {other:?}")
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
                negotiated: None,
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

        // Spawn if needed. The handshake happens inside, so the
        // negotiated capabilities are known from here on.
        if state.process.is_none() {
            self.spawn_locked(&mut state).await?;
        }

        // Refuse before spending a round trip on a call the
        // plugin already said it cannot serve. The check lives
        // here rather than in per-method wrappers so a caller
        // that builds a `PluginRequest` by hand cannot slip past
        // it.
        if let Some(required) = required_capability(&call) {
            let declared = state.negotiated.unwrap_or_else(Capabilities::empty);
            if !declared.contains(required) {
                return Err(PluginClientError::UnsupportedCapability {
                    plugin: self.manifest.name.clone(),
                    capability: capability_label(required).to_owned(),
                });
            }
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
            RpcOutcome::Result(PluginResponse::Init(result)) => {
                state.negotiated = Some(Capabilities::from_bits_truncate(result.capabilities_bits));
            }
            RpcOutcome::Result(other) => {
                // A handshake that answers with something else is
                // not a working session, and pretending otherwise
                // only moves the failure to the first real call.
                self.record_crash_locked_msg(state, "init returned a non-init reply");
                return Err(PluginClientError::InitFailed {
                    plugin: self.manifest.name.clone(),
                    detail: init_reply_mismatch_detail(&other),
                });
            }
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

// Tests run a real subprocess plugin (a shell script). Shell
// scripts and `chmod +x` (`std::os::unix::fs::PermissionsExt`)
// are UNIX-only, so the test module is gated to UNIX targets.
// Tests exec a freshly-written shell script via tokio. Linux runners
// (especially cargo-llvm-cov + ubuntu-arm) sporadically surface
// ETXTBSY ("Text file busy") even after sync_all + a sync warmup
// exec, due to the kernel deferring text-segment release on those
// filesystems. macOS runs the same exec without the quirk, so gate
// the whole module to macOS — mirrors the same fix already in
// `crates/plugins/secrets/1password/src/lib.rs`.
/// The shell body of a fake plugin, without writing or running it.
///
/// Split out so the script can be checked on any platform: the
/// module that spawns these is macOS-only, so a malformed reply
/// template here — a doubled brace in a `format!`, say — is
/// invisible until a macOS runner reports a parse error, which is
/// exactly how one got in.
#[cfg(test)]
fn fake_plugin_script(dir: &std::path::Path, name: &str, behaviour: &str) -> String {
    match behaviour {
        "echo" => format!(
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  printf '{{"jsonrpc":"2.0","id":%s,"result":{{"source_name":"{name}","capabilities_bits":1,"plugin_version":"0.0.1"}}}}\n' "$id"
done
"#
        ),
        // Records every request line it is handed, so a test
        // can assert that a refused call never reached the
        // plugin at all.
        "log-calls" => format!(
            r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "{}/calls.txt"
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  printf '{{"jsonrpc":"2.0","id":%s,"result":{{"source_name":"{name}","capabilities_bits":1,"plugin_version":"0.0.1"}}}}\n' "$id"
done
"#,
            dir.display()
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
    }
}

/// Pure capability-mapping tests.
///
/// Deliberately outside the module below: that one is gated to
/// macOS because spawning a shell script out of a temp dir trips
/// a noexec quirk on the Linux runners. Nothing here spawns
/// anything, and a rule that decides whether a call is even
/// attempted should not be checked on one platform out of three.
#[cfg(test)]
mod capability_gate_tests {
    use super::*;
    use crate::plugin_protocol::PROTOCOL_VERSION;

    /// The fake plugins answer in JSON, and nothing on a Linux
    /// developer machine ever runs them — the module that spawns
    /// them is macOS-only. So check the text they would print.
    ///
    /// This exists because a `format!` with doubled braces
    /// produced `{{"jsonrpc"...` and the failure surfaced only as
    /// a parse error on a macOS runner, two pushes later.
    #[test]
    fn every_fake_plugin_emits_parseable_json() {
        let dir = std::path::Path::new("/tmp/whatever");

        for behaviour in ["echo", "log-calls", "env-dump"] {
            let script = fake_plugin_script(dir, "probe", behaviour);

            let line = script
                .lines()
                .find(|l| l.contains("jsonrpc"))
                .unwrap_or_else(|| panic!("{behaviour}: no reply line in the script"));

            // Lift the payload out of `printf '<payload>\n' "$id"`
            // and stand in for the shell's own substitution.
            let start = line.find('\'').expect("opening quote") + 1;
            let end = line.rfind("\\n'").expect("closing quote");
            let payload = line[start..end].replace("%s", "1");

            let parsed: serde_json::Value = serde_json::from_str(&payload)
                .unwrap_or_else(|e| panic!("{behaviour} emits invalid JSON: {e}\n{payload}"));

            assert_eq!(parsed["jsonrpc"], "2.0", "{behaviour}");
            assert_eq!(parsed["result"]["source_name"], "probe", "{behaviour}");
            assert!(
                parsed["result"]["capabilities_bits"].is_number(),
                "{behaviour}: the handshake has to carry a bitset"
            );
        }
    }

    /// Every method maps to exactly one capability, and the two
    /// session-level ones map to none. A wrong entry here either
    /// blocks a working call or lets an unsupported one through,
    /// and neither shows up until a plugin is in front of it.
    #[test]
    fn each_method_declares_the_capability_it_needs() {
        use crate::plugin_protocol::{
            DeleteParams, GetParams, InitParams, KeyMaterialParams, PutParams, ValidateParams,
        };

        let init = PluginRequest::Init(InitParams {
            source_name: "s".into(),
            config: Default::default(),
            protocol_version: PROTOCOL_VERSION.into(),
        });
        assert_eq!(required_capability(&init), None);
        assert_eq!(required_capability(&PluginRequest::IsAvailable), None);

        let cases = [
            (
                PluginRequest::Get(GetParams {
                    reference: "r".into(),
                }),
                Capabilities::READ,
            ),
            (PluginRequest::List, Capabilities::LIST),
            (
                PluginRequest::Validate(ValidateParams {
                    reference: "r".into(),
                }),
                Capabilities::VALIDATE,
            ),
            (
                PluginRequest::Put(PutParams {
                    reference: "r".into(),
                    value: "v".into(),
                }),
                Capabilities::WRITE,
            ),
            (
                PluginRequest::Delete(DeleteParams {
                    reference: "r".into(),
                }),
                Capabilities::DELETE,
            ),
            (
                PluginRequest::KeyMaterial(KeyMaterialParams {
                    purpose: "p".into(),
                }),
                Capabilities::KEY_SOURCE,
            ),
        ];
        for (call, want) in cases {
            assert_eq!(required_capability(&call), Some(want), "{call:?}");
        }
    }
}

/// Redaction guarantees that hold on every platform.
///
/// Deliberately its own module rather than an addition to the
/// macOS-only one below: nothing here spawns a process, and a
/// test that a secret never reaches a log string is worthless if
/// it only runs on the platform none of us develop on. It was
/// written into that module once, and neither the failure nor
/// the missing import showed up until a macOS runner picked the
/// job up days later.
#[cfg(test)]
mod redaction_tests {
    use super::*;
    use crate::plugin_protocol::GetResult;

    /// A plugin that answers the handshake with a `get` reply must
    /// not write the user's secret into the error string.
    ///
    /// This is not hypothetical plumbing: request/response desync is
    /// what the id check above this exists to catch, and a desynced
    /// plugin's queued `get` reply is precisely what lands in the
    /// wrong slot. The detail string goes to logs.
    #[test]
    fn a_wrong_handshake_reply_does_not_print_the_secret() {
        let detail = init_reply_mismatch_detail(&PluginResponse::Get(GetResult {
            value: "correct-horse-battery-staple".into(),
            lease_seconds: Some(300),
        }));

        assert!(
            !detail.contains("correct-horse-battery-staple"),
            "the plaintext reached an error string: {detail}"
        );
        assert!(
            detail.contains("expected an init result"),
            "the diagnostic still has to say what went wrong: {detail}"
        );
    }

    /// The guarantee at its source, so it holds for every `{:?}`
    /// site and not only the one that was found.
    #[test]
    fn get_result_never_prints_its_value() {
        let one = GetResult {
            value: "s3cret-alpha".into(),
            lease_seconds: None,
        };
        let rendered = format!("{one:?}");
        assert!(!rendered.contains("s3cret-alpha"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");

        // Nested in the response enum — how it actually travels.
        let nested = format!(
            "{:?}",
            PluginResponse::Get(GetResult {
                value: "s3cret-beta".into(),
                lease_seconds: Some(60),
            })
        );
        assert!(!nested.contains("s3cret-beta"), "{nested}");
        assert!(
            nested.contains("60"),
            "the lease is not a secret and stays useful: {nested}"
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
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
    /// The point of keeping the handshake result: a plugin that
    /// never claimed `write` must be refused **before** the host
    /// spends a round trip, and the refusal has to name the
    /// capability rather than surface as a generic backend error.
    #[tokio::test]
    async fn a_call_the_plugin_never_claimed_is_refused_without_reaching_it() {
        use crate::plugin_protocol::PutParams;

        let dir = tempfile::tempdir().unwrap();
        let (manifest, path) = write_fake_plugin(dir.path(), "logger", "log-calls");
        let client = PluginClient::new(manifest, path, LifetimePolicy::default());

        // A read is declared (bits = 1) and goes through.
        client
            .request(PluginRequest::IsAvailable)
            .await
            .expect("is_available is session-level");

        let err = client
            .request(PluginRequest::Put(PutParams {
                reference: "personal/github/token".into(),
                value: "ghp_never_sent".into(),
            }))
            .await
            .expect_err("write was never declared");

        match &err {
            PluginClientError::UnsupportedCapability { capability, .. } => {
                assert_eq!(capability, "write");
            }
            other => panic!("expected a capability refusal, got {other:?}"),
        }

        let calls = std::fs::read_to_string(dir.path().join("calls.txt")).unwrap_or_default();
        assert!(
            !calls.contains("ghp_never_sent"),
            "the value reached the plugin despite the refusal: {calls}"
        );
        assert!(
            !calls.contains("secret_source.put"),
            "the call reached the plugin despite the refusal: {calls}"
        );
    }

    fn write_fake_plugin(dir: &Path, name: &str, behaviour: &str) -> (PluginManifest, PathBuf) {
        let exec_path = dir.join(format!("devboy-source-{name}"));
        let script = fake_plugin_script(dir, name, behaviour);
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
