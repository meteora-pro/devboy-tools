//! `devboy-secret-local-vault` — built-in `SecretSource` for the
//! encrypted local-vault per [ADR-021] §8 + [ADR-023] §3.
//!
//! Where `keychain` talks directly to the OS, `local-vault` talks
//! to the [`devboy-secrets-agent`] daemon over a UNIX socket using
//! the JSON-RPC 2.0 wire protocol from ADR-023 §3.3. The daemon
//! owns the unlocked vault state; this source is just a thin
//! request/response client.
//!
//! # Why a daemon?
//!
//! Per ADR-023 §3.3, the unlock window has to live somewhere. If
//! every CLI invocation re-derived the vault key from the
//! passphrase + Argon2id, every `secret.get` would cost the user
//! ~hundred milliseconds of KDF time. The daemon caches the
//! unlocked key (under `secrecy::SecretBox` zeroize-on-drop) and
//! re-locks itself after a configurable idle window.
//!
//! # Capabilities
//!
//! Per ADR-021 §8: `READ | LIST | VALIDATE | WRITE | ROTATE |
//! BIOMETRIC_PROMPT`. The biometric flag is always set because at
//! least one operation can prompt for user presence — write/rotate
//! always require a fresh `fresh_unlock` regardless of the daemon's
//! cached state, and reads can prompt when the unlock window has
//! expired.
//!
//! # Reference shape
//!
//! `reference` is the ADR-020 path itself. Like the keychain
//! source, the local-vault treats the path as the backend key with
//! no transformation.
//!
//! # Connection model
//!
//! One TCP-style connection per RPC call. The daemon's accept loop
//! handles short-lived connections cheaply; the cache layer (P5.4)
//! keeps the call frequency low for warm reads. Persistent
//! connections / connection pooling can land later if profiling
//! shows it matters.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md
//! [`devboy-secrets-agent`]: https://docs.rs/devboy-secrets-agent

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use devboy_secrets_agent::{
    ENTRY_NOT_FOUND, JsonRpcResponse, VAULT_LOCKED, default_socket_path,
};
#[cfg(unix)]
use devboy_secrets_agent::{
    FramingError, JSONRPC_VERSION, JsonRpcRequest, read_response, write_request,
};
use devboy_storage::{
    Capabilities, GetOutcome, RemoteRef, SecretSource, SourceError, SourceStatus,
};
use secrecy::SecretString;
use serde_json::{Value, json};
#[cfg(unix)]
use tokio::io::BufReader;
// `tokio::net::UnixStream` is unavailable on Windows. The
// daemon protocol is UNIX-domain-socket only by design (per
// ADR-023 §3.3 — the socket is `<config_dir>/devboy-tools/
// secrets/agent.sock`). On Windows this source compiles into a
// stub whose `rpc_call` returns `SourceError::Upstream`; the
// rest of the workspace still depends on this crate so the type
// must exist on every platform.
#[cfg(unix)]
use tokio::net::UnixStream;

/// `SecretSource` backed by the encrypted local-vault daemon.
#[derive(Debug, Clone)]
pub struct LocalVaultSource {
    /// Logical name from `[[source]] name = "..."` in `sources.toml`.
    name: String,
    /// On-disk path to the agent socket the daemon binds to.
    socket_path: PathBuf,
}

impl LocalVaultSource {
    /// Build a source named `name` pointing at the canonical agent
    /// socket (`<config_dir>/devboy-tools/secrets/agent.sock`).
    pub fn new(name: impl Into<String>) -> Result<Self, devboy_secrets_agent::AgentError> {
        let socket_path = default_socket_path()?;
        Ok(Self {
            name: name.into(),
            socket_path,
        })
    }

    /// Build a source with an explicit socket path. Used by
    /// integration tests that spin up an in-process daemon at a
    /// `tempfile`-managed location.
    pub fn with_socket_path(name: impl Into<String>, socket_path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            socket_path: socket_path.into(),
        }
    }

    /// Borrow the socket path the source is configured against.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Send one request, await one response. New connection per
    /// call. UNIX-only — see the `tokio::net::UnixStream` import
    /// note above.
    #[cfg(unix)]
    async fn rpc_call(&self, method: &str, params: Value) -> Result<JsonRpcResponse, SourceError> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| SourceError::Io {
                name: self.name.clone(),
                source: e,
            })?;
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = BufReader::new(read);
        let request = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id: json!(1),
            method: method.to_owned(),
            params,
        };
        write_request(&mut write, &request)
            .await
            .map_err(|e| framing_to_source(self, e))?;
        // Half-close the write side so the daemon's read loop can
        // see EOF after the single request and won't block waiting
        // for more bytes when our write_response side stays alive.
        drop(write);
        read_response(&mut reader)
            .await
            .map_err(|e| framing_to_source(self, e))
    }

    /// Windows stub — UNIX domain sockets do not exist here.
    /// The whole crate compiles into the same shape as on UNIX
    /// so consumers need no `#[cfg]` at the call site, but every
    /// RPC short-circuits to `Unsupported`.
    #[cfg(not(unix))]
    async fn rpc_call(
        &self,
        _method: &str,
        _params: Value,
    ) -> Result<JsonRpcResponse, SourceError> {
        Err(SourceError::Upstream {
            name: self.name.clone(),
            message: "local-vault source requires UNIX domain sockets — \
                      not available on this platform"
                .to_owned(),
        })
    }
}

#[cfg(unix)]
fn framing_to_source(src: &LocalVaultSource, e: FramingError) -> SourceError {
    match e {
        FramingError::Io(io) => SourceError::Io {
            name: src.name.clone(),
            source: io,
        },
        other => SourceError::Upstream {
            name: src.name.clone(),
            message: other.to_string(),
        },
    }
}

#[async_trait]
impl SecretSource for LocalVaultSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ
            | Capabilities::LIST
            | Capabilities::VALIDATE
            | Capabilities::WRITE
            | Capabilities::ROTATE
            | Capabilities::BIOMETRIC_PROMPT
    }

    async fn is_available(&self) -> SourceStatus {
        // Connection failure → daemon isn't running → treat as
        // NotInstalled so the router falls back per ADR-021 §2 step 3.
        let resp = match self.rpc_call("vault.status", Value::Null).await {
            Ok(r) => r,
            Err(SourceError::Io { .. }) => return SourceStatus::NotInstalled,
            Err(e) => return SourceStatus::Error(e.to_string()),
        };
        if let Some(err) = resp.error {
            return SourceStatus::Error(err.message);
        }
        let result = match resp.result {
            Some(v) => v,
            None => return SourceStatus::Error("vault.status returned no result".to_owned()),
        };
        match result.get("state").and_then(Value::as_str) {
            Some("unlocked") => SourceStatus::Available,
            Some("locked") => SourceStatus::Locked,
            other => {
                SourceStatus::Error(format!("vault.status returned unexpected state {other:?}"))
            }
        }
    }

    async fn get(&self, reference: &str) -> Result<Option<GetOutcome>, SourceError> {
        if reference.is_empty() {
            return Err(SourceError::BadReference {
                name: self.name.clone(),
                reference: reference.to_owned(),
                reason: "reference must be a non-empty path".to_owned(),
            });
        }
        let resp = self
            .rpc_call("secret.get", json!({"path": reference}))
            .await?;
        if let Some(err) = resp.error {
            return Err(map_rpc_error(self, err));
        }
        let result = resp.result.ok_or_else(|| SourceError::Upstream {
            name: self.name.clone(),
            message: "secret.get returned neither result nor error".to_owned(),
        })?;
        let value =
            result
                .get("value")
                .and_then(Value::as_str)
                .ok_or_else(|| SourceError::Upstream {
                    name: self.name.clone(),
                    message: "secret.get result missing 'value' field".to_owned(),
                })?;
        Ok(Some(GetOutcome {
            value: SecretString::from(value.to_owned()),
            // The daemon does not surface lease info; the local
            // vault has no leases of its own.
            lease_duration: None,
        }))
    }

    async fn list(&self) -> Result<Vec<RemoteRef>, SourceError> {
        let resp = self.rpc_call("secret.list", Value::Null).await?;
        if let Some(err) = resp.error {
            return Err(map_rpc_error(self, err));
        }
        let result = resp.result.ok_or_else(|| SourceError::Upstream {
            name: self.name.clone(),
            message: "secret.list returned neither result nor error".to_owned(),
        })?;
        let entries = result
            .get("entries")
            .and_then(Value::as_array)
            .ok_or_else(|| SourceError::Upstream {
                name: self.name.clone(),
                message: "secret.list result missing 'entries' array".to_owned(),
            })?;
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let path =
                entry
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| SourceError::Upstream {
                        name: self.name.clone(),
                        message: "secret.list entry missing 'path'".to_owned(),
                    })?;
            let display = entry
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned);
            out.push(RemoteRef {
                reference: path.to_owned(),
                display,
            });
        }
        Ok(out)
    }

    async fn validate(&self, reference: &str) -> Result<(), SourceError> {
        if reference.is_empty() {
            return Err(SourceError::BadReference {
                name: self.name.clone(),
                reference: reference.to_owned(),
                reason: "reference must be a non-empty path".to_owned(),
            });
        }
        Ok(())
    }
}

/// Map the daemon's JSON-RPC error envelope onto a typed
/// `SourceError`. `ENTRY_NOT_FOUND` (-32007) is conventionally a
/// "no such entry" rather than an upstream failure — but our trait
/// signals that via `Ok(None)`, not via Err. Callers should already
/// have inspected `result` first and only invoke this helper when
/// `error` is set. We map ENTRY_NOT_FOUND through `Upstream` here
/// so the daemon's specific error code propagates verbatim into
/// the message.
fn map_rpc_error(src: &LocalVaultSource, err: devboy_secrets_agent::JsonRpcError) -> SourceError {
    match err.code {
        VAULT_LOCKED => SourceError::Locked {
            name: src.name.clone(),
        },
        ENTRY_NOT_FOUND => SourceError::Upstream {
            name: src.name.clone(),
            message: err.message,
        },
        _ => SourceError::Upstream {
            name: src.name.clone(),
            message: format!("daemon error {}: {}", err.code, err.message),
        },
    }
}

// =============================================================================
// Tests
// =============================================================================

// Tests spin up a real `AgentListener` over a UNIX domain
// socket, so the module is gated to UNIX targets — the daemon
// protocol itself is UNIX-only by design (see header note).
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    // -- Pure-data trait surface --------------------------------------

    #[test]
    fn name_returns_constructor_value() {
        let s = LocalVaultSource::with_socket_path("local-vault", "/tmp/agent.sock");
        assert_eq!(s.name(), "local-vault");
    }

    #[test]
    fn socket_path_returns_constructor_value() {
        let s = LocalVaultSource::with_socket_path("local-vault", "/tmp/agent.sock");
        assert_eq!(s.socket_path(), Path::new("/tmp/agent.sock"));
    }

    #[test]
    fn capabilities_match_adr_021_section_8_local_vault() {
        let s = LocalVaultSource::with_socket_path("local-vault", "/tmp/agent.sock");
        let caps = s.capabilities();
        assert!(caps.contains(Capabilities::READ));
        assert!(caps.contains(Capabilities::LIST));
        assert!(caps.contains(Capabilities::VALIDATE));
        assert!(caps.contains(Capabilities::WRITE));
        assert!(caps.contains(Capabilities::ROTATE));
        assert!(caps.contains(Capabilities::BIOMETRIC_PROMPT));
        // local-vault is local; reads aren't logged anywhere
        // observable.
        assert!(!caps.contains(Capabilities::AUDIT_LOGGED));
    }

    #[test]
    fn requires_credential_returns_none_local_vault_is_self_unlocked() {
        // Per ADR-021 §4: local-vault is credential-free (it
        // unlocks via passphrase / biometric / recovery, all
        // handled internally by the daemon).
        let s = LocalVaultSource::with_socket_path("local-vault", "/tmp/agent.sock");
        assert!(s.requires_credential().is_none());
    }

    #[tokio::test]
    async fn validate_accepts_non_empty_reference() {
        let s = LocalVaultSource::with_socket_path("local-vault", "/tmp/agent.sock");
        s.validate("team/gitlab/token-deploy").await.unwrap();
    }

    #[tokio::test]
    async fn validate_rejects_empty_reference() {
        let s = LocalVaultSource::with_socket_path("local-vault", "/tmp/agent.sock");
        let err = s.validate("").await.unwrap_err();
        assert!(matches!(err, SourceError::BadReference { .. }));
    }

    #[tokio::test]
    async fn get_rejects_empty_reference() {
        let s = LocalVaultSource::with_socket_path("local-vault", "/tmp/agent.sock");
        let err = s.get("").await.unwrap_err();
        assert!(matches!(err, SourceError::BadReference { .. }));
    }

    // -- Daemon-down behaviour ------------------------------------------

    #[tokio::test]
    async fn is_available_returns_not_installed_when_no_daemon() {
        // Socket path that doesn't exist → connect fails → treat
        // as NotInstalled so the router fallback fires.
        let s = LocalVaultSource::with_socket_path(
            "local-vault",
            "/nonexistent/devboy-test/agent.sock",
        );
        let status = s.is_available().await;
        assert_eq!(status, SourceStatus::NotInstalled);
    }

    #[tokio::test]
    async fn get_when_daemon_is_down_returns_io_error() {
        let s = LocalVaultSource::with_socket_path(
            "local-vault",
            "/nonexistent/devboy-test/agent.sock",
        );
        let err = s.get("a/b/c").await.unwrap_err();
        match err {
            SourceError::Io { name, .. } => assert_eq!(name, "local-vault"),
            other => panic!("expected Io, got {other:?}"),
        }
    }

    // -- In-process daemon fixture --------------------------------------
    //
    // Spin up a real `VaultServer` bound to a temp socket so the
    // source can do an end-to-end round-trip through the JSON-RPC
    // wire.

    use devboy_secrets_agent::{AgentListener, VaultServer};
    use devboy_vault_crypto::{EnvelopeKdfParams, InitialUnlock, Vault};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    fn fast_init(passphrase: &str) -> InitialUnlock {
        InitialUnlock {
            passphrase: SecretString::from(passphrase.to_owned()),
            // Tiny KDF params so vault create + unlock is instant.
            passphrase_params: Some(EnvelopeKdfParams { m: 8, t: 1, p: 1 }),
            with_recovery: false,
            with_keychain_account: None,
        }
    }

    /// Spawn a daemon serving `vault_path` at a fresh socket. The
    /// returned `TempDir` keeps the socket directory alive for the
    /// caller's lifetime; the `JoinHandle` is the accept loop and
    /// stays running until the listener is dropped (when the dir
    /// goes away).
    async fn spawn_daemon(
        vault_path: PathBuf,
    ) -> (
        TempDir,
        PathBuf,
        Arc<Mutex<VaultServer>>,
        tokio::task::JoinHandle<()>,
    ) {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("agent.sock");
        let listener = AgentListener::bind(&socket_path).await.unwrap();
        let server = Arc::new(Mutex::new(VaultServer::new(vault_path)));
        let server_for_loop = server.clone();
        let handle = tokio::spawn(async move {
            loop {
                let stream = match listener.accept_authenticated().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let server = server_for_loop.clone();
                tokio::spawn(async move {
                    let mut s = server.lock().await;
                    let _ = s.serve_connection(stream).await;
                });
            }
        });
        (dir, socket_path, server, handle)
    }

    #[tokio::test]
    async fn is_available_returns_locked_when_daemon_is_locked() {
        let vdir = TempDir::new().unwrap();
        let vault_path = vdir.path().join("vault.dvb");
        Vault::create(&vault_path, fast_init("p")).unwrap();
        let (_dir, socket, _server, _handle) = spawn_daemon(vault_path).await;

        let s = LocalVaultSource::with_socket_path("local-vault", socket);
        let status = s.is_available().await;
        assert_eq!(status, SourceStatus::Locked);
    }

    #[tokio::test]
    async fn end_to_end_round_trip_through_real_daemon() {
        use secrecy::ExposeSecret;

        let vdir = TempDir::new().unwrap();
        let vault_path = vdir.path().join("vault.dvb");
        Vault::create(&vault_path, fast_init("p")).unwrap();
        let (_dir, socket, server, _handle) = spawn_daemon(vault_path).await;

        // Pre-unlock + put through the in-process server so we don't
        // have to wire the unlock RPC through the source surface
        // (the trait doesn't yet expose `put`).
        {
            let req_payload = JsonRpcRequest {
                jsonrpc: JSONRPC_VERSION.to_owned(),
                id: json!(1),
                method: "vault.unlock".to_owned(),
                params: json!({"kind": "passphrase", "secret": "p"}),
            };
            let mut s = server.lock().await;
            let _ = s.handle_request(req_payload).await;

            let put_req = JsonRpcRequest {
                jsonrpc: JSONRPC_VERSION.to_owned(),
                id: json!(2),
                method: "secret.put".to_owned(),
                params: json!({
                    "path": "team/gitlab/token-deploy",
                    "value": "glpat-fixture",
                    "fresh_unlock": {"kind": "passphrase", "secret": "p"},
                }),
            };
            let _ = s.handle_request(put_req).await;
        }

        // Now drive the public surface:
        let src = LocalVaultSource::with_socket_path("local-vault", socket);

        // is_available — should be Available now.
        let status = src.is_available().await;
        assert_eq!(status, SourceStatus::Available);

        // get — should return the value.
        let outcome = src
            .get("team/gitlab/token-deploy")
            .await
            .unwrap()
            .expect("expected Some after put");
        assert_eq!(outcome.value.expose_secret(), "glpat-fixture");
        assert!(outcome.lease_duration.is_none());

        // list — should include the path.
        let entries = src.list().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].reference, "team/gitlab/token-deploy");
    }

    #[tokio::test]
    async fn get_returns_locked_error_when_vault_is_locked() {
        let vdir = TempDir::new().unwrap();
        let vault_path = vdir.path().join("vault.dvb");
        Vault::create(&vault_path, fast_init("p")).unwrap();
        let (_dir, socket, _server, _handle) = spawn_daemon(vault_path).await;

        let src = LocalVaultSource::with_socket_path("local-vault", socket);
        let err = src.get("team/x/y").await.unwrap_err();
        match err {
            SourceError::Locked { name } => assert_eq!(name, "local-vault"),
            other => panic!("expected Locked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dyn_compat_smoke() {
        let s: Box<dyn SecretSource> = Box::new(LocalVaultSource::with_socket_path(
            "local-vault",
            "/tmp/x.sock",
        ));
        assert_eq!(s.name(), "local-vault");
        assert!(s.requires_credential().is_none());
        assert!(s.capabilities().contains(Capabilities::WRITE));
    }
}
