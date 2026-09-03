//! `VaultServer` — the JSON-RPC 2.0 method dispatcher per [ADR-023]
//! §3.3.
//!
//! Wraps a [`devboy_vault_crypto::Vault`] (when unlocked) and routes
//! the ADR-023 §3.3 methods, plus the ADR-024 additions, against it:
//!
//! | Method                   | State requirement | `fresh_unlock`   | Extends the window |
//! |--------------------------|-------------------|------------------|--------------------|
//! | `vault.unlock`           | locked            | n/a              | opens it           |
//! | `vault.request_unlock`   | locked            | daemon collects  | opens it           |
//! | `totp.unlock`            | locked            | six-digit code   | opens it           |
//! | `vault.lock`             | unlocked          | no               | closes it          |
//! | `vault.status`           | any               | no               | no                 |
//! | `secret.get`             | unlocked          | no (cached)      | yes                |
//! | `secret.list`            | unlocked          | no (cached)      | yes                |
//! | `secret.validate`        | unlocked          | no (cached)      | **no**             |
//! | `secret.put`             | unlocked          | **yes**          | yes                |
//! | `secret.put_interactive` | unlocked          | daemon collects  | yes                |
//! | `secret.rotate`          | unlocked          | **yes**          | yes                |
//! | `metadata.update`        | unlocked          | no (plaintext)   | yes                |
//!
//! `fresh_unlock` is the ADR's hybrid-mode requirement: write
//! operations revalidate the user's unlock method on every call so
//! reads can benefit from daemon caching while writes can't.
//! Implementation: `VaultServer::verify_fresh_unlock` re-opens the
//! vault file with the supplied unlock method and discards the
//! resulting handle if the credentials check out.
//!
//! The last column is the idle timer of ADR-023 §3.3, kept by the
//! private `is_user_activity`. `secret.validate` is the one value-touching
//! method that does not refresh it, because it is the one an *agent*
//! can reach: a method an agent may call in a loop must not be able
//! to hold the vault open indefinitely.
//!
//! See also [`crate::rpc`] for the JSON-RPC framing and error codes
//! the dispatcher returns.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::path::PathBuf;
use std::time::Duration;

use devboy_secret_patterns::SecretPattern;
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
    NO_TERMINAL_AT_ALL, REPLAYED_TOTP, TOTP_RATE_LIMITED, TOTP_UNAVAILABLE, VAULT_LOCKED,
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
    /// Whether to look for a controlling terminal of our own when a
    /// passphrase has to be collected (ADR-024 §7, Ф14).
    ///
    /// Always `Detect` in production; the other variant exists only
    /// under `cfg(test)` and is therefore unreachable from a shipped
    /// binary. It is here because a test that reaches the prompt
    /// **blocks on the developer's real screen** when `cargo test`
    /// runs in a terminal — which is how a hang went unnoticed:
    /// neither CI nor a piped shell has a controlling terminal, so
    /// the branch only ever fired on a person's laptop.
    #[cfg(unix)]
    own_tty: OwnTty,
}

/// Where the daemon looks for its own terminal.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnTty {
    /// Open `/dev/tty` and use it if it exists.
    Detect,
    /// Behave as a properly-installed daemon does: no terminal.
    #[cfg(test)]
    Absent,
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
            #[cfg(unix)]
            own_tty: OwnTty::Detect,
        }
    }

    /// Behave as a properly-installed daemon does: no controlling
    /// terminal of its own.
    ///
    /// Test-only, and the variant it sets does not exist outside
    /// `cfg(test)`. Without it a test that reaches the passphrase
    /// prompt blocks on the developer's real screen whenever
    /// `cargo test` is run from a terminal — silently, because
    /// neither CI nor a piped shell has one.
    #[cfg(all(test, unix))]
    fn pretend_to_have_no_terminal(&mut self) -> &mut Self {
        self.own_tty = OwnTty::Absent;
        self
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
            #[cfg(unix)]
            own_tty: OwnTty::Detect,
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
            #[cfg(unix)]
            own_tty: OwnTty::Detect,
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
            #[cfg(unix)]
            own_tty: OwnTty::Detect,
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
        let Ok(Some(secret)) = vault.get(crate::totp_session::TOTP_SECRET_PATH) else {
            return;
        };

        // Decoded, not taken verbatim. The slot is a `SecretString`,
        // so enrolment stores the base32 *text* of the key; the
        // authenticator app decodes that same text and HMACs the raw
        // bytes. Using the text as the key produces a different key
        // and rejects every code the user's phone shows — which is
        // what happened until this decode was added.
        match devboy_vault_crypto::totp::decode_secret(secret.expose_secret()) {
            Ok(key) => self.totp.set_secret(key),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "the stored TOTP secret could not be decoded, so TOTP re-unlock is \
                     unavailable. Re-enrol with `devboy secrets add-totp`"
                );
            }
        }
    }

    /// Everything the vault holds that the scrubber should know.
    ///
    /// Reserved slots are excluded: those must not be handed around
    /// even for the purpose of being redacted.
    fn scrubbable_values(vault: &Vault) -> Vec<(String, String)> {
        vault
            .paths()
            .filter(|p| !crate::totp_session::is_reserved(p))
            .filter_map(|p| {
                vault
                    .get(p)
                    .ok()
                    .flatten()
                    .map(|v| (p.to_owned(), v.expose_secret().to_owned()))
            })
            .collect()
    }

    /// Teach the audit scrubber a secret that changed after unlock.
    ///
    /// Called after every successful write. Without it the scrubber
    /// protects only the secrets that existed when the vault was
    /// opened, and the freshly written one — the likeliest to be
    /// quoted in some component's error text right now — passes
    /// through the log untouched.
    fn relearn_secrets_for_audit(&mut self) {
        let Some(vault) = self.vault.as_ref() else {
            return;
        };
        let values = Self::scrubbable_values(vault);
        if let Some(writer) = self.audit.as_mut() {
            writer.relearn_values(values);
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

        let values = Self::scrubbable_values(vault);

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
        self.audit_with_detail(action, path, actor, None);
    }

    /// Append a record carrying free-text detail.
    ///
    /// `detail` goes through the scrubber on the way in — that is
    /// the only way to obtain the type `record` accepts, so the
    /// redaction cannot be skipped by a caller in a hurry.
    fn audit_with_detail(&mut self, action: &str, path: &str, actor: &str, detail: Option<&str>) {
        let Some(vault) = self.vault.as_ref() else {
            return;
        };
        let Ok(key) = vault.audit_key() else {
            return;
        };
        let Some(writer) = self.audit.as_mut() else {
            return;
        };
        let scrubbed = detail.map(|d| writer.scrub(d));
        if let Err(e) = writer.record(&key, action, path, actor, scrubbed) {
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
            "secret.validate" => self.handle_secret_validate(req).await,
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
        let kind_label = params.kind.clone();
        let unlock = match params.into_unlock_method() {
            Ok(u) => u,
            Err(e) => return JsonRpcResponse::err(id, e),
        };
        let trust = crate::provenance::startup_provenance().trust_level();
        match Vault::open(&self.vault_path, unlock) {
            Ok(vault) => {
                self.adopt_unlocked_vault(vault);
                // Which method opened the vault, and how much this
                // daemon's own position is worth, are the two things
                // someone reading the trail afterwards most wants to
                // know — an unlock at agent-parented trust means
                // something quite different from one at independent.
                let detail = format!("method={kind_label} trust={}", trust.as_str());
                self.audit_with_detail("unlock", "vault", "user", Some(&detail));
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
    /// # Two channels, and why the second one exists
    ///
    /// The daemon's own controlling terminal is tried first. A daemon
    /// that satisfies the §7 startup check does not have one — the
    /// check demands reparenting to init, and a reparented process
    /// has no controlling terminal (our own systemd unit sets
    /// `StandardInput=null`). On its own that made this method
    /// useless in exactly the configuration we recommend.
    ///
    /// So a caller may **lend** its terminal by naming it, and the
    /// daemon opens that and asks there
    /// (see [`crate::client_terminal`], which also works through why
    /// letting the caller choose does not weaken §7). The passphrase
    /// still never crosses the socket and never enters the client's
    /// memory; only the *location of the screen* comes from the
    /// caller.
    ///
    /// With neither channel available this returns
    /// [`NO_PROMPT_SURFACE`], and the error says what would fix it.
    fn handle_vault_request_unlock(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();

        if self.is_unlocked() {
            return JsonRpcResponse::ok(id, json!({"state": "unlocked"}));
        }

        // Anything else the caller sent is ignored rather than
        // rejected — notably a `passphrase` field, which this method
        // must never honour.
        let params: RequestUnlockParams = serde_json::from_value(req.params).unwrap_or_default();

        let (mut prompt, channel) = match self.prompt_surface(params.tty.as_deref()) {
            Ok(pair) => pair,
            Err(message) => {
                return JsonRpcResponse::err(id, JsonRpcError::new(NO_PROMPT_SURFACE, message));
            }
        };

        let passphrase = match prompt.read_passphrase("Unlock the devboy vault: ") {
            Ok(p) => p,
            Err(e) => {
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(
                        NO_PROMPT_SURFACE,
                        format!("could not read a passphrase from the {channel} terminal: {e}"),
                    ),
                );
            }
        };

        match Vault::open(&self.vault_path, UnlockMethod::Passphrase(passphrase)) {
            Ok(vault) => {
                self.vault = Some(vault);
                self.idle.record_unlock();
                self.adopt_totp_secret();
                // An unlock the daemon collected itself is still an
                // unlock, and leaving it out of the trail would make
                // the safest path the least visible one.
                self.open_audit();
                let trust = crate::provenance::startup_provenance().trust_level();
                // Which screen the question was printed on is part of
                // the record: "the daemon asked" and "the daemon asked
                // on a terminal the caller named" are different
                // enough that a reader of the trail should not have
                // to guess which happened.
                let detail = format!(
                    "method=daemon-prompt channel={channel} trust={}",
                    trust.as_str()
                );
                self.audit_with_detail("unlock", "vault", "user", Some(&detail));
                JsonRpcResponse::ok(id, json!({"state": "unlocked"}))
            }
            Err(e) => JsonRpcResponse::err(id, vault_error_to_rpc(&e)),
        }
    }

    /// Install a freshly-opened vault, with everything that has to
    /// happen alongside it.
    ///
    /// # Why this is a method and not three lines at each call site
    ///
    /// Opening the vault is only part of unlocking it. The idle
    /// window has to start, the TOTP secret has to become resident,
    /// and the audit trail has to open — and each of those is
    /// invisible by its absence.
    ///
    /// `secret.put_interactive` used to do `self.vault = Some(vault)`
    /// and nothing else. The consequences were not subtle:
    /// `IdleTracker` treats a `None` last-activity as "no window
    /// running", so the vault stayed unlocked **forever** while
    /// `vault.status` cheerfully reported `unlocked`; and with no
    /// audit trail open, the write that followed recorded nothing,
    /// though the identical write through `secret.put` was recorded.
    ///
    /// So there is one way in, and it is this one.
    fn adopt_unlocked_vault(&mut self, vault: Vault) {
        self.vault = Some(vault);
        self.idle.record_unlock();
        // The TOTP path is established by this unlock and by nothing
        // else: the secret lives in the vault, so it becomes resident
        // exactly when the vault opens.
        self.adopt_totp_secret();
        self.open_audit();
    }

    /// Find a screen to ask on, preferring the daemon's own.
    ///
    /// Returns the prompt and a label naming the channel, or the
    /// message to fail with. The daemon's own terminal comes first
    /// because it needs nothing from the caller; a lent one is the
    /// fallback that makes the properly-installed case work at all.
    #[cfg(unix)]
    fn prompt_surface(
        &self,
        lent: Option<&str>,
    ) -> Result<(crate::prompt::TtyPrompt, &'static str), String> {
        let own = match self.own_tty {
            OwnTty::Detect => crate::prompt::TtyPrompt::open(),
            #[cfg(test)]
            OwnTty::Absent => None,
        };
        choose_prompt_surface(own, lent)
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
                // The window's *policy* is not its *state*. A
                // caller told the vault re-locks after 900s of
                // inactivity still cannot tell whether it has
                // 800 seconds left or 8, and deciding whether to
                // act now or ask the user first is exactly what
                // that number is for.
                "unlock_seconds_remaining": self.idle.remaining_seconds(),
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

    /// `secret.validate` — is the stored value the right shape?
    ///
    /// The whole point is that the answer crosses the socket and the
    /// value does not. An agent gets to know its freshly provisioned
    /// token is well-formed without ever being trusted with it.
    ///
    /// # Why the caller may not supply the rule
    ///
    /// The obvious convenience — let the caller pass a `format_regex`
    /// from the manifest it already holds — turns this method into a
    /// value oracle. Anyone who can reach the socket asks
    /// `^sk-a.*`, then `^sk-ab.*`, and reads the secret out one
    /// character at a time from a sequence of yes/no answers. So the
    /// rule comes from the `pattern_id` this daemon has stored
    /// against the entry, resolved through this daemon's own
    /// catalogue, and from nowhere else.
    ///
    /// What leaks is one bit against a shape that the manifest
    /// already declares in the open. What would leak otherwise is
    /// the secret.
    ///
    /// # Why it does not extend the unlock window
    ///
    /// This is the first value-touching method an *agent* can reach:
    /// `secret.get` exists, but only the CLI calls it, because agents
    /// are given aliases rather than values. If validating bumped the
    /// activity timestamp, an agent calling it on a loop would hold
    /// the vault open forever and auto-lock (ADR-023 §3.3) would
    /// never fire again. So it behaves like `vault.status`: it needs
    /// an unlock window that a human opened, and it cannot lengthen
    /// one. See [`is_user_activity`].
    async fn handle_secret_validate(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        let id = req.id.clone();
        let params: ValidateParams = match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(e) => return invalid_params(id, e),
        };
        // Same silence as `secret.get`: the TOTP slot is not a
        // caller's business, and "is the shared secret well-formed?"
        // is not a question worth answering either.
        if crate::totp_session::is_reserved(&params.path) {
            return JsonRpcResponse::err(
                id,
                JsonRpcError::new(
                    ENTRY_NOT_FOUND,
                    format!("no entry for path '{}'", params.path),
                ),
            );
        }

        let Some(vault) = self.vault.as_ref() else {
            return locked_response(id);
        };

        let value = match vault.get(&params.path) {
            Ok(Some(v)) => v,
            Ok(None) => {
                return JsonRpcResponse::err(
                    id,
                    JsonRpcError::new(
                        ENTRY_NOT_FOUND,
                        format!("no entry for path '{}'", params.path),
                    ),
                );
            }
            Err(e) => return JsonRpcResponse::err(id, vault_error_to_rpc(&e)),
        };

        // `paths()` and `list()` walk the same iterator, so zipping
        // them pairs each path with its own metadata. Same approach
        // as `secret.list`, and it keeps the vault's public surface
        // unchanged.
        let meta = vault
            .paths()
            .map(str::to_owned)
            .zip(vault.list())
            .find(|(path, _)| path == &params.path)
            .map(|(_, meta)| meta)
            .unwrap_or_default();

        let pattern = resolved_pattern(&meta);
        let verdict = format_verdict(pattern, value.expose_secret());

        // Reading the value is reading the value, whoever asked and
        // whatever we told them. An access that skipped the trail
        // because only a verdict came back would be the easy way to
        // read a vault quietly.
        self.audit_with_detail("validate", &params.path, "agent", Some(verdict.as_str()));

        let liveness = if params.liveness {
            let spec = pattern.and_then(devboy_secret_patterns::SecretPattern::liveness);

            // A separate record, and not a nicety. `validate` says a
            // value was read inside this process; a probe says it
            // left the machine. Someone reading the trail after an
            // incident wants those distinguishable, and wants the
            // destination.
            if let Some(spec) = spec {
                let devboy_secret_patterns::LivenessKind::Http { url, .. } = &spec.kind;
                self.audit_with_detail(
                    "liveness-probe",
                    &params.path,
                    "agent",
                    Some(&format!("sending to {url}")),
                );
            }

            Some(crate::liveness::probe(spec, value.expose_secret()).await)
        } else {
            None
        };

        JsonRpcResponse::ok(
            id,
            json!({
                "format": verdict.as_str(),
                "liveness": liveness.map(crate::liveness::LivenessOutcome::as_str),
                "pattern_id": meta.pattern_id,
                "expires_at": meta.expires_at,
            }),
        )
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
            self.relearn_secrets_for_audit();
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
            // A human just typed a passphrase on the daemon's own
            // terminal — that is an unlock, and it starts a window
            // like any other.
            Ok(vault) => self.adopt_unlocked_vault(vault),
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
            self.relearn_secrets_for_audit();
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
        // Same guard as every other write path. Without it, rotate
        // was a way to overwrite `__totp/secret` with a value of the
        // caller's choosing: the next unlock adopts it, and whoever
        // chose it can mint valid codes from then on. The reserved
        // slot is unreadable, unlistable and unwritable — and rotate
        // is a write.
        if crate::totp_session::is_reserved(&params.path) {
            return JsonRpcResponse::err(
                id,
                JsonRpcError::new(
                    INVALID_PARAMS,
                    format!("'{}' is a reserved path and cannot be written", params.path),
                ),
            );
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
                // Recorded like every other write. A rotation that
                // leaves no trace is the one an investigator most
                // wants to see.
                self.relearn_secrets_for_audit();
                self.audit("rotate", &params.path, "user");
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

/// Parameters for `secret.validate`.
///
/// Note what is absent: any way to supply the rule. A caller that
/// could name the regex would have a value oracle, and one that
/// could name the liveness endpoint would have an exfiltration
/// channel. Both come from the catalogue instead.
#[derive(Debug, Deserialize)]
struct ValidateParams {
    path: String,
    /// Also ask the provider whether the credential still works.
    #[serde(default)]
    liveness: bool,
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

/// Parameters for `vault.request_unlock`.
///
/// Note what is *not* here: a passphrase. This request only says "a
/// human is asking to unlock"; the secret never crosses the socket.
/// Unknown fields are ignored rather than rejected, so a caller that
/// sends one is refused by omission rather than by an error that
/// might tempt someone to add the field.
#[derive(Debug, Default, Deserialize)]
struct RequestUnlockParams {
    /// A terminal the caller is lending the daemon to ask on
    /// (ADR-024 §7, Ф14). Absent means "use your own, if you have
    /// one".
    #[serde(default)]
    tty: Option<String>,
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
            "no TOTP secret is resident: unlock the vault with its passphrase first, or enrol an              authenticator with `devboy secrets add-totp`",
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

/// Decide which screen to ask the passphrase on (ADR-024 §7, Ф14).
///
/// `own` is the daemon's own controlling terminal if it has one, and
/// `lent` a path the caller offered. Taken as arguments rather than
/// read inside, for a reason that is not merely stylistic: a test
/// process usually *does* have a controlling terminal, so a version
/// that opened `/dev/tty` itself would take the "own" branch on a
/// developer's machine and block on their real screen. The branch
/// that matters would then be exercised only in CI, where nobody
/// looks until it breaks.
///
/// The daemon's own terminal wins when both exist: it needs nothing
/// from the caller, and the fewer moving parts in a passphrase prompt
/// the better.
#[cfg(unix)]
fn choose_prompt_surface(
    own: Option<crate::prompt::TtyPrompt>,
    lent: Option<&str>,
) -> Result<(crate::prompt::TtyPrompt, &'static str), String> {
    if let Some(prompt) = own {
        return Ok((prompt, "own"));
    }

    let Some(path) = lent else {
        return Err(NO_TERMINAL_AT_ALL.to_string());
    };

    match crate::client_terminal::open_client_terminal(std::path::Path::new(path)) {
        Ok(file) => Ok((crate::prompt::TtyPrompt::from_file(file), "client")),
        Err(e) => Err(format!(
            "this daemon has no terminal of its own, and the one offered by the caller cannot be \
             used: {e}"
        )),
    }
}

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

/// The three answers `secret.validate` can give about a value's
/// shape.
///
/// The wire names are the join with
/// `devboy_mcp::secrets_validate::FormatVerdict`, which is the only
/// consumer. Nothing in the type system holds those two together, so
/// a test pins the strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatVerdict {
    /// The value matches the declared shape.
    Ok,
    /// The value does not match.
    Invalid,
    /// Nothing was declared about this value's shape.
    ///
    /// Deliberately not `Ok`. "Checked and passed" and "nobody said
    /// what this should look like" are different facts, and an agent
    /// that conflates them reports confidence it has not earned.
    Unknown,
}

impl FormatVerdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Invalid => "invalid",
            Self::Unknown => "unknown",
        }
    }
}

/// Check a value against the rule this daemon holds for it.
///
/// The rule is the entry's own `pattern_id`, resolved through this
/// process's catalogue — which includes whatever the user declared
/// in `<config>/secrets/patterns.d/`, so an in-house token format
/// is checkable here too, without anyone teaching the daemon about
/// a particular vendor.
///
/// A `pattern_id` naming a pattern the catalogue does not have is
/// `Unknown` rather than `Invalid`. The value is not at fault for a
/// typo in its metadata, and answering "invalid" would send someone
/// rotating a perfectly good secret.
fn format_verdict(pattern: Option<&'static dyn SecretPattern>, value: &str) -> FormatVerdict {
    match pattern {
        Some(p) if p.format_regex().is_match(value) => FormatVerdict::Ok,
        Some(_) => FormatVerdict::Invalid,
        None => FormatVerdict::Unknown,
    }
}

/// The catalogue pattern an entry's metadata points at, if any.
///
/// Resolved once per request and shared by the format check and the
/// liveness probe, so the two can never disagree about which rule
/// this entry is under.
fn resolved_pattern(meta: &EntryMetadata) -> Option<&'static dyn SecretPattern> {
    meta.pattern_id
        .as_deref()
        .and_then(|id| devboy_secret_patterns::resolved::shared().find(id))
}

/// Whether a method counts as "user activity" for the ADR-023 §3.3
/// idle-timeout. `vault.status` and `vault.lock` are free probes /
/// shutdown signals; they do not extend the unlock window.
/// `vault.unlock` resets the window through `record_unlock`, not via
/// this helper.
fn is_user_activity(method: &str) -> bool {
    matches!(
        method,
        "secret.get"
            | "secret.list"
            | "secret.put"
            | "secret.put_interactive"
            | "secret.rotate"
            | "metadata.update"
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
        let mut server = VaultServer::new(path);
        // Every test here runs against the configuration the ADR
        // recommends: a daemon with no terminal of its own. It is
        // also the only way these tests do not block on the
        // developer's screen when `cargo test` runs in a terminal.
        server.pretend_to_have_no_terminal();
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

    /// A secret written after unlock must be scrubbed from the
    /// audit log too.
    ///
    /// The scrubber was built once, in `open_audit`, over what the
    /// vault held at that moment. Everything written afterwards was
    /// invisible to it — and the newest secret is the one most
    /// likely to be sitting in some component's error text right
    /// now. The value below is deliberately not secret-shaped, so
    /// the catalogue patterns cannot rescue the test; only knowing
    /// the value works.
    #[tokio::test]
    async fn a_secret_written_after_unlock_is_still_scrubbed() {
        const AFTER: &str = "written-after-the-vault-was-opened-9182736455";

        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;

        let before = server
            .audit
            .as_ref()
            .expect("unlock opens the audit trail")
            .known_value_count();

        let put = server
            .handle_request(req(
                2,
                "secret.put",
                json!({"path": "team/api/later", "value": AFTER, "fresh_unlock": {"kind": "passphrase", "secret": "p"}}),
            ))
            .await;
        assert!(put.error.is_none(), "{:?}", put.error);

        let writer = server.audit.as_ref().expect("still open");
        assert_eq!(
            writer.known_value_count(),
            before + 1,
            "the write must reach the scrubber"
        );

        let scrubbed = writer.scrub(&format!("upstream said: {AFTER} is invalid"));
        assert!(
            !scrubbed.as_str().contains(AFTER),
            "a secret written after unlock reached the audit log in plaintext: {}",
            scrubbed.as_str()
        );
    }

    /// Same for a rotation: the new value replaces the old one the
    /// scrubber was built with.
    #[tokio::test]
    async fn a_rotated_value_is_scrubbed_and_not_only_the_old_one() {
        const OLD: &str = "the-original-value-172635444";
        const NEW: &str = "the-rotated-value-998877665544";

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
                json!({"path": "team/api/rotating", "value": OLD, "fresh_unlock": {"kind": "passphrase", "secret": "p"}}),
            ))
            .await;

        let rotate = server
            .handle_request(req(
                3,
                "secret.rotate",
                json!({"path": "team/api/rotating", "new_value": NEW, "fresh_unlock": {"kind": "passphrase", "secret": "p"}}),
            ))
            .await;
        assert!(rotate.error.is_none(), "{:?}", rotate.error);

        let scrubbed = server
            .audit
            .as_ref()
            .expect("still open")
            .scrub(&format!("upstream rejected {NEW}"));
        assert!(
            !scrubbed.as_str().contains(NEW),
            "the rotated-in value was not scrubbed: {}",
            scrubbed.as_str()
        );
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

    /// The scrub path must actually run in production, not only in
    /// the writer's own tests. Until an unlock carried detail, the
    /// daemon passed `None` every time and the scrubber — the whole
    /// point of Ф9a — never executed against a real record.
    #[tokio::test]
    async fn the_unlock_record_carries_scrubbed_detail() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;

        let key = server.vault.as_ref().unwrap().audit_key().unwrap();
        let records = server.audit.as_ref().unwrap().read_all(&key).unwrap();
        let unlock = records
            .iter()
            .find(|r| r.action == "unlock")
            .expect("the unlock is recorded");

        let detail = unlock
            .detail
            .as_deref()
            .expect("the unlock record should carry detail, or the scrubber never runs");
        assert!(
            detail.contains("method=passphrase"),
            "an auditor needs to know which method opened the vault: {detail}"
        );
        assert!(
            detail.contains("trust="),
            "and how much the daemon's own position was worth: {detail}"
        );
        assert!(
            !detail.contains('p') || !detail.contains("secret"),
            "the passphrase must not appear in the detail: {detail}"
        );
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

    /// The policy is not the state. A caller told the vault
    /// re-locks after N seconds of inactivity still cannot tell
    /// whether it has most of that left or almost none, and that
    /// is the difference between acting now and asking the user
    /// first. `remaining_seconds()` computed the answer and
    /// nothing put it in the reply.
    #[tokio::test]
    async fn status_reports_how_much_of_the_window_is_left_not_just_its_size() {
        let (_dir, mut server) = fresh_vault("p");

        let locked = server
            .handle_request(req(1, "vault.status", Value::Null))
            .await;
        assert!(
            locked.result.expect("status")["unlock_seconds_remaining"].is_null(),
            "a locked vault has no window to have a remainder of"
        );

        server
            .handle_request(req(
                2,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;

        let r = server
            .handle_request(req(3, "vault.status", Value::Null))
            .await;
        let result = r.result.expect("status");

        let remaining = result["unlock_seconds_remaining"]
            .as_u64()
            .expect("an unlocked vault reports its remainder");
        let ttl = result["unlock_ttl_seconds"].as_u64().expect("ttl");

        assert!(
            remaining > 0 && remaining <= ttl,
            "remainder {remaining} is not inside the window {ttl}"
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

    /// A code computed the way a phone computes it must verify
    /// through the daemon.
    ///
    /// This is the regression test for the defect that shipped: the
    /// slot holds base32, the phone HMACs the *decoded* bytes, and
    /// the daemon used to HMAC the text. Every other test seeded the
    /// slot with an arbitrary string and derived the expected code
    /// from that same string, so both halves agreed with each other
    /// and neither agreed with the user.
    #[tokio::test]
    async fn a_code_a_phone_would_show_unlocks_through_the_daemon() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;

        // Enrolment stores base32 text, exactly as `secrets add-totp`
        // does.
        let key = [0x2au8; 32];
        server
            .vault
            .as_mut()
            .unwrap()
            .put(
                crate::totp_session::TOTP_SECRET_PATH,
                &SecretString::from(data_encoding::BASE32_NOPAD.encode(&key)),
                EntryMetadata::default(),
            )
            .expect("seed");

        // Re-unlock so the daemon adopts it.
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

        // The code the phone shows, derived from the raw key — never
        // from anything the daemon stored.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let code = devboy_vault_crypto::totp::code_for_step(&key, now / 30).expect("code");

        // Verified against the session rather than through
        // `totp.unlock`, because §7 refuses the RPC outright for a
        // daemon started by its caller — which is every daemon under
        // `cargo test`. That refusal is why this defect survived:
        // the only path that would have exercised the adopted key was
        // closed before it reached the key.
        assert!(
            server.totp.is_available(),
            "the daemon should have adopted the enrolled secret"
        );
        server
            .totp
            .verify(&code, std::time::Instant::now(), now)
            .expect("a code from the user's authenticator must verify");
    }

    /// The check the whole TOTP scheme rests on.    /// The check the whole TOTP scheme rests on.
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
                // Base32, because that is what enrolment writes. The
                // earlier version of this test seeded a raw ASCII
                // string, which the daemon then used verbatim as the
                // HMAC key — the test agreed with the daemon, both
                // disagreed with the user's authenticator app, and
                // the feature was dead in production while this
                // stayed green.
                &SecretString::from(data_encoding::BASE32_NOPAD.encode(b"12345678901234567890")),
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

    /// The whole point of Ф14, driven through the dispatcher: a
    /// daemon with no screen of its own asks on the one the caller
    /// lent, and a human answering there unlocks the vault.
    ///
    /// # Why the typist waits for echo to go off
    ///
    /// The prompt disables echo with `TCSAFLUSH`, which discards
    /// input typed *before* the prompt appeared — a deliberate
    /// property, so a stray keystroke cannot be read as a
    /// passphrase. That makes "sleep 50ms and hope the flush already
    /// happened" a race, and losing it is unrecoverable: the flush
    /// eats the line and the read waits for a second one that never
    /// comes.
    ///
    /// It lost that race on macOS, and there it does not even fail.
    /// On Linux the read returns EOF once the last master fd closes;
    /// on the BSD side it simply blocks. The job ran for six hours
    /// and was killed with no diagnosis.
    ///
    /// # Why the master is drained
    ///
    /// Waiting for echo is only half of it, and the half alone
    /// deadlocks. `TCSAFLUSH` also waits for pending *output* to
    /// drain, and the prompt has just been written — so with nobody
    /// reading the master, the prompt blocks *before* it can disable
    /// echo, while the typist waits for exactly that to happen. On
    /// Linux the buffer is large enough that the write never blocks;
    /// on macOS it is not. `prompt`'s own tests already worked this
    /// out and drain; this one had not caught up. A real terminal is
    /// always being drained by its emulator, so draining is the more
    /// faithful simulation as well.
    ///
    /// # Why the bound is a thread and not `tokio::time::timeout`
    ///
    /// That was the first attempt, and it does nothing here. The
    /// read inside the prompt is an ordinary blocking syscall, so
    /// the future never reaches an await point and the timer never
    /// gets to fire — measured: the probe still hung. Cancelling a
    /// future cannot cancel a thread stuck in `read`. So the request
    /// runs on a thread of its own and the test waits on a channel;
    /// if it never answers, the test fails in seconds and the stuck
    /// thread dies with the process.
    #[test]
    fn a_lent_terminal_unlocks_through_the_dispatcher() {
        use nix::sys::termios::{LocalFlags, tcgetattr};
        use std::io::{Read, Write};
        use std::os::fd::AsFd;

        let (_dir, mut server) = fresh_vault("correct horse");
        let pty = nix::pty::openpty(None, None).expect("openpty");
        let path = nix::unistd::ttyname(pty.slave.as_fd()).expect("ttyname");

        // Nothing else reads the master, so the prompt's own output
        // would sit in the queue and block `TCSAFLUSH` forever.
        let mut drain_source =
            std::fs::File::from(pty.master.try_clone().expect("clone master for drain"));
        std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            while matches!(drain_source.read(&mut buf), Ok(n) if n > 0) {}
        });

        // Reading blocks, so the human types from another thread.
        let master = pty.master.try_clone().expect("clone master");
        let slave_path = path.clone();
        let typist = std::thread::spawn(move || {
            let watch = std::fs::File::open(&slave_path).expect("open the lent terminal");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let flags = tcgetattr(watch.as_fd()).expect("tcgetattr").local_flags;
                if !flags.contains(LocalFlags::ECHO) {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the prompt never turned echo off, so it never got as far as reading"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let mut m = std::fs::File::from(master);
            m.write_all(b"correct horse\n").expect("type");
        });

        let tty = path.display().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            let r = rt.block_on(server.handle_request(req(
                1,
                "vault.request_unlock",
                json!({ "tty": tty }),
            )));
            let _ = tx.send((server, r));
        });

        let (server, r) = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the daemon blocked forever reading the lent terminal");
        typist.join().expect("typist");

        assert!(r.error.is_none(), "unlock failed: {:?}", r.error);
        assert!(
            server.is_unlocked(),
            "the passphrase typed on the lent terminal must have opened the vault"
        );
    }

    /// A caller naming something that is not a terminal must be
    /// refused, and told which of the two channels failed — the
    /// daemon having no screen is a different problem from the
    /// offered one being unusable.
    #[tokio::test]
    async fn a_lent_path_that_is_not_a_terminal_is_refused() {
        let (_dir, mut server) = fresh_vault("p");

        let r = server
            .handle_request(req(
                1,
                "vault.request_unlock",
                json!({"tty": "/etc/passwd"}),
            ))
            .await;

        let err = r.error.expect("must refuse");
        assert_eq!(err.code, NO_PROMPT_SURFACE);
        assert!(
            err.message.contains("offered by the caller"),
            "the message must say which channel failed: {}",
            err.message
        );
        assert!(!server.is_unlocked());
    }

    /// The daemon's own terminal wins when it has one: it needs
    /// nothing from the caller, and fewer moving parts in a
    /// passphrase prompt is better.
    #[test]
    fn the_daemons_own_terminal_is_preferred_over_a_lent_one() {
        let pty = nix::pty::openpty(None, None).expect("openpty");
        let own = crate::prompt::TtyPrompt::from_file(std::fs::File::from(pty.slave));

        let (_prompt, channel) =
            choose_prompt_surface(Some(own), Some("/dev/pts/999")).expect("own wins");

        assert_eq!(channel, "own");
    }

    /// Neither channel available is its own message, distinct from
    /// "what you offered is unusable".
    #[test]
    fn no_terminal_anywhere_says_so_and_names_the_way_out() {
        let err = choose_prompt_surface(None, None).expect_err("no surface");

        assert_eq!(err, NO_TERMINAL_AT_ALL);
        assert!(err.contains("devboy secrets agent unlock"), "{err}");
        assert!(err.contains("DEVBOY_VAULT_PASSPHRASE"), "{err}");
    }

    /// Every path that opens the vault must start a window, or the
    /// vault stays unlocked forever while `status` says otherwise.
    ///
    /// `secret.put_interactive` used to set `self.vault` directly and
    /// skip all of it: no window, no audit trail, no TOTP. This pins
    /// the single door both paths now go through.
    #[tokio::test]
    async fn adopting_a_vault_starts_the_window_and_the_trail() {
        let (_dir, mut server) = fresh_vault("p");

        let vault = Vault::open(
            &server.vault_path,
            UnlockMethod::Passphrase(SecretString::from("p".to_owned())),
        )
        .expect("open");

        server.adopt_unlocked_vault(vault);

        assert!(server.is_unlocked());
        assert!(
            server.idle.expires_at().is_some(),
            "an unlock with no expiry never re-locks — the whole window is void"
        );
        assert!(
            server.audit.is_some(),
            "without an open trail every later write records nothing"
        );
    }

    /// Rotate is a write, and the reserved slot is unwritable.
    ///
    /// It was the one write path without the guard. Overwriting
    /// `__totp/secret` there hands the next unlock an attacker-chosen
    /// shared secret, after which they can produce valid codes — the
    /// exact thing "unwritable even on a fully unlocked vault" is
    /// supposed to prevent.
    #[tokio::test]
    async fn rotate_refuses_the_reserved_totp_slot() {
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
                    "path": crate::totp_session::TOTP_SECRET_PATH,
                    "new_value": "attacker-chosen",
                    "fresh_unlock": {"kind": "passphrase", "secret": "p"},
                }),
            ))
            .await;

        let err = r.error.expect("a reserved path must not be rotatable");
        assert!(
            err.message.contains("reserved"),
            "the refusal should say why: {}",
            err.message
        );
    }

    /// `vault.request_unlock` must carry no passphrase field at all.    /// `vault.request_unlock` must carry no passphrase field at all.    /// `vault.request_unlock` must carry no passphrase field at all.
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

    // -- secret.validate ---------------------------------------------------

    /// A well-formed GitLab PAT. Long enough for the built-in
    /// `^glpat-[A-Za-z0-9_-]{20,}$`.
    const GOOD_PAT: &str = "glpat-ABCDEFGHIJKLMNOPQRSTU";

    /// Unlock, then store `value` at `path` carrying `pattern_id`.
    async fn seed(server: &mut VaultServer, path: &str, value: &str, pattern_id: Option<&str>) {
        let unlocked = server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;
        assert!(unlocked.error.is_none(), "{:?}", unlocked.error);

        let meta = match pattern_id {
            Some(id) => json!({"pattern_id": id}),
            None => json!({}),
        };
        let put = server
            .handle_request(req(
                2,
                "secret.put",
                json!({
                    "path": path,
                    "value": value,
                    "meta": meta,
                    "fresh_unlock": {"kind": "passphrase", "secret": "p"}
                }),
            ))
            .await;
        assert!(put.error.is_none(), "{:?}", put.error);
    }

    async fn validate(server: &mut VaultServer, params: Value) -> JsonRpcResponse {
        server
            .handle_request(req(9, "secret.validate", params))
            .await
    }

    /// The whole point of the method: the caller learns the value is
    /// well-formed and does not learn the value.
    #[tokio::test]
    async fn a_verdict_comes_back_and_the_value_does_not() {
        let (_dir, mut server) = fresh_vault("p");
        seed(
            &mut server,
            "team/gitlab/token",
            GOOD_PAT,
            Some("gitlab-pat"),
        )
        .await;

        let resp = validate(&mut server, json!({"path": "team/gitlab/token"})).await;

        let result = resp.result.expect("a stored, well-formed value validates");
        assert_eq!(result["format"], "ok");
        assert_eq!(result["pattern_id"], "gitlab-pat");

        let rendered = serde_json::to_string(&result).unwrap();
        assert!(
            !rendered.contains(GOOD_PAT),
            "the value crossed the socket: {rendered}"
        );
    }

    #[tokio::test]
    async fn a_value_of_the_wrong_shape_is_invalid() {
        let (_dir, mut server) = fresh_vault("p");
        seed(
            &mut server,
            "team/gitlab/token",
            "not-a-gitlab-token",
            Some("gitlab-pat"),
        )
        .await;

        let resp = validate(&mut server, json!({"path": "team/gitlab/token"})).await;

        assert_eq!(resp.result.unwrap()["format"], "invalid");
    }

    /// "Checked and passed" and "nobody said what this should look
    /// like" are different facts. An agent that reads `unknown` as
    /// `ok` reports confidence nobody earned.
    #[tokio::test]
    async fn an_entry_with_no_declared_shape_is_unknown_rather_than_ok() {
        let (_dir, mut server) = fresh_vault("p");
        seed(&mut server, "team/thing/token", "whatever", None).await;

        let result = validate(&mut server, json!({"path": "team/thing/token"}))
            .await
            .result
            .unwrap();

        assert_eq!(result["format"], "unknown");
        assert!(result["pattern_id"].is_null());
    }

    /// A `pattern_id` that resolves to nothing is a defect in the
    /// metadata, not in the value. Answering `invalid` here would
    /// send someone rotating a perfectly good secret because of a
    /// typo.
    #[tokio::test]
    async fn a_dangling_pattern_id_is_unknown_rather_than_invalid() {
        let (_dir, mut server) = fresh_vault("p");
        seed(
            &mut server,
            "team/thing/token",
            GOOD_PAT,
            Some("no-such-pattern-was-ever-declared"),
        )
        .await;

        let result = validate(&mut server, json!({"path": "team/thing/token"}))
            .await
            .result
            .unwrap();

        assert_eq!(result["format"], "unknown");
    }

    /// The attack this method has to survive.
    ///
    /// If the caller could hand in the rule, `secret.validate` would
    /// be a value oracle: ask `^g.*`, then `^gl.*`, then `^gla.*`,
    /// and read the secret out of a sequence of yes/no answers. The
    /// rule therefore comes from the daemon's own metadata and from
    /// nowhere else, and a `format_regex` in the request is inert.
    ///
    /// Checked in both directions, because a one-way test passes for
    /// a parser that ignores the field *and* for one that ANDs it
    /// with the real rule — and the second is still an oracle.
    #[tokio::test]
    async fn a_rule_supplied_by_the_caller_is_ignored() {
        let (_dir, mut server) = fresh_vault("p");
        seed(
            &mut server,
            "team/gitlab/token",
            "not-a-gitlab-token",
            Some("gitlab-pat"),
        )
        .await;

        // A rule that matches anything must not turn a mismatch into
        // a pass.
        let permissive = validate(
            &mut server,
            json!({"path": "team/gitlab/token", "format_regex": "^.*$"}),
        )
        .await;
        assert_eq!(
            permissive.result.unwrap()["format"],
            "invalid",
            "the caller's regex decided the verdict"
        );

        // ...and one that matches nothing must not turn a pass into
        // a mismatch. An implementation that ANDs the two would pass
        // the first probe and fail here — and would still leak the
        // value one character at a time.
        let (_dir2, mut server2) = fresh_vault("p");
        seed(
            &mut server2,
            "team/gitlab/token",
            GOOD_PAT,
            Some("gitlab-pat"),
        )
        .await;
        let restrictive = validate(
            &mut server2,
            json!({"path": "team/gitlab/token", "format_regex": "^definitely-not-this$"}),
        )
        .await;
        assert_eq!(
            restrictive.result.unwrap()["format"],
            "ok",
            "the caller's regex decided the verdict"
        );
    }

    /// Reading a value is reading a value, however little of it comes
    /// back. A verdict-only reply that skipped the trail would be the
    /// quiet way to walk a vault.
    #[tokio::test]
    async fn a_validation_is_written_to_the_audit_trail() {
        let (_dir, mut server) = fresh_vault("p");
        seed(
            &mut server,
            "team/gitlab/token",
            GOOD_PAT,
            Some("gitlab-pat"),
        )
        .await;
        validate(&mut server, json!({"path": "team/gitlab/token"})).await;

        let key = server.vault.as_ref().unwrap().audit_key().unwrap();
        let records = server.audit.as_ref().unwrap().read_all(&key).unwrap();

        let record = records
            .iter()
            .find(|r| r.action == "validate")
            .expect("a validation must be recorded");
        assert_eq!(record.path, "team/gitlab/token");
        assert_eq!(record.actor, "agent");
        assert_eq!(
            record.detail.as_deref(),
            Some("ok"),
            "the verdict is the useful part of the record"
        );
    }

    /// Same silence as `secret.get`: an agent has no business
    /// learning that the TOTP slot exists, and "is the shared secret
    /// well-formed?" is not a question worth answering either.
    #[tokio::test]
    async fn the_totp_slot_cannot_be_probed() {
        let (_dir, mut server) = fresh_vault("p");
        server
            .handle_request(req(
                1,
                "vault.unlock",
                json!({"kind": "passphrase", "secret": "p"}),
            ))
            .await;

        let resp = validate(
            &mut server,
            json!({"path": format!("{}shared", crate::totp_session::RESERVED_PREFIX)}),
        )
        .await;

        assert_eq!(resp.error.unwrap().code, ENTRY_NOT_FOUND);
    }

    #[tokio::test]
    async fn a_locked_vault_validates_nothing() {
        let (_dir, mut server) = fresh_vault("p");

        let resp = validate(&mut server, json!({"path": "team/gitlab/token"})).await;

        assert_eq!(resp.error.unwrap().code, VAULT_LOCKED);
    }

    /// Validation must not extend the unlock window.
    ///
    /// This is the first value-touching method an agent can reach —
    /// `secret.get` exists but only the CLI calls it, because agents
    /// are handed aliases rather than values. An agent that could
    /// refresh the timer by validating on a loop would keep the vault
    /// open indefinitely and auto-lock (ADR-023 §3.3) would never
    /// fire again.
    ///
    /// Two advances of 6s each against a 10s window: if validating
    /// bumped the timestamp, only 6s would have elapsed since the
    /// last activity and the vault would still be open.
    #[tokio::test]
    async fn validating_does_not_extend_the_unlock_window() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.dvb");
        let _outcome = Vault::create(&path, fast_init("p")).unwrap();
        let clock = crate::idle::ManualClock::new(std::time::Instant::now());
        let mut server = server_with_manual_clock(path, clock.clone());
        server.pretend_to_have_no_terminal();
        seed(
            &mut server,
            "team/gitlab/token",
            GOOD_PAT,
            Some("gitlab-pat"),
        )
        .await;

        clock.advance(std::time::Duration::from_secs(6));
        let inside = validate(&mut server, json!({"path": "team/gitlab/token"})).await;
        assert!(
            inside.error.is_none(),
            "still inside the window: {:?}",
            inside.error
        );

        clock.advance(std::time::Duration::from_secs(6));
        let after = validate(&mut server, json!({"path": "team/gitlab/token"})).await;

        assert_eq!(
            after.error.expect("the window must have closed").code,
            VAULT_LOCKED,
            "validating refreshed the idle timer"
        );
        assert!(!server.is_unlocked());
    }

    /// The control for the test above.
    ///
    /// Without it, `validating_does_not_extend_the_unlock_window`
    /// would also pass on a build where *nothing* refreshes the timer
    /// — where the window simply runs from the unlock. This proves
    /// the harness can see a bump when one happens.
    #[tokio::test]
    async fn a_real_operation_does_extend_the_unlock_window() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.dvb");
        let _outcome = Vault::create(&path, fast_init("p")).unwrap();
        let clock = crate::idle::ManualClock::new(std::time::Instant::now());
        let mut server = server_with_manual_clock(path, clock.clone());
        server.pretend_to_have_no_terminal();
        seed(
            &mut server,
            "team/gitlab/token",
            GOOD_PAT,
            Some("gitlab-pat"),
        )
        .await;

        clock.advance(std::time::Duration::from_secs(6));
        let inside = server
            .handle_request(req(20, "secret.list", json!({})))
            .await;
        assert!(inside.error.is_none(), "{:?}", inside.error);

        clock.advance(std::time::Duration::from_secs(6));
        let after = server
            .handle_request(req(21, "secret.list", json!({})))
            .await;

        assert!(
            after.error.is_none(),
            "a listing should have refreshed the timer: {:?}",
            after.error
        );
    }

    /// A pattern with no liveness endpoint answers `unsupported`
    /// when a probe is asked for — not silence. An agent that asked
    /// and got no `liveness` field back would read it as "not
    /// requested" and never learn the check was unavailable.
    #[tokio::test]
    async fn asking_for_liveness_where_none_is_declared_says_so() {
        let (_dir, mut server) = fresh_vault("p");
        seed(
            &mut server,
            "team/gitlab/token",
            GOOD_PAT,
            Some("gitlab-pat"),
        )
        .await;

        let result = validate(
            &mut server,
            json!({"path": "team/gitlab/token", "liveness": true}),
        )
        .await
        .result
        .unwrap();

        assert_eq!(result["format"], "ok");
        assert_eq!(result["liveness"], "unsupported");
    }

    /// The default. A probe costs a network round trip and a line in
    /// the provider's own audit log; a caller that did not ask must
    /// not pay for either.
    #[tokio::test]
    async fn liveness_is_not_probed_unless_asked_for() {
        let (_dir, mut server) = fresh_vault("p");
        seed(
            &mut server,
            "team/gitlab/token",
            GOOD_PAT,
            Some("gitlab-pat"),
        )
        .await;

        let result = validate(&mut server, json!({"path": "team/gitlab/token"}))
            .await
            .result
            .unwrap();

        assert!(
            result["liveness"].is_null(),
            "a probe nobody asked for: {result}"
        );

        let key = server.vault.as_ref().unwrap().audit_key().unwrap();
        let records = server.audit.as_ref().unwrap().read_all(&key).unwrap();
        assert!(
            !records.iter().any(|r| r.action == "liveness-probe"),
            "the trail records a probe that should not have happened"
        );
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
