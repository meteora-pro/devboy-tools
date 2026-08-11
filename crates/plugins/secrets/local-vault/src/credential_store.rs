//! Bridge from the local-vault daemon to [`CredentialStore`](devboy_storage::CredentialStore)
//! (ADR-024 §6, gap found while landing the default flip).
//!
//! # The gap
//!
//! ADR-024 names `local-vault` as the default store. But
//! `local-vault` implements `SecretSource` — the ADR-021 router
//! stack — while every provider token in the CLI and MCP server
//! resolves through `CredentialStore`, the ADR-005 chain. Two
//! traits, no adapter between them.
//!
//! Dropping the keychain from the default chain therefore left
//! nothing behind the environment: a token in the vault was
//! invisible to `doctor`, to the MCP server, and to every provider
//! client. This module is that missing adapter.
//!
//! # Why it speaks the socket directly
//!
//! [`SecretSource`](devboy_storage::SecretSource) is async and
//! `CredentialStore` is sync, so adapting
//! [`LocalVaultSource`](crate::LocalVaultSource) would mean driving
//! a runtime from inside a sync call — which panics when a runtime
//! is already running, exactly the situation in the MCP server. The
//! daemon protocol is line-delimited JSON-RPC over a UNIX socket,
//! so a synchronous client is short and has no runtime to conflict
//! with.
//!
//! # Why it lives here and not in `devboy-storage`
//!
//! `devboy-storage` is published to crates.io; the daemon crate is
//! not. A published crate cannot depend on an unpublished one, so
//! hosting this there breaks `cargo publish` outright. The
//! constraint points at the right layer anyway: this crate already
//! exists to talk to the vault daemon and already depends on both
//! halves. `devboy-storage` keeps a daemon-free default chain, and
//! the application composes the real one.
//!
//! # Read-through, not read-write
//!
//! This bridge deliberately implements **reads only**, and
//! [`is_writable`](devboy_storage::CredentialStore::is_writable)
//! reports `false`.
//!
//! The daemon requires a `fresh_unlock` proof — a passphrase or
//! TOTP code — on every `secret.put` and `secret.rotate`
//! (ADR-023 §3.3, the hybrid-mode requirement). The
//! `CredentialStore::store(&self, key, value)` signature carries no
//! unlock material and has no channel to ask for one. Satisfying it
//! from here would mean either prompting from inside a library
//! crate or caching a passphrase in this struct — the first
//! pre-empts the daemon-side prompt channel that ADR-024 §7
//! requires, and the second is worse than the keychain this epic
//! just demoted.
//!
//! So writes fail with an actionable error naming what is missing,
//! rather than silently succeeding into nowhere. Writing through
//! the daemon is real work that depends on the prompt channel
//! landing first; it is tracked separately.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use devboy_core::{Error, Result};
use devboy_secrets_agent::{ENTRY_NOT_FOUND, VAULT_LOCKED};
use devboy_storage::CredentialStore;
use secrecy::SecretString;
use serde_json::{Value, json};
use tracing::debug;

/// How long to wait on the daemon before giving up.
///
/// Short on purpose: a wedged daemon must not stall a command that
/// could have fallen through to another store.
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Synchronous, read-through [`CredentialStore`] over the
/// local-vault daemon.
#[derive(Debug, Clone)]
pub struct VaultStore {
    socket_path: PathBuf,
}

impl VaultStore {
    /// Build a store against the canonical agent socket.
    ///
    /// Returns `None` when the socket path cannot be determined at
    /// all (no config directory), since a store that can never
    /// connect is not worth putting in a chain.
    pub fn new() -> Option<Self> {
        devboy_secrets_agent::default_socket_path()
            .ok()
            .map(|socket_path| Self { socket_path })
    }

    /// Build a store against an explicit socket path.
    pub fn with_socket(path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: path.into(),
        }
    }

    /// The socket this store talks to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Whether the daemon socket exists.
    ///
    /// Checked before connecting so a machine that never starts the
    /// daemon pays a `stat` rather than a connect timeout on every
    /// lookup.
    pub fn daemon_present(&self) -> bool {
        self.socket_path.exists()
    }

    /// Error returned for any write attempt. See the module docs
    /// for why writes are not implementable against this trait.
    fn write_unsupported(&self, key: &str) -> Error {
        Error::Storage(format!(
            "cannot write '{key}' to the local vault through this path: the daemon requires a \
             fresh passphrase or TOTP proof for every write, which this interface cannot supply. \
             Store it with `devboy secrets ui`, or set the corresponding environment variable."
        ))
    }

    #[cfg(unix)]
    fn rpc(&self, method: &str, params: Value) -> std::result::Result<Value, RpcFailure> {
        use std::os::unix::net::UnixStream;

        let stream = UnixStream::connect(&self.socket_path).map_err(RpcFailure::Unreachable)?;
        stream.set_read_timeout(Some(RPC_TIMEOUT)).ok();
        stream.set_write_timeout(Some(RPC_TIMEOUT)).ok();

        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let mut writer = stream.try_clone().map_err(RpcFailure::Unreachable)?;
        writeln!(writer, "{request}").map_err(RpcFailure::Unreachable)?;
        writer.flush().map_err(RpcFailure::Unreachable)?;
        // Half-close so the daemon's read loop sees EOF and answers
        // instead of blocking for more bytes.
        drop(writer);

        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(RpcFailure::Unreachable)?;

        let response: Value = serde_json::from_str(&line).map_err(|e| {
            RpcFailure::Protocol(format!("malformed reply from secret daemon: {e}"))
        })?;

        if let Some(error) = response.get("error")
            && !error.is_null()
        {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0) as i32;
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown daemon error")
                .to_owned();
            return Err(RpcFailure::Daemon { code, message });
        }

        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    #[cfg(not(unix))]
    fn rpc(&self, _method: &str, _params: Value) -> std::result::Result<Value, RpcFailure> {
        // The daemon protocol is UNIX-domain-socket only by design
        // (ADR-023 §3.3). On Windows the store simply never
        // participates, which `daemon_present` already reports.
        Err(RpcFailure::Protocol(
            "the secret daemon is only reachable over UNIX domain sockets".to_owned(),
        ))
    }
}

/// Why an RPC did not produce a result.
///
/// Kept separate from [`Error`] because the chain treats these
/// differently: an unreachable daemon means "ask the next store",
/// a locked vault means "stop and tell the user".
#[derive(Debug)]
enum RpcFailure {
    /// Could not reach the daemon at all.
    Unreachable(std::io::Error),
    /// Reached it, but the exchange did not make sense.
    Protocol(String),
    /// The daemon answered with a JSON-RPC error.
    Daemon { code: i32, message: String },
}

impl std::fmt::Display for RpcFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "daemon unreachable: {e}"),
            Self::Protocol(m) => write!(f, "protocol error: {m}"),
            Self::Daemon { code, message } => write!(f, "daemon error {code}: {message}"),
        }
    }
}

impl CredentialStore for VaultStore {
    fn store(&self, key: &str, _value: &SecretString) -> Result<()> {
        Err(self.write_unsupported(key))
    }

    fn get(&self, key: &str) -> Result<Option<SecretString>> {
        if !self.daemon_present() {
            return Ok(None);
        }

        let path = key_to_vault_path(key);
        match self.rpc("secret.get", json!({ "path": path })) {
            Ok(result) => {
                let value = result
                    .get("value")
                    .and_then(Value::as_str)
                    .map(|s| SecretString::from(s.to_owned()));
                if value.is_some() {
                    debug!(key = key, path = %path, "resolved credential from local vault");
                }
                Ok(value)
            }
            // A locked vault is not a missing secret. Reporting it
            // as `None` would send the caller down the "set this
            // environment variable" path when the real fix is to
            // unlock — the single most confusing failure this
            // bridge could produce.
            Err(RpcFailure::Daemon { code, message }) if code == VAULT_LOCKED => {
                Err(Error::Storage(format!(
                    "the local vault is locked, so '{key}' cannot be read: {message}. Unlock it \
                     with `devboy secrets agent unlock`."
                )))
            }
            // Genuinely absent: the next store gets a turn, and
            // there is nothing to report.
            Err(RpcFailure::Daemon { code, .. }) if code == ENTRY_NOT_FOUND => Ok(None),
            // Anything else also falls through, but quietly falling
            // through would make a misbehaving daemon look exactly
            // like an empty vault. Leave a trace.
            Err(other) => {
                debug!(
                    key = key,
                    path = %path,
                    reason = %other,
                    "local vault did not answer; falling through to the next store"
                );
                Ok(None)
            }
        }
    }

    fn delete(&self, key: &str) -> Result<()> {
        // The daemon exposes no deletion method, and ADR-024 §5
        // made deletion a tombstone write in any case — which is a
        // write, and therefore subject to the same unlock
        // requirement as `store`.
        Err(self.write_unsupported(key))
    }

    fn is_available(&self) -> bool {
        self.daemon_present()
    }

    fn is_writable(&self) -> bool {
        false
    }
}

/// Map an ADR-005 credential key to an ADR-020 vault path.
///
/// `github.token` → `personal/github/token`.
///
/// Dots become slashes and an unscoped key lands under `personal/`,
/// so the same secret is reachable by the same logical name from
/// either stack — the mirror of the legacy-name expansion in the
/// env-store.
///
/// A key that already looks like a path passes through untouched,
/// so callers that have migrated are not mangled.
pub fn key_to_vault_path(key: &str) -> String {
    if key.contains('/') {
        return key.to_owned();
    }
    format!("personal/{}", key.replace('.', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_keys_map_onto_scoped_vault_paths() {
        assert_eq!(key_to_vault_path("github.token"), "personal/github/token");
        assert_eq!(key_to_vault_path("gitlab.token"), "personal/gitlab/token");
        assert_eq!(
            key_to_vault_path("proxy.my-server.token"),
            "personal/proxy/my-server/token"
        );
    }

    /// A caller that already speaks ADR-020 must not have its path
    /// rewritten underneath it.
    #[test]
    fn paths_are_passed_through_unchanged() {
        assert_eq!(key_to_vault_path("team/gitlab/token"), "team/gitlab/token");
        assert_eq!(
            key_to_vault_path("__sources/vault/default"),
            "__sources/vault/default"
        );
    }

    #[test]
    fn a_bare_key_still_gets_a_scope() {
        assert_eq!(key_to_vault_path("token"), "personal/token");
    }

    /// A machine that never starts the daemon must not pay a
    /// connect timeout on every lookup, and must not fail the
    /// lookup either — the next store gets a turn.
    #[test]
    fn a_missing_daemon_falls_through_rather_than_erroring() {
        let store = VaultStore::with_socket("/nonexistent/devboy-test.sock");

        assert!(!store.daemon_present());
        assert!(!store.is_available());
        assert!(store.get("github.token").unwrap().is_none());
    }

    /// Writes must fail loudly. A store that reports success and
    /// drops the secret is the exact failure this epic keeps
    /// designing against, and it is what the old CI chain did by
    /// pairing the env store with an in-memory one.
    #[test]
    fn writes_fail_loudly_rather_than_vanishing() {
        let store = VaultStore::with_socket("/nonexistent/devboy-test.sock");

        let err = store
            .store("github.token", &SecretString::from("v".to_owned()))
            .expect_err("a write must never report success here");
        let message = err.to_string();
        assert!(
            message.contains("github.token"),
            "the error should name the key: {message}"
        );
        assert!(
            message.contains("devboy secrets ui") || message.contains("environment variable"),
            "the error should say where to put the secret instead: {message}"
        );

        // Deletion is a tombstone write, so it is refused the same
        // way rather than silently doing nothing.
        assert!(store.delete("github.token").is_err());
    }

    /// The chain consults `is_writable` to pick a write target.
    /// Claiming to be writable here would route every write into
    /// the error above.
    #[test]
    fn the_store_does_not_claim_to_be_writable() {
        let store = VaultStore::with_socket("/nonexistent/devboy-test.sock");
        assert!(!store.is_writable());
    }
}
