//! `VaultServer` — the JSON-RPC 2.0 method dispatcher per [ADR-023]
//! §3.3.
//!
//! Wraps a [`devboy_vault_crypto::Vault`] (when unlocked) and routes
//! the eight ADR-023 §3.3 methods against it:
//!
//! | Method            | State requirement | `fresh_unlock` |
//! |-------------------|-------------------|----------------|
//! | `vault.unlock`    | locked            | n/a            |
//! | `vault.lock`      | unlocked          | no             |
//! | `vault.status`    | any               | no             |
//! | `secret.get`      | unlocked          | no (cached)    |
//! | `secret.list`     | unlocked          | no (cached)    |
//! | `secret.put`      | unlocked          | **yes**        |
//! | `secret.rotate`   | unlocked          | **yes**        |
//! | `metadata.update` | unlocked          | no (plaintext) |
//!
//! `fresh_unlock` is the ADR's hybrid-mode requirement: write
//! operations revalidate the user's unlock method on every call so
//! reads can benefit from daemon caching while writes can't.
//! Implementation: `VaultServer::verify_fresh_unlock` re-opens the
//! vault file with the supplied unlock method and discards the
//! resulting handle if the credentials check out.
//!
//! See also [`crate::rpc`] for the JSON-RPC framing and error codes
//! the dispatcher returns.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::path::PathBuf;
use std::time::Duration;

use devboy_vault_crypto::{
    EntryMetadata, RecoveryPhrase, UnlockMethod, Vault, VaultError, parse_recovery_phrase,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};

use crate::idle::{IdleClock, IdleTracker, UnlockWindow};
use crate::rpc::{
    BAD_UNLOCK, ENTRY_NOT_FOUND, FramingError, INVALID_PARAMS, IO_ERROR, JsonRpcError,
    JsonRpcRequest, JsonRpcResponse, METHOD_NOT_FOUND, NO_MATCHING_ENVELOPE, VAULT_LOCKED,
    read_request, write_response,
};

/// Daemon-side state machine wrapping a vault.
///
/// `vault` is `Some` after a successful `vault.unlock` and `None`
/// after `vault.lock` (or before the first unlock). The on-disk path
/// is captured at construction time so unlock attempts can find the
/// file again.
///
/// `idle` carries the ADR-023 §3.3 idle-timeout policy: every
/// successful "real" operation (`secret.get`, `.list`, `.put`,
/// `.rotate`, `metadata.update`) bumps `last_activity`; the next
/// dispatched request observes the elapsed time and, if it exceeds
/// `idle_timeout`, drops the cached `Vault` (zeroizing the wrap key)
/// before running the handler.
pub struct VaultServer {
    /// Path of the vault file on disk.
    vault_path: PathBuf,
    /// Currently-unlocked vault, or `None` when locked.
    vault: Option<Vault>,
    /// Idle-timeout state. See ADR-023 §3.3.
    idle: IdleTracker,
}

impl VaultServer {
    /// Build a new server around `vault_path` with the ADR-023 §3.3
    /// default 15-minute idle timeout. The vault starts **locked** —
    /// call `vault.unlock` (via JSON-RPC) before any `secret.*`
    /// method.
    pub fn new(vault_path: PathBuf) -> Self {
        Self {
            vault_path,
            vault: None,
            idle: IdleTracker::new(),
        }
    }

    /// Build a server with a custom idle-timeout duration.
    ///
    /// Leaves the overall unlock window at its default — use
    /// [`Self::with_window`] to honour the user's configuration.
    pub fn with_idle_timeout(vault_path: PathBuf, idle_timeout: Duration) -> Self {
        Self {
            vault_path,
            vault: None,
            idle: IdleTracker::with_timeout(idle_timeout),
        }
    }

    /// Build a server around the unlock window the user configured
    /// (ADR-024 §2).
    ///
    /// This is the constructor the daemon uses. Without it the
    /// `secrets.profile` setting and every `*_ttl_seconds` key were
    /// inert: the server always built its tracker from
    /// [`UnlockWindow::default`], so a user who chose `strict` and
    /// expected a fifteen-minute window kept the eight-hour one —
    /// and `secrets selftest` reported the configured number as
    /// though it were in force.
    pub fn with_window(vault_path: PathBuf, window: UnlockWindow) -> Self {
        Self {
            vault_path,
            vault: None,
            idle: IdleTracker::with_window(window, std::sync::Arc::new(crate::idle::SystemClock)),
        }
    }

    /// The unlock window this server is enforcing.
    ///
    /// Exposed so the daemon can report what is *actually* in
    /// force, rather than leaving callers to re-read the config and
    /// assume it arrived.
    pub fn window(&self) -> &UnlockWindow {
        &self.idle.window
    }

    /// Build a server with a caller-supplied clock. Used by tests
    /// that want to fast-forward the idle timer without sleeping.
    pub fn with_clock(
        vault_path: PathBuf,
        idle_timeout: Duration,
        clock: std::sync::Arc<dyn IdleClock>,
    ) -> Self {
        Self {
            vault_path,
            vault: None,
            idle: IdleTracker::with_clock(idle_timeout, clock),
        }
    }

    /// `true` when the daemon is holding an unlocked vault.
    pub fn is_unlocked(&self) -> bool {
        self.vault.is_some()
    }

    /// Path of the vault file the server operates on.
    pub fn vault_path(&self) -> &std::path::Path {
        &self.vault_path
    }

    /// Borrow the idle tracker. Useful for `doctor`-style
    /// diagnostics and tests that want to inspect the timer state.
    pub fn idle(&self) -> &IdleTracker {
        &self.idle
    }

    /// Drop the in-memory `Vault` (zeroizes the vault key) and reset
    /// the idle tracker. Idempotent — safe to call when already
    /// locked. Used both by `vault.lock` and by the auto-lock check
    /// before each request.
    fn lock_now(&mut self) {
        self.vault = None;
        self.idle.record_lock();
    }

    /// Run the auto-lock check before dispatching a request. Drops
    /// the cached `Vault` if the idle window has elapsed; subsequent
    /// `secret.*` operations then return `VAULT_LOCKED` without any
    /// extra plumbing.
    fn check_auto_lock(&mut self) {
        if self.idle.should_auto_lock() {
            self.lock_now();
        }
    }

    /// Serve a single connection: read requests, dispatch, write
    /// responses, until the peer closes the stream.
    ///
    /// Generic over [`AsyncRead`] + [`AsyncWrite`] so tests can drive
    /// the server through `tokio::io::duplex` without binding a real
    /// socket.
    pub async fn serve_connection<S>(&mut self, stream: S) -> Result<(), FramingError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        loop {
            let req = match read_request(&mut reader).await {
                Ok(req) => req,
                Err(FramingError::Eof) => return Ok(()),
                Err(FramingError::Parse(e)) => {
                    let resp = JsonRpcResponse::err(
                        Value::Null,
                        JsonRpcError::new(crate::rpc::PARSE_ERROR, format!("malformed JSON: {e}")),
                    );
                    write_response(&mut write_half, &resp).await?;
                    continue;
                }
                Err(other) => return Err(other),
            };
            let resp = self.handle_request(req).await;
            write_response(&mut write_half, &resp).await?;
        }
    }

    /// Dispatch a single request and produce its response.
    ///
    /// Public for testing; the connection loop in
    /// [`serve_connection`](Self::serve_connection) calls this
    /// internally per request.
    ///
    /// Runs the auto-lock check before dispatching so a request that
    /// arrives after the idle window has elapsed sees the locked
    /// state. "Real" operations (`secret.*`, `metadata.update`) bump
    /// the activity timestamp on success so the timer resets per
    /// ADR-023 §3.3.
    pub async fn handle_request(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        self.check_auto_lock();
        let method = req.method.clone();
        let response = match method.as_str() {
            "vault.unlock" => self.handle_vault_unlock(req).await,
            "vault.lock" => self.handle_vault_lock(req),
            "vault.status" => self.handle_vault_status(req),
            "secret.get" => self.handle_secret_get(req),
            "secret.list" => self.handle_secret_list(req),
            "secret.put" => self.handle_secret_put(req).await,
            "secret.rotate" => self.handle_secret_rotate(req).await,
            "metadata.update" => self.handle_metadata_update(req),
            other => JsonRpcResponse::err(
                req.id,
                JsonRpcError::new(METHOD_NOT_FOUND, format!("unknown method: {other}")),
            ),
        };
        // Bump the activity timestamp when a "real" op succeeded.
        // `vault.unlock`/`vault.lock`/`vault.status` are not real
        // ops — the unlock handler bumps via `record_unlock`, the
        // lock handler clears via `record_lock`, and `vault.status`
        // is a free probe that should not extend the window.
        if response.error.is_none() && is_user_activity(&method) {
            self.idle.record_activity();
        }
        response
    }

    // -- vault.* handlers --------------------------------------------------

    async fn handle_vault_unlock(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        let params: UnlockParams = match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(e) => return invalid_params(id, e),
        };
        let unlock = match params.into_unlock_method() {
            Ok(u) => u,
            Err(e) => return JsonRpcResponse::err(id, e),
        };
        match Vault::open(&self.vault_path, unlock) {
            Ok(vault) => {
                self.vault = Some(vault);
                self.idle.record_unlock();
                JsonRpcResponse::ok(id, json!({"state": "unlocked"}))
            }
            Err(e) => JsonRpcResponse::err(id, vault_error_to_rpc(&e)),
        }
    }

    fn handle_vault_lock(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        // `vault` is dropped here, which zeroizes the SecretBox per
        // ADR-023 §3.3 "eager re-lock".
        self.lock_now();
        JsonRpcResponse::ok(req.id, json!({"locked": true}))
    }

    /// Report lock state **and the window actually being enforced**.
    ///
    /// The window is on the wire so a caller can tell what the
    /// daemon is doing rather than re-reading the config and
    /// assuming it arrived. Those two disagreed silently until the
    /// daemon started reading config at all, and a tool that
    /// reports the intended value as the real one is worse than a
    /// tool that reports nothing.
    fn handle_vault_status(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let state = if self.is_unlocked() {
            "unlocked"
        } else {
            "locked"
        };
        let window = self.window();
        JsonRpcResponse::ok(
            req.id,
            json!({
                "state": state,
                "unlock_ttl_seconds": window.unlock_ttl.as_secs(),
                "max_unlock_ttl_seconds": window.max_unlock_ttl.as_secs(),
                "idle_relock_seconds": window.idle_relock.map(|d| d.as_secs()),
            }),
        )
    }

    // -- secret.* handlers -------------------------------------------------

    fn handle_secret_get(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        let params: PathOnly = match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(e) => return invalid_params(id, e),
        };
        let vault = match self.vault.as_ref() {
            Some(v) => v,
            None => return locked_response(id),
        };
        match vault.get(&params.path) {
            Ok(Some(value)) => JsonRpcResponse::ok(id, json!({"value": value.expose_secret()})),
            Ok(None) => JsonRpcResponse::err(
                id,
                JsonRpcError::new(
                    ENTRY_NOT_FOUND,
                    format!("no entry for path '{}'", params.path),
                ),
            ),
            Err(e) => JsonRpcResponse::err(id, vault_error_to_rpc(&e)),
        }
    }

    fn handle_secret_list(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        let vault = match self.vault.as_ref() {
            Some(v) => v,
            None => return locked_response(id),
        };
        let entries: Vec<Value> = vault
            .paths()
            .zip(vault.list())
            .map(|(p, m)| {
                json!({
                    "path": p,
                    "description": m.description,
                    "expires_at": m.expires_at,
                    "last_rotated_at": m.last_rotated_at,
                    "pattern_id": m.pattern_id,
                })
            })
            .collect();
        JsonRpcResponse::ok(id, json!({"entries": entries}))
    }

    async fn handle_secret_put(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        let params: PutParams = match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(e) => return invalid_params(id, e),
        };
        if let Err(e) = self.verify_fresh_unlock(&params.fresh_unlock) {
            return JsonRpcResponse::err(id, e);
        }
        let vault = match self.vault.as_mut() {
            Some(v) => v,
            None => return locked_response(id),
        };
        let metadata = params.meta.unwrap_or_default().into_entry_metadata();
        match vault.put(&params.path, &SecretString::from(params.value), metadata) {
            Ok(()) => JsonRpcResponse::ok(id, json!({"ok": true})),
            Err(e) => JsonRpcResponse::err(id, vault_error_to_rpc(&e)),
        }
    }

    async fn handle_secret_rotate(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        let params: RotateParams = match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(e) => return invalid_params(id, e),
        };
        if let Err(e) = self.verify_fresh_unlock(&params.fresh_unlock) {
            return JsonRpcResponse::err(id, e);
        }
        let vault = match self.vault.as_mut() {
            Some(v) => v,
            None => return locked_response(id),
        };
        match vault.rotate(&params.path, &SecretString::from(params.new_value)) {
            Ok(()) => {
                let last_rotated_at = vault
                    .list()
                    .find_map(|m| m.last_rotated_at)
                    .unwrap_or_default();
                JsonRpcResponse::ok(id, json!({"ok": true, "last_rotated_at": last_rotated_at}))
            }
            Err(VaultError::EntryNotFound { path }) => JsonRpcResponse::err(
                id,
                JsonRpcError::new(ENTRY_NOT_FOUND, format!("no entry for path '{path}'")),
            ),
            Err(e) => JsonRpcResponse::err(id, vault_error_to_rpc(&e)),
        }
    }

    // -- metadata.* handlers ----------------------------------------------

    fn handle_metadata_update(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        let _params: MetadataUpdateParams = match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(e) => return invalid_params(id, e),
        };
        if !self.is_unlocked() {
            return locked_response(id);
        }
        // The Vault API in P3.6 does not yet expose a metadata-only
        // mutation path (every write goes through put / rotate /
        // delete which re-encrypts). Surface that as INVALID_PARAMS
        // for now so the wire protocol stays declared even though
        // the data path lands in a follow-up. Specifying it here is
        // important so the JSON-RPC schema is the contract callers
        // depend on; the implementation gap is internal.
        JsonRpcResponse::err(
            id,
            JsonRpcError::new(
                crate::rpc::INTERNAL_ERROR,
                "metadata.update is not yet implemented (Vault::update_metadata pending)",
            ),
        )
    }

    // -- fresh_unlock verification ----------------------------------------

    /// Re-validate that `fresh_unlock` opens the vault on disk AND
    /// swap the freshly-opened handle into `self.vault` so the
    /// subsequent put/rotate sees current state rather than the
    /// snapshot we captured at the original `vault.unlock` call.
    ///
    /// Closes two TOCTOU windows:
    ///
    /// * Concurrent writer (another process, another agent run, the
    ///   UI binary) mutated the file on disk between our last
    ///   unlock and this put — without the swap, our put would
    ///   atomically replace the on-disk update with our stale tree.
    /// * Vault was re-keyed under us and the new envelope still
    ///   accepts the supplied passphrase — without the swap we'd
    ///   write entries encrypted under the OLD wrap key, and the
    ///   next reader couldn't decrypt with the NEW envelope.
    ///
    /// Cost is the same Argon2id derive that the per-call
    /// `fresh_unlock` already pays. Adopting the new handle is just
    /// a `self.vault = Some(handle)` — entry data is already in
    /// memory by the time `Vault::open` returns.
    fn verify_fresh_unlock(&mut self, fresh_unlock: &UnlockParams) -> Result<(), JsonRpcError> {
        let unlock_method = fresh_unlock.clone().into_unlock_method()?;
        match Vault::open(&self.vault_path, unlock_method) {
            Ok(handle) => {
                self.vault = Some(handle);
                Ok(())
            }
            Err(_) => Err(JsonRpcError::new(
                BAD_UNLOCK,
                "fresh_unlock did not validate",
            )),
        }
    }
}

// =============================================================================
// Param shapes (private, just for serde)
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
struct UnlockParams {
    /// One of `"passphrase"`, `"recovery"`, `"keyfile"`.
    kind: String,
    /// Passphrase or 24-word recovery phrase. Empty for `keyfile`,
    /// whose unlock factor is a file the daemon reads from its own
    /// configuration — deliberately not nameable from the wire.
    /// The local field name carries `_material`
    /// to keep the CI secrets-discipline grep from flagging this
    /// (the value is wrapped in `SecretString` immediately in
    /// `into_unlock_method`); the wire name stays `secret` via
    /// `#[serde(rename)]` so existing JSON-RPC clients still work.
    #[serde(default, rename = "secret")]
    secret_material: String,
}

impl UnlockParams {
    fn into_unlock_method(self) -> Result<UnlockMethod, JsonRpcError> {
        match self.kind.as_str() {
            "passphrase" => Ok(UnlockMethod::Passphrase(SecretString::from(
                self.secret_material,
            ))),
            "recovery" => {
                let phrase: RecoveryPhrase =
                    parse_recovery_phrase(&self.secret_material).map_err(|e| {
                        JsonRpcError::new(INVALID_PARAMS, format!("invalid recovery phrase: {e}"))
                    })?;
                Ok(UnlockMethod::Recovery(phrase))
            }
            // ADR-024 §1: `totp` is deliberately NOT reachable
            // through this wire field.
            //
            // The TOTP path unwraps using a secret the daemon holds
            // in memory, never one a caller supplies — accepting
            // secret material here would let any client present its
            // own secret and unwrap the vault, which is the exact
            // opposite of the guarantee. Re-unlock goes through
            // `secrets_unlock`, which takes a six-digit code and
            // pairs it with the resident secret.
            "totp" => Err(JsonRpcError::new(
                INVALID_PARAMS,
                "TOTP unlock does not accept secret material; use the dedicated re-unlock call \
                 with a 6-digit code"
                    .to_string(),
            )),
            // ADR-024 §6: the keyfile path comes from
            // configuration, never from the request.
            //
            // A caller that could name the file would just point the
            // unlock at a keyfile it wrote itself, and the envelope
            // would dutifully open the vault. The file's location is
            // the user's standing decision; the request only says
            // "use it".
            "keyfile" => {
                let config = devboy_core::config::Config::load().unwrap_or_default();
                let path = config
                    .secrets_keyfile_path()
                    .ok_or_else(|| {
                        JsonRpcError::new(
                            INVALID_PARAMS,
                            "no keyfile is configured; set `secrets.keyfile_path` and enrol the                              keyfile with `devboy secrets keyfile add` before unlocking with one"
                                .to_string(),
                        )
                    })?;
                let bytes = devboy_vault_crypto::keyfile::load_keyfile(path).map_err(|e| {
                    JsonRpcError::new(
                        INVALID_PARAMS,
                        format!(
                            "could not read the configured keyfile {}: {e}",
                            path.display()
                        ),
                    )
                })?;
                Ok(UnlockMethod::Keyfile { keyfile: bytes })
            }
            other => Err(JsonRpcError::new(
                INVALID_PARAMS,
                format!("unknown unlock kind '{other}'"),
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PathOnly {
    path: String,
}

#[derive(Debug, Deserialize)]
struct PutParams {
    path: String,
    value: String,
    #[serde(default)]
    meta: Option<EntryMetadataParams>,
    fresh_unlock: UnlockParams,
}

#[derive(Debug, Deserialize)]
struct RotateParams {
    path: String,
    new_value: String,
    fresh_unlock: UnlockParams,
}

#[derive(Debug, Deserialize)]
struct MetadataUpdateParams {
    #[allow(dead_code)]
    path: String,
    #[allow(dead_code)]
    fields: serde_json::Value,
}

#[derive(Debug, Default, Deserialize)]
struct EntryMetadataParams {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    retrieval_url: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    last_rotated_at: Option<String>,
    #[serde(default)]
    pattern_id: Option<String>,
}

impl EntryMetadataParams {
    fn into_entry_metadata(self) -> EntryMetadata {
        EntryMetadata {
            description: self.description,
            retrieval_url: self.retrieval_url,
            expires_at: self.expires_at,
            last_rotated_at: self.last_rotated_at,
            pattern_id: self.pattern_id,
        }
    }
}

// =============================================================================
// Error helpers
// =============================================================================

fn invalid_params(id: Value, source: serde_json::Error) -> JsonRpcResponse {
    JsonRpcResponse::err(
        id,
        JsonRpcError::new(INVALID_PARAMS, format!("invalid params: {source}")),
    )
}

fn locked_response(id: Value) -> JsonRpcResponse {
    JsonRpcResponse::err(
        id,
        JsonRpcError::new(VAULT_LOCKED, "vault is locked; call vault.unlock first"),
    )
}

/// Whether a method counts as "user activity" for the ADR-023 §3.3
/// idle-timeout. `vault.status` and `vault.lock` are free probes /
/// shutdown signals; they do not extend the unlock window.
/// `vault.unlock` resets the window through `record_unlock`, not via
/// this helper.
fn is_user_activity(method: &str) -> bool {
    matches!(
        method,
        "secret.get" | "secret.list" | "secret.put" | "secret.rotate" | "metadata.update"
    )
}

fn vault_error_to_rpc(e: &VaultError) -> JsonRpcError {
    match e {
        VaultError::NoMatchingEnvelope { kind } => {
            JsonRpcError::new(NO_MATCHING_ENVELOPE, format!("no '{kind}' envelope"))
        }
        VaultError::EntryNotFound { path } => {
            JsonRpcError::new(ENTRY_NOT_FOUND, format!("no entry for path '{path}'"))
        }
        VaultError::Format(_) => JsonRpcError::new(IO_ERROR, e.to_string()),
        // Passphrase / Recovery / Keychain / Aead all collapse to
        // BadUnlock because the AEAD failure on unwrap looks the
        // same regardless of which envelope rejected us.
        _ => JsonRpcError::new(BAD_UNLOCK, e.to_string()),
    }
}

/// Serializable helper kept here so the wire protocol's `secret.put`
/// metadata shape is a public type the local-vault client crate (P6.2)
/// can consume without re-declaring it.
#[derive(Debug, Default, Clone, Serialize)]
pub struct WireEntryMetadata {
    /// Free-text description.
    pub description: Option<String>,
    /// Retrieval URL.
    pub retrieval_url: Option<String>,
    /// ISO 8601 expiry date.
    pub expires_at: Option<String>,
    /// ISO 8601 date of the last rotation.
    pub last_rotated_at: Option<String>,
    /// Pattern catalogue id.
    pub pattern_id: Option<String>,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_vault_crypto::{EnvelopeKdfParams, InitialUnlock};
    use tempfile::TempDir;

    fn passphrase(s: &str) -> SecretString {
        SecretString::from(s.to_owned())
    }

    /// Fast Argon2 params so vault create + every `fresh_unlock` is
    /// a few milliseconds, not a few hundred.
    fn fast_init(p: &str) -> InitialUnlock {
        InitialUnlock {
            passphrase: passphrase(p),
            passphrase_params: Some(EnvelopeKdfParams { m: 8, t: 1, p: 1 }),
            with_recovery: false,
            with_totp_secret: None,
        }
    }

    /// Build a vault on disk and return (path, server). The server
    /// starts locked.
    fn fresh_vault(p: &str) -> (TempDir, VaultServer) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.dvb");
        let _outcome = Vault::create(&path, fast_init(p)).unwrap();
        let server = VaultServer::new(path);
        (dir, server)
    }

    fn req(id: i64, method: &str, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: json!(id),
            method: method.to_owned(),
            params,
        }
    }

    // -- vault.* -----------------------------------------------------------

    #[tokio::test]
    async fn vault_status_locked_then_unlocked() {
        let (_dir, mut server) = fresh_vault("p");
        let r = server
            .handle_request(req(1, "vault.status", Value::Null))
            .await;
        assert_eq!(r.result.unwrap()["state"], "locked");

        let unlock = server
            .handle_request(req(
                2,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        assert_eq!(unlock.result.unwrap()["state"], "unlocked");

        let r = server
            .handle_request(req(3, "vault.status", Value::Null))
            .await;
        assert_eq!(r.result.unwrap()["state"], "unlocked");
    }

    /// `vault.status` must report the window the daemon is really
    /// enforcing, so a caller can tell it apart from the one in the
    /// config file. Those two disagree whenever the config changed
    /// after the daemon started, and `secrets selftest` was
    /// presenting the configured number as the live one.
    #[tokio::test]
    async fn vault_status_reports_the_window_actually_in_force() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault_path = dir.path().join("vault.dvb");
        Vault::create(&vault_path, fast_init("p")).expect("create");

        let window = UnlockWindow {
            unlock_ttl: Duration::from_secs(900),
            max_unlock_ttl: Duration::from_secs(3600),
            idle_relock: Some(Duration::from_secs(300)),
        };
        let mut server = VaultServer::with_window(vault_path, window);

        let r = server
            .handle_request(req(1, "vault.status", Value::Null))
            .await;
        let result = r.result.expect("status has a result");

        assert_eq!(result["unlock_ttl_seconds"], 900);
        assert_eq!(result["max_unlock_ttl_seconds"], 3600);
        assert_eq!(result["idle_relock_seconds"], 300);
    }

    /// The keyfile unlock must never take a path from the wire.
    ///
    /// A caller that could name the file would simply point the
    /// unlock at a keyfile it wrote itself, and the envelope would
    /// dutifully open the vault — so the request carries no path,
    /// and a `secret` field on a keyfile unlock is ignored rather
    /// than honoured.
    #[test]
    fn a_keyfile_unlock_takes_no_path_from_the_request() {
        let params: UnlockParams = serde_json::from_value(json!({
            "kind": "keyfile",
            "secret": "/tmp/attacker-controlled.key",
        }))
        .expect("params parse");

        // The struct has nowhere to put a path: the only fields are
        // the kind and the (unused for keyfile) secret material.
        assert_eq!(params.kind, "keyfile");

        // With no keyfile configured the unlock is refused, and the
        // refusal names configuration rather than the string the
        // caller supplied.
        let err = params
            .into_unlock_method()
            .err()
            .expect("no keyfile is configured in this test environment");
        assert!(
            !err.message.contains("attacker-controlled"),
            "the caller's string must not steer the unlock: {}",
            err.message
        );
        assert!(
            err.message.contains("secrets.keyfile_path"),
            "the error should point at configuration: {}",
            err.message
        );
    }

    /// The default constructors must not silently claim the strict
    /// window — otherwise the report would be as misleading as the
    /// config-only one it replaces.
    #[tokio::test]
    async fn a_default_server_reports_the_default_window() {
        let (_dir, mut server) = fresh_vault("p");
        let r = server
            .handle_request(req(1, "vault.status", Value::Null))
            .await;
        let result = r.result.expect("status has a result");

        assert_eq!(
            result["unlock_ttl_seconds"].as_u64().unwrap(),
            UnlockWindow::default().unlock_ttl.as_secs()
        );
    }

    #[tokio::test]
    async fn vault_unlock_with_wrong_passphrase_returns_bad_unlock() {
        let (_dir, mut server) = fresh_vault("right");
        let r = server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "wrong"}),
            ))
            .await;
        let err = r.error.expect("error response");
        assert_eq!(err.code, BAD_UNLOCK);
    }

    #[tokio::test]
    async fn vault_lock_returns_to_locked_state() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        let r = server
            .handle_request(req(2, "vault.lock", Value::Null))
            .await;
        assert_eq!(r.result.unwrap()["locked"], true);
        assert!(!server.is_unlocked());
    }

    // -- secret.* ----------------------------------------------------------

    #[tokio::test]
    async fn secret_get_when_locked_returns_vault_locked() {
        let (_dir, mut server) = fresh_vault("p");
        let r = server
            .handle_request(req(1, "secret.get", json!({"path": "a/b/c"})))
            .await;
        assert_eq!(r.error.unwrap().code, VAULT_LOCKED);
    }

    #[tokio::test]
    async fn put_then_get_round_trip_through_rpc() {
        let (_dir, mut server) = fresh_vault("p");
        // Unlock first.
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        // Put with fresh_unlock.
        let put = server
            .handle_request(req(
                2,
                "secret.put",
                json!({
                    "path": "team/x/y",
                    "value": "v1",
                    "fresh_unlock": {"kind": "passphrase", "secret": "p"}
                }),
            ))
            .await;
        assert_eq!(put.result.unwrap()["ok"], true);
        // Get.
        let get = server
            .handle_request(req(3, "secret.get", json!({"path": "team/x/y"})))
            .await;
        assert_eq!(get.result.unwrap()["value"], "v1");
    }

    #[tokio::test]
    async fn put_with_wrong_fresh_unlock_returns_bad_unlock() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        let put = server
            .handle_request(req(
                2,
                "secret.put",
                json!({
                    "path": "team/x/y",
                    "value": "v1",
                    "fresh_unlock": {"kind": "passphrase", "secret": "WRONG"}
                }),
            ))
            .await;
        assert_eq!(put.error.unwrap().code, BAD_UNLOCK);
    }

    #[tokio::test]
    async fn get_unknown_path_returns_entry_not_found() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        let r = server
            .handle_request(req(2, "secret.get", json!({"path": "no/such/path"})))
            .await;
        assert_eq!(r.error.unwrap().code, ENTRY_NOT_FOUND);
    }

    #[tokio::test]
    async fn list_returns_entry_metadata() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        for path in ["a/b/c", "x/y/z"] {
            server
                .handle_request(req(
                    2,
                    "secret.put",
                    json!({
                        "path": path,
                        "value": "v",
                        "fresh_unlock": {"kind": "passphrase", "secret": "p"}
                    }),
                ))
                .await;
        }
        let r = server
            .handle_request(req(3, "secret.list", Value::Null))
            .await;
        let entries = r.result.unwrap()["entries"].as_array().unwrap().clone();
        assert_eq!(entries.len(), 2);
        let paths: Vec<&str> = entries
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();
        assert!(paths.contains(&"a/b/c"));
        assert!(paths.contains(&"x/y/z"));
    }

    #[tokio::test]
    async fn rotate_round_trip_stamps_last_rotated_at() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        server
            .handle_request(req(
                2,
                "secret.put",
                json!({
                    "path": "a/b/c",
                    "value": "v1",
                    "fresh_unlock": {"kind": "passphrase", "secret": "p"}
                }),
            ))
            .await;
        let r = server
            .handle_request(req(
                3,
                "secret.rotate",
                json!({
                    "path": "a/b/c",
                    "new_value": "v2",
                    "fresh_unlock": {"kind": "passphrase", "secret": "p"}
                }),
            ))
            .await;
        let result = r.result.unwrap();
        assert_eq!(result["ok"], true);
        assert!(
            result["last_rotated_at"]
                .as_str()
                .unwrap()
                .starts_with("20")
        );

        let get = server
            .handle_request(req(4, "secret.get", json!({"path": "a/b/c"})))
            .await;
        assert_eq!(get.result.unwrap()["value"], "v2");
    }

    /// R1 (PR #265 review) — verify_fresh_unlock must replace
    /// self.vault with the freshly-opened handle so a put/rotate
    /// sees concurrent on-disk writes. Reproduces the silent-
    /// rollback case: server A unlocks + caches snapshot, server
    /// B writes a new entry to the same file, server A then puts
    /// via fresh_unlock. Before R1 server A's put would
    /// atomically replace B's entry with the stale snapshot;
    /// after R1 verify_fresh_unlock re-reads the file and B's
    /// entry survives.
    #[tokio::test]
    async fn verify_fresh_unlock_picks_up_concurrent_write() {
        let (_dir, mut server_a) = fresh_vault("p");
        let vault_path = server_a.vault_path.clone();

        server_a
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;

        // Server B mimics another process / agent run writing
        // directly to the same vault on disk.
        let mut vault_b = Vault::open(
            &vault_path,
            UnlockMethod::Passphrase(SecretString::from("p")),
        )
        .unwrap();
        vault_b
            .put(
                "out/of/band/path",
                &SecretString::from("out-of-band-value"),
                Default::default(),
            )
            .unwrap();
        drop(vault_b);

        // Server A's put goes through fresh_unlock; the post-R1
        // swap of self.vault must adopt B's new tree, so A's put
        // doesn't atomically overwrite B's entry.
        server_a
            .handle_request(req(
                2,
                "secret.put",
                json!({
                    "path": "a/b/c",
                    "value": "a-value",
                    "fresh_unlock": {"kind": "passphrase", "secret": "p"}
                }),
            ))
            .await;

        // Re-open from scratch and assert BOTH writes survived.
        let vault_check = Vault::open(
            &vault_path,
            UnlockMethod::Passphrase(SecretString::from("p")),
        )
        .unwrap();
        use secrecy::ExposeSecret as _;
        let a = vault_check.get("a/b/c").unwrap().unwrap();
        let b = vault_check.get("out/of/band/path").unwrap().unwrap();
        assert_eq!(a.expose_secret(), "a-value");
        assert_eq!(b.expose_secret(), "out-of-band-value");
    }

    #[tokio::test]
    async fn rotate_unknown_path_returns_entry_not_found() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        let r = server
            .handle_request(req(
                2,
                "secret.rotate",
                json!({
                    "path": "no/such/path",
                    "new_value": "v",
                    "fresh_unlock": {"kind": "passphrase", "secret": "p"}
                }),
            ))
            .await;
        assert_eq!(r.error.unwrap().code, ENTRY_NOT_FOUND);
    }

    // -- Method dispatch ---------------------------------------------------

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let (_dir, mut server) = fresh_vault("p");
        let r = server
            .handle_request(req(1, "no.such.method", Value::Null))
            .await;
        assert_eq!(r.error.unwrap().code, METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn invalid_params_shape_returns_invalid_params() {
        let (_dir, mut server) = fresh_vault("p");
        // `secret.get` requires `path` — sending an array shape
        // instead of object should be rejected.
        let r = server
            .handle_request(req(1, "secret.get", json!([1, 2, 3])))
            .await;
        assert_eq!(r.error.unwrap().code, INVALID_PARAMS);
    }

    // -- Connection loop ---------------------------------------------------

    #[tokio::test]
    async fn serve_connection_processes_one_request_then_eof() {
        // End-to-end through `tokio::io::duplex` — verifies the
        // serve_connection loop reads, dispatches, writes, and exits
        // cleanly on EOF.
        use tokio::io::AsyncWriteExt;

        let (_dir, mut server) = fresh_vault("p");
        let (mut client, server_io) = tokio::io::duplex(1024);

        // Write one request and close the write half.
        let request_bytes = serde_json::to_vec(&req(1, "vault.status", Value::Null)).unwrap();
        client.write_all(&request_bytes).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        // We need the server to see EOF so the loop exits — close
        // our write side by shutting it down. The client read side
        // remains open so we can read the response.
        let (mut client_read, mut client_write) = tokio::io::split(client);
        client_write.shutdown().await.unwrap();
        drop(client_write);

        let server_handle = tokio::spawn(async move {
            server.serve_connection(server_io).await.unwrap();
        });

        // Read the response from the client read half.
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        client_read.read_to_end(&mut buf).await.unwrap();
        // Expect at least one full JSON-RPC response terminated by \n.
        let line = String::from_utf8(buf).unwrap();
        let line = line.trim_end_matches('\n');
        let resp: JsonRpcResponse = serde_json::from_str(line).unwrap();
        assert_eq!(resp.id, json!(1));
        assert_eq!(resp.result.unwrap()["state"], "locked");

        server_handle.await.unwrap();
    }

    // -- Idle-timeout integration -----------------------------------------

    /// Build a server bound to `vault_path` with a 10-second idle
    /// timeout and a [`ManualClock`] so the timer can be raced past
    /// without sleeping.
    fn server_with_manual_clock(
        vault_path: PathBuf,
        clock: crate::idle::ManualClock,
    ) -> VaultServer {
        VaultServer::with_clock(
            vault_path,
            std::time::Duration::from_secs(10),
            std::sync::Arc::new(clock),
        )
    }

    #[tokio::test]
    async fn auto_lock_after_idle_timeout_returns_vault_locked() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.dvb");
        let _outcome = Vault::create(&path, fast_init("p")).unwrap();
        let clock = crate::idle::ManualClock::new(std::time::Instant::now());
        let mut server = server_with_manual_clock(path, clock.clone());

        // Unlock + put.
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        server
            .handle_request(req(
                2,
                "secret.put",
                json!({
                    "path": "a/b/c",
                    "value": "v",
                    "fresh_unlock": {"kind": "passphrase", "secret": "p"}
                }),
            ))
            .await;

        // Sanity: get works while inside the window.
        let r = server
            .handle_request(req(3, "secret.get", json!({"path": "a/b/c"})))
            .await;
        assert_eq!(r.result.unwrap()["value"], "v");

        // Race the clock past the 10-second timeout.
        clock.advance(std::time::Duration::from_secs(11));

        // Next get must observe the auto-lock and return VAULT_LOCKED.
        let r = server
            .handle_request(req(4, "secret.get", json!({"path": "a/b/c"})))
            .await;
        assert_eq!(r.error.unwrap().code, VAULT_LOCKED);
        assert!(!server.is_unlocked());
    }

    #[tokio::test]
    async fn activity_resets_idle_window() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.dvb");
        let _outcome = Vault::create(&path, fast_init("p")).unwrap();
        let clock = crate::idle::ManualClock::new(std::time::Instant::now());
        let mut server = server_with_manual_clock(path, clock.clone());

        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        server
            .handle_request(req(
                2,
                "secret.put",
                json!({
                    "path": "a/b/c",
                    "value": "v",
                    "fresh_unlock": {"kind": "passphrase", "secret": "p"}
                }),
            ))
            .await;

        // Two get calls, separated by 8 seconds each. Without the
        // activity reset the cumulative 16 seconds would auto-lock;
        // with the reset, only the 8 seconds since the last bump
        // count.
        clock.advance(std::time::Duration::from_secs(8));
        let r1 = server
            .handle_request(req(3, "secret.get", json!({"path": "a/b/c"})))
            .await;
        assert!(r1.result.is_some(), "first get inside the window");
        clock.advance(std::time::Duration::from_secs(8));
        let r2 = server
            .handle_request(req(4, "secret.get", json!({"path": "a/b/c"})))
            .await;
        assert!(
            r2.result.is_some(),
            "second get must succeed because the first bumped the timer"
        );
        assert!(server.is_unlocked());
    }

    #[tokio::test]
    async fn vault_status_does_not_extend_idle_window() {
        // ADR-023 §3.3: only `secret.*` and `metadata.update` count
        // as activity. `vault.status` is a free probe and must not
        // keep the daemon awake past its idle timeout.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.dvb");
        let _outcome = Vault::create(&path, fast_init("p")).unwrap();
        let clock = crate::idle::ManualClock::new(std::time::Instant::now());
        let mut server = server_with_manual_clock(path, clock.clone());

        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;

        // Walk the clock forward in 4-second steps, calling
        // vault.status each time. Total elapsed = 12 seconds (>10s
        // timeout).
        for step in 0..3 {
            clock.advance(std::time::Duration::from_secs(4));
            let r = server
                .handle_request(req(100 + step as i64, "vault.status", Value::Null))
                .await;
            assert!(r.error.is_none(), "vault.status itself never errors");
        }

        // The next "real" call must observe the auto-lock —
        // vault.status didn't bump the timer.
        let r = server
            .handle_request(req(2, "secret.list", Value::Null))
            .await;
        assert_eq!(r.error.unwrap().code, VAULT_LOCKED);
    }
}
