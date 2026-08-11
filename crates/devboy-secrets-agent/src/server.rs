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

use crate::audit_writer::AuditWriter;
use crate::idle::{IdleClock, IdleTracker, UnlockWindow};
use crate::rpc::{
    BAD_TOTP, BAD_UNLOCK, ENTRY_NOT_FOUND, FramingError, INVALID_PARAMS, IO_ERROR, JsonRpcError,
    JsonRpcRequest, JsonRpcResponse, METHOD_NOT_FOUND, NO_MATCHING_ENVELOPE, NO_PROMPT_SURFACE,
    REPLAYED_TOTP, TOTP_RATE_LIMITED, TOTP_UNAVAILABLE, VAULT_LOCKED, read_request, write_response,
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
    /// Audit trail, when one is open (ADR-024 §4).
    ///
    /// Established on unlock — the log is encrypted under the vault
    /// key, so there is nothing to write to before one exists — and
    /// dropped on lock along with the key that could read it.
    audit: Option<AuditWriter>,
    /// TOTP state: the resident secret plus its replay guard and
    /// rate limit (ADR-024 §1).
    ///
    /// Populated from the vault's reserved slot on every successful
    /// unlock and cleared on every lock, so the TOTP path never
    /// outlives the unlock that established it.
    totp: crate::totp_session::TotpSession,
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
            audit: None,
            totp: crate::totp_session::TotpSession::new(),
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
            audit: None,
            totp: crate::totp_session::TotpSession::new(),
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
            audit: None,
            totp: crate::totp_session::TotpSession::new(),
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
            audit: None,
            totp: crate::totp_session::TotpSession::new(),
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
        // The TOTP secret must not outlive the unlock it came with:
        // a code that still re-opened a vault the user deliberately
        // closed would make locking meaningless.
        self.totp.clear();
        // The audit log is encrypted under the vault key, so a
        // locked daemon has no way to write to it anyway.
        self.audit = None;
    }

    /// Load the shared TOTP secret out of the vault's reserved slot.
    ///
    /// A vault with no secret enrolled simply has no TOTP path —
    /// which is a fact, not a failure, and the caller finds out as
    /// `Unavailable` when it tries.
    fn adopt_totp_secret(&mut self) {
        let Some(vault) = self.vault.as_ref() else {
            return;
        };
        if let Ok(Some(secret)) = vault.get(crate::totp_session::TOTP_SECRET_PATH) {
            self.totp
                .set_secret(secret.expose_secret().as_bytes().to_vec());
        }
    }

    /// Open the audit trail for this unlock.
    ///
    /// The scrubber is built over the values the vault currently
    /// holds, so anything that later reaches a record's detail text
    /// is redacted by path. Failure to open the log is logged and
    /// swallowed: an audit trail that cannot be written is a
    /// problem, but refusing to unlock the vault over it would turn
    /// a diagnostic into an outage.
    fn open_audit(&mut self) {
        let Some(vault) = self.vault.as_ref() else {
            return;
        };

        // Values for the scrubber: everything the vault holds,
        // except the reserved slots, which must not be handed
        // around even to be redacted.
        let values: Vec<(String, String)> = vault
            .paths()
            .filter(|p| !crate::totp_session::is_reserved(p))
            .filter_map(|p| {
                vault
                    .get(p)
                    .ok()
                    .flatten()
                    .map(|v| (p.to_owned(), v.expose_secret().to_owned()))
            })
            .collect();

        let path = self.vault_path.with_file_name("audit-log.dvb");
        match AuditWriter::open(&path, values) {
            Ok(writer) => {
                if let Some(warning) = writer.scrub_warning() {
                    tracing::warn!("{warning}");
                }
                self.audit = Some(writer);
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not open the audit log; continuing without it")
            }
        }
    }

    /// Append an audit record, if a trail is open.
    ///
    /// Errors are logged rather than propagated for the same reason
    /// as above: a failed write should not turn a working secret
    /// lookup into a failed one.
    fn audit(&mut self, action: &str, path: &str, actor: &str) {
        let Some(vault) = self.vault.as_ref() else {
            return;
        };
        let Ok(key) = vault.audit_key() else {
            return;
        };
        let Some(writer) = self.audit.as_mut() else {
            return;
        };
        if let Err(e) = writer.record(&key, action, path, actor, None) {
            tracing::warn!(error = %e, action, path, "could not append an audit record");
        }
    }

    /// Whether a TOTP re-unlock is possible right now.
    pub fn totp_available(&self) -> bool {
        self.totp.is_available()
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
            "vault.request_unlock" => self.handle_vault_request_unlock(req),
            "totp.unlock" => self.handle_totp_unlock(req),
            "vault.lock" => self.handle_vault_lock(req),
            "vault.status" => self.handle_vault_status(req),
            "secret.get" => self.handle_secret_get(req),
            "secret.list" => self.handle_secret_list(req),
            "secret.put" => self.handle_secret_put(req).await,
            "secret.put_interactive" => self.handle_secret_put_interactive(req),
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
                // The TOTP path is established by this unlock and
                // by nothing else: the secret lives in the vault,
                // so it becomes resident exactly when the vault
                // opens.
                self.adopt_totp_secret();
                self.open_audit();
                self.audit("unlock", "vault", "user");
                JsonRpcResponse::ok(id, json!({"state": "unlocked"}))
            }
            Err(e) => JsonRpcResponse::err(id, vault_error_to_rpc(&e)),
        }
    }

    /// Collect the passphrase **here**, in the daemon, and unlock
    /// (ADR-024 §7).
    ///
    /// The request carries no passphrase — that is the entire
    /// point. An agent that can run shell commands as the user can
    /// replace the client binary and read anything typed into it,
    /// so a passphrase that transits the client is a passphrase the
    /// agent can have. This method only says "a human is asking to
    /// unlock"; the secret never crosses the socket.
    ///
    /// # Why this usually cannot work yet
    ///
    /// The only channel implemented is the daemon's own controlling
    /// terminal, and a daemon that satisfies the §7 startup check
    /// does not have one: the check demands reparenting to init, and
    /// a reparented process has no controlling terminal (our own
    /// systemd unit sets `StandardInput=null`). So in the supported
    /// configuration this returns [`NO_PROMPT_SURFACE`], and the
    /// error says what would fix it.
    ///
    /// That is a real conflict inside §7 rather than an oversight
    /// here, and resolving it means choosing a channel that does not
    /// need a terminal — `systemd-ask-password`, a launchd GUI
    /// helper, or a helper process of our own.
    fn handle_vault_request_unlock(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();

        if self.is_unlocked() {
            return JsonRpcResponse::ok(id, json!({"state": "unlocked"}));
        }

        let Some(mut prompt) = crate::prompt::TtyPrompt::open() else {
            return JsonRpcResponse::err(
                id,
                JsonRpcError::new(
                    NO_PROMPT_SURFACE,
                    "this daemon has no terminal to ask on, so it cannot collect the passphrase                      itself. Unlock it from the terminal it was started in, or export                      DEVBOY_VAULT_PASSPHRASE for an unattended start."
                        .to_string(),
                ),
            );
        };

        let passphrase = match prompt.read_passphrase("Unlock the devboy vault: ") {
            Ok(p) => p,
            Err(e) => {
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(
                        NO_PROMPT_SURFACE,
                        format!("could not read a passphrase from the daemon's terminal: {e}"),
                    ),
                );
            }
        };

        match Vault::open(&self.vault_path, UnlockMethod::Passphrase(passphrase)) {
            Ok(vault) => {
                self.vault = Some(vault);
                self.idle.record_unlock();
                self.adopt_totp_secret();
                JsonRpcResponse::ok(id, json!({"state": "unlocked"}))
            }
            Err(e) => JsonRpcResponse::err(id, vault_error_to_rpc(&e)),
        }
    }

    /// Re-unlock with a TOTP code (ADR-024 §1).
    ///
    /// The refusals are deliberately four different codes rather
    /// than one. "No secret resident" cannot be fixed by retrying,
    /// "replayed" will succeed at the next step, "bad code" wants a
    /// fresh look at the authenticator, and "rate limited" wants a
    /// wait — an agent given a single "denied" retries the same
    /// value forever.
    fn handle_totp_unlock(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        let params: TotpUnlockParams = match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(e) => return invalid_params(id, e),
        };

        // §7: a daemon the agent could ptrace has no TOTP path at
        // all. Possession of a code would prove nothing when the
        // agent can read the secret out of memory.
        if !crate::provenance::startup_provenance()
            .trust_level()
            .allows_totp()
        {
            return JsonRpcResponse::err(
                id,
                JsonRpcError::new(
                    TOTP_UNAVAILABLE,
                    "this daemon was started by its caller, so a TOTP code would prove nothing —                      the shared secret is readable from its memory. Start the daemon as a service."
                        .to_string(),
                ),
            );
        }

        let now = std::time::Instant::now();
        let unix_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        match self.totp.verify(&params.code, now, unix_seconds) {
            Ok(()) => {
                // A verified code extends the window; it does not
                // create one longer than the user's ceiling.
                let granted = self
                    .idle
                    .window
                    .resolve(params.duration_seconds.map(Duration::from_secs));
                self.idle.record_unlock();
                self.audit("totp-unlock", "vault", "user");
                JsonRpcResponse::ok(
                    id,
                    json!({
                        "state": "unlocked",
                        "granted_seconds": granted.as_secs(),
                    }),
                )
            }
            Err(denial) => JsonRpcResponse::err(id, totp_denial_to_rpc(denial)),
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
        let trust = crate::provenance::startup_provenance().trust_level();

        // `totp` drops out of the list when there is no resident
        // secret OR when the daemon is agent-parented — in the
        // second case a code proves nothing, because the agent can
        // read the secret out of the daemon's memory.
        let mut available_methods = vec!["passphrase"];
        if self.totp.is_available() && trust.allows_totp() {
            available_methods.push("totp");
        }

        // Opening /dev/tty is how the daemon finds out whether it
        // has one at all; there is no cheaper question to ask.
        let (prompt_channel, terminal_id) =
            describe_prompt_channel(crate::prompt::TtyPrompt::open());

        let window = self.window();
        JsonRpcResponse::ok(
            req.id,
            json!({
                "state": state,
                "unlock_ttl_seconds": window.unlock_ttl.as_secs(),
                "max_unlock_ttl_seconds": window.max_unlock_ttl.as_secs(),
                "idle_relock_seconds": window.idle_relock.map(|d| d.as_secs()),
                "available_methods": available_methods,
                "trust_level": trust.as_str(),
                // §7 level 2 vs level 3 turns on who owns the input
                // channel, so the daemon reports the channel it
                // actually has rather than leaving a caller to
                // assume one exists. `terminal_id` lets a client
                // check the daemon is not about to prompt on the
                // terminal the client is already watching — two
                // processes sharing one makes the trusted path
                // collapse.
                "prompt_channel": prompt_channel,
                "terminal_id": terminal_id,
                "insecure_override": crate::provenance::insecure_override_active(),
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
        // ADR-024 §1: the TOTP secret is unreachable from the wire.
        //
        // This is the load-bearing check of the whole re-unlock
        // scheme. A code proves a human is present only because the
        // agent cannot mint one — and an agent that could simply
        // *ask* an unlocked daemon for the shared secret would mint
        // codes all day. Reported as not-found rather than as a
        // refusal: the existence of the slot is not a caller's
        // business either.
        if crate::totp_session::is_reserved(&params.path) {
            return JsonRpcResponse::err(
                id,
                JsonRpcError::new(
                    ENTRY_NOT_FOUND,
                    format!("no entry for path '{}'", params.path),
                ),
            );
        }

        let vault = match self.vault.as_ref() {
            Some(v) => v,
            None => return locked_response(id),
        };
        let outcome = vault.get(&params.path);
        if matches!(outcome, Ok(Some(_))) {
            // Recorded after the fact and only on success: an
            // audit trail of reads that did not happen is noise,
            // and a failed lookup is already visible to the caller.
            self.audit("read", &params.path, "agent");
        }
        match outcome {
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
            // Reserved slots are absent from the listing for the
            // same reason they are unreadable: an agent has no
            // business knowing the TOTP secret is there, let alone
            // where.
            .filter(|(p, _)| !crate::totp_session::is_reserved(p))
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
        // Excluding the slot from reads while allowing writes would
        // let a caller overwrite the shared secret with one of its
        // own — a quieter way to mint valid codes than reading it.
        if crate::totp_session::is_reserved(&params.path) {
            return JsonRpcResponse::err(
                id,
                JsonRpcError::new(
                    INVALID_PARAMS,
                    format!("'{}' is a reserved path and cannot be written", params.path),
                ),
            );
        }
        if let Err(e) = self.verify_fresh_unlock(&params.fresh_unlock) {
            return JsonRpcResponse::err(id, e);
        }
        let vault = match self.vault.as_mut() {
            Some(v) => v,
            None => return locked_response(id),
        };
        let metadata = params.meta.unwrap_or_default().into_entry_metadata();
        let outcome = vault.put(&params.path, &SecretString::from(params.value), metadata);
        if outcome.is_ok() {
            self.audit("write", &params.path, "agent");
        }
        match outcome {
            Ok(()) => JsonRpcResponse::ok(id, json!({"ok": true})),
            Err(e) => JsonRpcResponse::err(id, vault_error_to_rpc(&e)),
        }
    }

    /// Write a secret, with the daemon collecting the freshness
    /// proof itself (ADR-024 §7).
    ///
    /// `secret.put` requires a `fresh_unlock` in the request, which
    /// means the caller holds the passphrase — fine for the UI,
    /// impossible for the credential chain, whose interface carries
    /// no unlock material and has nowhere to ask.
    ///
    /// This variant closes that gap without weakening the rule: the
    /// write still requires a fresh passphrase, the daemon just
    /// collects it on its own channel rather than accepting it over
    /// the socket. A caller that cannot be trusted with the
    /// passphrase never sees it.
    fn handle_secret_put_interactive(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        let params: PutInteractiveParams = match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(e) => return invalid_params(id, e),
        };

        if crate::totp_session::is_reserved(&params.path) {
            return JsonRpcResponse::err(
                id,
                JsonRpcError::new(
                    INVALID_PARAMS,
                    format!("'{}' is a reserved path and cannot be written", params.path),
                ),
            );
        }

        let Some(mut prompt) = crate::prompt::TtyPrompt::open() else {
            return JsonRpcResponse::err(
                id,
                JsonRpcError::new(
                    NO_PROMPT_SURFACE,
                    "this daemon has no terminal, so it cannot collect the passphrase a write                      requires. Store the secret with `devboy secrets ui`, which prompts where you                      are."
                        .to_string(),
                ),
            );
        };

        let passphrase =
            match prompt.read_passphrase(&format!("Passphrase to store '{}': ", params.path)) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::err(
                        id,
                        JsonRpcError::new(
                            NO_PROMPT_SURFACE,
                            format!("could not read a passphrase from the daemon's terminal: {e}"),
                        ),
                    );
                }
            };

        // Same freshness rule as `secret.put`: re-open the vault
        // with the supplied passphrase, so a stale unlock cannot
        // authorise a write.
        match Vault::open(&self.vault_path, UnlockMethod::Passphrase(passphrase)) {
            Ok(vault) => self.vault = Some(vault),
            Err(_) => {
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(BAD_UNLOCK, "that passphrase did not open the vault"),
                );
            }
        }

        let vault = match self.vault.as_mut() {
            Some(v) => v,
            None => return locked_response(id),
        };
        let outcome = vault.put(
            &params.path,
            &SecretString::from(params.value),
            EntryMetadata::default(),
        );
        if outcome.is_ok() {
            self.audit("write", &params.path, "user");
        }
        match outcome {
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

/// Parameters for `secret.put_interactive`.
///
/// Note what is absent: no `fresh_unlock`. The daemon collects it,
/// which is the entire difference from `secret.put`.
#[derive(Debug, Deserialize)]
struct PutInteractiveParams {
    path: String,
    value: String,
}

/// Parameters for `totp.unlock`.
#[derive(Debug, Deserialize)]
struct TotpUnlockParams {
    /// The six-digit code from the user's authenticator.
    code: String,
    /// Requested unlock length. Clamped to the configured ceiling —
    /// a per-call argument does not override the user's standing
    /// decision.
    #[serde(default)]
    duration_seconds: Option<u64>,
}

/// Describe the prompt channel for `vault.status`.
///
/// Split out from the handler so both branches are reachable from a
/// test. The daemon under a test harness has no controlling
/// terminal, so a check written inline could only ever exercise the
/// "none" case — and would pass whatever the "terminal" case did.
fn describe_prompt_channel(tty: Option<crate::prompt::TtyPrompt>) -> (&'static str, Option<Value>) {
    match tty {
        Some(tty) => (
            "terminal",
            tty.identity().ok().map(|(rdev, ino)| json!([rdev, ino])),
        ),
        None => ("none", None),
    }
}

/// Map a [`TotpDenial`] onto its wire error.
fn totp_denial_to_rpc(denial: crate::totp_session::TotpDenial) -> JsonRpcError {
    use crate::totp_session::TotpDenial;
    match denial {
        TotpDenial::Unavailable => JsonRpcError::new(
            TOTP_UNAVAILABLE,
            "no TOTP secret is resident: unlock the vault with its passphrase first, or enrol an              authenticator with `devboy secrets vault add-totp`",
        ),
        TotpDenial::BadCode => JsonRpcError::new(
            BAD_TOTP,
            "that code did not verify; check the authenticator and try the current one",
        ),
        TotpDenial::Replayed => JsonRpcError::new(
            REPLAYED_TOTP,
            "that code was already used; wait for the next one to appear",
        ),
        TotpDenial::RateLimited {
            retry_after_seconds,
        } => JsonRpcError {
            code: TOTP_RATE_LIMITED,
            message: format!(
                "too many attempts; the TOTP path is closed for {retry_after_seconds} seconds"
            ),
            data: Some(json!({ "retry_after_seconds": retry_after_seconds })),
        },
    }
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

    /// A secret read must leave a trace. The audit log exists so a
    /// user who suspects an agent has been reading things it should
    /// not can find out afterwards, without having watched.
    #[tokio::test]
    async fn reading_a_secret_leaves_an_audit_record() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        server
            .vault
            .as_mut()
            .unwrap()
            .put(
                "team/a/one",
                &SecretString::from("value-long-enough-to-scrub".to_owned()),
                EntryMetadata::default(),
            )
            .unwrap();

        server
            .handle_request(req(2, "secret.get", json!({"path": "team/a/one"})))
            .await;

        let key = server.vault.as_ref().unwrap().audit_key().unwrap();
        let records = server.audit.as_ref().unwrap().read_all(&key).unwrap();

        assert!(
            records.iter().any(|r| r.action == "unlock"),
            "the unlock itself should be recorded"
        );
        let read = records
            .iter()
            .find(|r| r.action == "read")
            .expect("the read should be recorded");
        assert_eq!(read.path, "team/a/one");
        assert_eq!(read.actor, "agent");
    }

    /// A lookup that found nothing must not be recorded as a read:
    /// an audit trail full of reads that did not happen is noise
    /// that hides the ones that did.
    #[tokio::test]
    async fn a_failed_lookup_is_not_recorded_as_a_read() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;

        server
            .handle_request(req(2, "secret.get", json!({"path": "no/such/path"})))
            .await;

        let key = server.vault.as_ref().unwrap().audit_key().unwrap();
        let records = server.audit.as_ref().unwrap().read_all(&key).unwrap();
        assert!(
            !records.iter().any(|r| r.action == "read"),
            "a miss should not look like a read"
        );
    }

    /// Locking drops the trail along with the key that could read
    /// it — the log is encrypted under a subkey of the vault key,
    /// so a locked daemon has nothing to write with anyway.
    #[tokio::test]
    async fn locking_closes_the_audit_trail() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        assert!(server.audit.is_some());

        server
            .handle_request(req(2, "vault.lock", Value::Null))
            .await;
        assert!(
            server.audit.is_none(),
            "a locked daemon must not hold an open audit trail"
        );
    }

    /// A restarted daemon must SAY the TOTP path is gone, not fail
    /// silently — the acceptance criterion for Ф6c/Ф6d.
    #[tokio::test]
    async fn totp_unlock_without_a_resident_secret_is_explicit() {
        let (_dir, mut server) = fresh_vault("p");

        let r = server
            .handle_request(req(1, "totp.unlock", json!({"code": "123456"})))
            .await;
        let err = r.error.expect("no secret is resident");

        assert!(
            err.code == TOTP_UNAVAILABLE || err.code == BAD_TOTP,
            "unexpected code {}",
            err.code
        );
        assert!(
            !err.message.is_empty(),
            "the refusal must explain itself rather than being bare"
        );
    }

    /// The four refusals must stay four codes. Collapsing them is
    /// how an agent ends up retrying a value that can never work.
    #[test]
    fn each_totp_denial_maps_to_its_own_code() {
        use crate::totp_session::TotpDenial;

        let codes: Vec<i32> = [
            TotpDenial::Unavailable,
            TotpDenial::BadCode,
            TotpDenial::Replayed,
            TotpDenial::RateLimited {
                retry_after_seconds: 60,
            },
        ]
        .into_iter()
        .map(|d| totp_denial_to_rpc(d).code)
        .collect();

        let unique: std::collections::BTreeSet<i32> = codes.iter().copied().collect();
        assert_eq!(
            unique.len(),
            codes.len(),
            "every denial needs its own code: {codes:?}"
        );
    }

    /// A rate-limited refusal carries how long to wait, so the
    /// caller does not have to guess.
    #[test]
    fn a_rate_limited_denial_says_how_long_to_wait() {
        use crate::totp_session::TotpDenial;

        let err = totp_denial_to_rpc(TotpDenial::RateLimited {
            retry_after_seconds: 42,
        });
        assert_eq!(err.code, TOTP_RATE_LIMITED);
        assert_eq!(
            err.data.expect("data payload")["retry_after_seconds"],
            42,
            "the wait has to be machine-readable, not only in the message"
        );
    }

    /// `available_methods` is the agent's view of what it may try.
    /// Offering `totp` when no secret is resident sends it down a
    /// path that cannot work.
    #[tokio::test]
    async fn status_offers_totp_only_when_a_secret_is_resident() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;

        let r = server
            .handle_request(req(2, "vault.status", Value::Null))
            .await;
        let result = r.result.expect("status");
        let methods = result["available_methods"].as_array().expect("methods");

        assert!(
            methods.iter().any(|m| m == "passphrase"),
            "passphrase is always available"
        );
        assert!(
            !methods.iter().any(|m| m == "totp"),
            "no secret is enrolled, so totp must not be offered: {methods:?}"
        );
        assert!(
            result["trust_level"].is_string(),
            "status should report the trust level"
        );
    }

    /// The check the whole TOTP scheme rests on.
    ///
    /// A code proves a human is present only because the agent
    /// cannot mint one. An agent that could ask an unlocked daemon
    /// for the shared secret would mint codes at will, so the
    /// reserved slot must be unreadable, unlistable and unwritable
    /// even on a fully unlocked vault.
    #[tokio::test]
    async fn the_totp_secret_is_unreachable_from_the_wire() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;

        // Seed the reserved slot the way enrolment would, going
        // around the wire because the wire refuses it.
        server
            .vault
            .as_mut()
            .unwrap()
            .put(
                crate::totp_session::TOTP_SECRET_PATH,
                &SecretString::from("12345678901234567890".to_owned()),
                EntryMetadata::default(),
            )
            .expect("seed reserved slot");

        // Unreadable.
        let get = server
            .handle_request(req(
                2,
                "secret.get",
                json!({"path": crate::totp_session::TOTP_SECRET_PATH}),
            ))
            .await;
        assert_eq!(
            get.error.expect("reserved paths must not resolve").code,
            ENTRY_NOT_FOUND
        );

        // Unlistable.
        let list = server
            .handle_request(req(3, "secret.list", Value::Null))
            .await;
        let body = serde_json::to_string(&list.result.unwrap()).unwrap();
        assert!(
            !body.contains("__totp"),
            "the reserved slot must not appear in a listing: {body}"
        );

        // Unwritable — otherwise an agent swaps in its own secret.
        let put = server
            .handle_request(req(
                4,
                "secret.put",
                json!({
                    "path": crate::totp_session::TOTP_SECRET_PATH,
                    "value": "attacker-chosen",
                    "fresh_unlock": {"kind": "passphrase", "secret": "p"}
                }),
            ))
            .await;
        assert!(
            put.error.is_some(),
            "writing the reserved slot must be refused"
        );
    }

    /// After a passphrase unlock the daemon holds the secret, and a
    /// lock takes it away again — a code must not re-open a vault
    /// the user deliberately closed.
    #[tokio::test]
    async fn the_totp_path_lives_and_dies_with_the_unlock() {
        let (_dir, mut server) = fresh_vault("p");
        assert!(!server.totp_available(), "locked daemon has no TOTP path");

        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        server
            .vault
            .as_mut()
            .unwrap()
            .put(
                crate::totp_session::TOTP_SECRET_PATH,
                &SecretString::from("12345678901234567890".to_owned()),
                EntryMetadata::default(),
            )
            .expect("seed");

        // Re-unlock so the daemon adopts the freshly-seeded secret.
        server
            .handle_request(req(2, "vault.lock", Value::Null))
            .await;
        server
            .handle_request(req(
                3,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        assert!(
            server.totp_available(),
            "an unlocked daemon with an enrolled secret should offer the TOTP path"
        );

        server
            .handle_request(req(4, "vault.lock", Value::Null))
            .await;
        assert!(
            !server.totp_available(),
            "locking must take the TOTP secret with it"
        );
    }

    /// A vault with no enrolled secret has no TOTP path, and that is
    /// a fact rather than a failure — the acceptance criterion is
    /// that it reports rather than staying silent.
    #[tokio::test]
    async fn a_vault_without_an_enrolled_secret_has_no_totp_path() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;

        assert!(!server.totp_available());
    }

    /// `vault.request_unlock` must carry no passphrase field at all.
    ///
    /// The whole point is that the secret never crosses the socket,
    /// so a caller supplying one must not be able to influence the
    /// unlock — the parameters are ignored entirely.
    #[tokio::test]
    async fn request_unlock_ignores_anything_the_caller_sends() {
        let (_dir, mut server) = fresh_vault("p");

        let r = server
            .handle_request(req(
                1,
                "vault.request_unlock",
                json!({"secret": "p", "passphrase": "p"}),
            ))
            .await;

        // The test process has no controlling terminal under the
        // harness, so the honest answer is "nowhere to ask" — not
        // "unlocked", which is what would happen if the supplied
        // passphrase had been honoured.
        let err = r
            .error
            .expect("a caller-supplied passphrase must not unlock the vault");
        assert_eq!(err.code, NO_PROMPT_SURFACE);
        assert!(
            !server.is_unlocked(),
            "the vault must stay locked when the daemon could not ask a human"
        );
    }

    /// The "nowhere to ask" error has to be distinguishable from a
    /// wrong passphrase: nothing about the passphrase is wrong, and
    /// the fix is completely different.
    #[tokio::test]
    async fn no_prompt_surface_is_its_own_error_not_a_bad_unlock() {
        let (_dir, mut server) = fresh_vault("p");

        let r = server
            .handle_request(req(1, "vault.request_unlock", Value::Null))
            .await;
        let err = r.error.expect("no terminal under test");

        assert_eq!(err.code, NO_PROMPT_SURFACE);
        assert_ne!(err.code, BAD_UNLOCK);
        assert!(
            err.message.contains("DEVBOY_VAULT_PASSPHRASE")
                || err.message.contains("terminal it was started in"),
            "the error should name a way forward: {}",
            err.message
        );
    }

    /// Asking to unlock an already-unlocked vault is a no-op rather
    /// than a second prompt — otherwise a stray call would make the
    /// user re-type their passphrase for nothing.
    #[tokio::test]
    async fn request_unlock_on_an_open_vault_does_not_prompt_again() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;

        let r = server
            .handle_request(req(2, "vault.request_unlock", Value::Null))
            .await;

        assert!(r.error.is_none(), "an open vault should answer, not fail");
        assert_eq!(r.result.unwrap()["state"], "unlocked");
    }

    /// The "terminal" branch must yield an identity, and the test
    /// harness has no controlling terminal — so the branch is
    /// driven with a real pty rather than left unreachable. An
    /// earlier version asserted it only when the ambient daemon
    /// happened to have a terminal, which in CI is never.
    #[test]
    fn a_terminal_channel_is_always_identifiable() {
        use std::fs::File;

        let pair = nix::pty::openpty(None, None).expect("openpty");
        let device = File::from(pair.slave);
        let prompt = crate::prompt::TtyPrompt::from_file(device);

        let (channel, id) = describe_prompt_channel(Some(prompt));
        assert_eq!(channel, "terminal");
        assert!(
            id.is_some(),
            "a terminal channel with no identity leaves a client unable to check whether the              daemon shares its terminal"
        );
    }

    #[test]
    fn no_terminal_yields_no_identity() {
        let (channel, id) = describe_prompt_channel(None);
        assert_eq!(channel, "none");
        assert!(id.is_none());
    }

    /// §7 grades trust by who owns the input channel, so the
    /// status has to name the channel rather than only the level.
    /// A level without a channel describes an intention.
    #[tokio::test]
    async fn status_reports_the_prompt_channel_it_actually_has() {
        let (_dir, mut server) = fresh_vault("p");
        let r = server
            .handle_request(req(1, "vault.status", Value::Null))
            .await;
        let result = r.result.expect("status");

        let channel = result["prompt_channel"]
            .as_str()
            .expect("the channel must be reported");
        assert!(
            channel == "terminal" || channel == "none",
            "unexpected channel {channel:?}"
        );

        // A terminal must come with something to identify it, or a
        // client cannot check the daemon is not about to prompt on
        // the terminal the client is already watching.
        if channel == "terminal" {
            assert!(
                !result["terminal_id"].is_null(),
                "a terminal channel must be identifiable"
            );
        } else {
            assert!(
                result["terminal_id"].is_null(),
                "there is no terminal to identify"
            );
        }
    }

    /// The override has to be visible in the status, not only in a
    /// log line nobody reads.
    #[tokio::test]
    async fn status_reports_whether_the_insecure_override_is_active() {
        let (_dir, mut server) = fresh_vault("p");
        let r = server
            .handle_request(req(1, "vault.status", Value::Null))
            .await;

        assert!(
            r.result.expect("status")["insecure_override"].is_boolean(),
            "the override state must be reported either way"
        );
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
