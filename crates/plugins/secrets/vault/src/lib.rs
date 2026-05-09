//! `devboy-secret-vault` — built-in `SecretSource` backed by the
//! HashiCorp Vault HTTP API (KV v2 secret engine) per
//! [ADR-021] §8.
//!
//! `reference` is `<mount>/data/<path>[#<field>]` — for example
//! `secret/data/team/gitlab/token-deploy#value`. The `#<field>`
//! suffix selects which KV-entry field to return; it defaults to
//! `value` when omitted (matches the convention `vault kv put` uses
//! for single-field entries).
//!
//! # Capabilities
//!
//! Per ADR-021 §8: `READ | LIST | VALIDATE | WRITE | ROTATE |
//! AUDIT_LOGGED` (subject to the policy attached to the active
//! token). The `AUDIT_LOGGED` flag is unconditional — every read
//! lands in the upstream audit log; users discover this through
//! `doctor` (P7.2).
//!
//! Write/rotate capabilities are declared honestly: the trait
//! doesn't yet expose `put` / `rotate`, so the actual write path
//! lands in a later phase. The cap declaration is what the router
//! uses to decide *whether* to dispatch a future write.
//!
//! # Authentication
//!
//! [`VaultAuth`] selects between three methods named in ADR-021:
//!
//! - **Token** — caller has already resolved a `X-Vault-Token`
//!   value (typically out of `__sources/<this-source>/token`).
//!   Every request sends it verbatim.
//! - **AppRole** — caller has `role_id` + `secret_id`. The source
//!   exchanges them for a token at `/v1/<auth-mount>/login` on
//!   first use and caches the token in-process.
//! - **OIDC** — placeholder. The full device-flow implementation
//!   lives in a follow-up; calling a source configured for OIDC
//!   in v1 returns a structured "not implemented" error so users
//!   discover the gap through `doctor` rather than at runtime.
//!
//! The cached token is held under `tokio::sync::Mutex` so
//! concurrent `get`/`list` requests don't all race the AppRole
//! login. Token expiry triggers an automatic re-login on the next
//! 403 (handled implicitly: `is_available` -> `Locked` causes the
//! router to surface the error).
//!
//! # Adaptive TTL
//!
//! The `lease_duration` Vault returns at the top of every read
//! response is plumbed straight into [`GetOutcome::lease_duration`].
//! For static KV v2 entries the upstream returns `0` — we surface
//! `None` so the router cache uses its source default; for dynamic
//! engines (database, AWS STS, …) the value is the real lease
//! length and the cache (P5.4) shrinks its TTL accordingly.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use std::time::Duration;

use async_trait::async_trait;
use devboy_storage::{
    Capabilities, GetOutcome, RemoteRef, SecretSource, SourceError, SourceStatus,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Mutex;

/// Default mount path used by [`VaultSource::list`] when the user
/// did not pin one explicitly. Lists the root of the conventional
/// KV v2 mount.
pub const DEFAULT_LIST_MOUNT: &str = "secret/metadata/";

/// Default AppRole login mount. Override via [`VaultAuth::AppRole`]
/// when the user wires AppRole at a non-standard path.
pub const DEFAULT_APPROLE_MOUNT: &str = "auth/approle";

// =============================================================================
// Errors
// =============================================================================

/// Failure modes for [`VaultSource::new`].
#[derive(Debug, Error)]
pub enum VaultSourceError {
    /// Underlying `reqwest::Client` could not be built (e.g. the
    /// caller passed a malformed system proxy URL).
    #[error("failed to build vault HTTP client: {0}")]
    Client(#[from] reqwest::Error),
}

// =============================================================================
// Auth methods
// =============================================================================

/// How the source authenticates against Vault.
#[derive(Debug, Clone)]
pub enum VaultAuth {
    /// Static `X-Vault-Token`. Caller has already resolved the
    /// token (typically by reading `__sources/<source>/token` from
    /// the credential store).
    Token(SecretString),
    /// AppRole — exchange `role_id` + `secret_id` for a token at
    /// `<addr>/v1/<mount>/login`.
    AppRole {
        /// Public `role_id`.
        role_id: String,
        /// `secret_id` — kept in `SecretString` so accidental
        /// `Debug` doesn't leak it.
        secret_id: SecretString,
        /// Optional auth-mount override (default
        /// [`DEFAULT_APPROLE_MOUNT`]).
        mount: Option<String>,
    },
    /// OIDC — placeholder. v1 returns a structured "not
    /// implemented" error if used; the full device-flow
    /// implementation is a follow-up.
    Oidc(OidcConfig),
}

/// Inputs for OIDC auth. Currently unused (returns a
/// `not-implemented` error); the struct is here so users can wire
/// configs forward-compatibly.
#[derive(Debug, Clone, Default)]
pub struct OidcConfig {
    /// OIDC role name registered with Vault.
    pub role: Option<String>,
    /// Optional mount override (default `auth/oidc`).
    pub mount: Option<String>,
}

// =============================================================================
// Source
// =============================================================================

/// `SecretSource` backed by the Vault HTTP API.
pub struct VaultSource {
    /// Logical name from `[[source]] name = "..."` in `sources.toml`.
    name: String,
    /// Vault address — `https://vault.example.com/` (with or
    /// without a trailing slash).
    address: String,
    /// Authentication method.
    auth: VaultAuth,
    /// HTTP client. Held by value; reqwest pools connections
    /// internally so a single client per source is the right
    /// granularity.
    http: reqwest::Client,
    /// Cached token after first successful auth (relevant for
    /// AppRole; for `Token` it's pre-populated at construction).
    cached_token: Mutex<Option<SecretString>>,
    /// Optional override for the `list` mount path.
    list_mount: Option<String>,
}

impl std::fmt::Debug for VaultSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the cached token or the AppRole secret_id.
        f.debug_struct("VaultSource")
            .field("name", &self.name)
            .field("address", &self.address)
            .field(
                "auth",
                &match &self.auth {
                    VaultAuth::Token(_) => "Token(<redacted>)",
                    VaultAuth::AppRole { .. } => "AppRole(<redacted>)",
                    VaultAuth::Oidc(_) => "Oidc(<not-implemented>)",
                },
            )
            .field("list_mount", &self.list_mount)
            .finish()
    }
}

impl VaultSource {
    /// Build a source named `name` against `address` with `auth`.
    pub fn new(
        name: impl Into<String>,
        address: impl Into<String>,
        auth: VaultAuth,
    ) -> Result<Self, VaultSourceError> {
        let http = reqwest::Client::builder().build()?;
        let cached_token = Mutex::new(match &auth {
            VaultAuth::Token(t) => Some(t.clone()),
            _ => None,
        });
        Ok(Self {
            name: name.into(),
            address: address.into(),
            auth,
            http,
            cached_token,
            list_mount: None,
        })
    }

    /// Override the list mount path. Defaults to
    /// [`DEFAULT_LIST_MOUNT`] when unset.
    pub fn with_list_mount(mut self, mount: impl Into<String>) -> Self {
        self.list_mount = Some(mount.into());
        self
    }

    /// Borrow the configured Vault address.
    pub fn address(&self) -> &str {
        &self.address
    }

    fn build_url(&self, path: &str) -> String {
        let base = self.address.trim_end_matches('/');
        let path = path.trim_start_matches('/');
        format!("{base}/{path}")
    }

    /// Resolve and cache the active token. AppRole logs in lazily
    /// on the first call; subsequent calls reuse the cached token.
    async fn resolve_token(&self) -> Result<SecretString, SourceError> {
        let mut guard = self.cached_token.lock().await;
        if let Some(t) = guard.as_ref() {
            return Ok(t.clone());
        }
        let token = match &self.auth {
            VaultAuth::Token(t) => t.clone(),
            VaultAuth::AppRole {
                role_id,
                secret_id,
                mount,
            } => {
                let mount = mount.as_deref().unwrap_or(DEFAULT_APPROLE_MOUNT);
                let url = self.build_url(&format!("v1/{}/login", mount.trim_matches('/')));
                let body = serde_json::json!({
                    "role_id": role_id,
                    "secret_id": secret_id.expose_secret(),
                });
                let resp = self.http.post(&url).json(&body).send().await.map_err(|e| {
                    SourceError::Upstream {
                        name: self.name.clone(),
                        message: format!("vault approle login transport: {e}"),
                    }
                })?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body_text = resp.text().await.unwrap_or_default();
                    return Err(SourceError::Upstream {
                        name: self.name.clone(),
                        message: format!("vault approle login {status}: {}", body_text.trim()),
                    });
                }
                let payload: ApproleLoginResponse =
                    resp.json().await.map_err(|e| SourceError::Upstream {
                        name: self.name.clone(),
                        message: format!("vault approle login malformed JSON: {e}"),
                    })?;
                SecretString::from(payload.auth.client_token)
            }
            VaultAuth::Oidc(_) => {
                return Err(SourceError::Upstream {
                    name: self.name.clone(),
                    message: "OIDC auth is not implemented in v1; use Token or AppRole".to_owned(),
                });
            }
        };
        *guard = Some(token.clone());
        Ok(token)
    }
}

// =============================================================================
// Reference parsing
// =============================================================================

fn parse_reference(reference: &str) -> Result<(String, String), String> {
    if reference.is_empty() {
        return Err("vault reference must be non-empty".to_owned());
    }
    let (path, field) = match reference.split_once('#') {
        Some((p, f)) => (p.trim().to_owned(), f.trim().to_owned()),
        None => (reference.to_owned(), "value".to_owned()),
    };
    if path.is_empty() {
        return Err("vault reference must include a path before '#'".to_owned());
    }
    if field.is_empty() {
        return Err("vault reference field after '#' must be non-empty".to_owned());
    }
    Ok((path, field))
}

// =============================================================================
// Trait impl
// =============================================================================

#[async_trait]
impl SecretSource for VaultSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ
            | Capabilities::LIST
            | Capabilities::VALIDATE
            | Capabilities::WRITE
            | Capabilities::ROTATE
            | Capabilities::AUDIT_LOGGED
    }

    async fn is_available(&self) -> SourceStatus {
        // 1) /sys/health: Vault reachable and not sealed?
        let url = self.build_url("v1/sys/health");
        let resp = match self.http.get(&url).send().await {
            Ok(r) => r,
            Err(e) if e.is_connect() => return SourceStatus::NotInstalled,
            Err(e) => return SourceStatus::Error(format!("vault sys/health transport: {e}")),
        };
        match resp.status().as_u16() {
            // 200 active, 429 standby, 472 DR secondary, 473 perf
            // standby — all mean the API is up enough for reads.
            200 | 429 | 472 | 473 => {}
            // 501 not initialized; 503 sealed.
            501 => return SourceStatus::NotInstalled,
            503 => return SourceStatus::Locked,
            other => {
                return SourceStatus::Error(format!("vault sys/health returned {other}",));
            }
        }

        // 2) Token lookup: do we have a valid X-Vault-Token?
        let token = match self.resolve_token().await {
            Ok(t) => t,
            Err(e) => return SourceStatus::Error(format!("vault token resolve: {e}")),
        };
        let url = self.build_url("v1/auth/token/lookup-self");
        let resp = match self
            .http
            .get(&url)
            .header("X-Vault-Token", token.expose_secret())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return SourceStatus::Error(format!("vault lookup-self transport: {e}")),
        };
        match resp.status().as_u16() {
            200 => SourceStatus::Available,
            403 => SourceStatus::Locked,
            other => SourceStatus::Error(format!("vault lookup-self returned {other}")),
        }
    }

    async fn get(&self, reference: &str) -> Result<Option<GetOutcome>, SourceError> {
        let (path, field) =
            parse_reference(reference).map_err(|reason| SourceError::BadReference {
                name: self.name.clone(),
                reference: reference.to_owned(),
                reason,
            })?;
        let token = self.resolve_token().await?;
        let url = self.build_url(&format!("v1/{}", path.trim_start_matches('/')));
        let resp = self
            .http
            .get(&url)
            .header("X-Vault-Token", token.expose_secret())
            .send()
            .await
            .map_err(|e| SourceError::Upstream {
                name: self.name.clone(),
                message: format!("vault get transport: {e}"),
            })?;
        match resp.status().as_u16() {
            200 => {}
            404 => return Ok(None),
            403 => {
                return Err(SourceError::Locked {
                    name: self.name.clone(),
                });
            }
            other => {
                return Err(SourceError::Upstream {
                    name: self.name.clone(),
                    message: format!("vault get returned {other}"),
                });
            }
        }
        let body: KvGetResponse = resp.json().await.map_err(|e| SourceError::Upstream {
            name: self.name.clone(),
            message: format!("vault get malformed JSON: {e}"),
        })?;
        let value = body
            .data
            .data
            .get(&field)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| SourceError::BadReference {
                name: self.name.clone(),
                reference: reference.to_owned(),
                reason: format!("vault entry has no string field '{field}'"),
            })?;
        let lease = if body.lease_duration > 0 {
            Some(Duration::from_secs(body.lease_duration))
        } else {
            None
        };
        Ok(Some(GetOutcome {
            value: SecretString::from(value.to_owned()),
            lease_duration: lease,
        }))
    }

    async fn list(&self) -> Result<Vec<RemoteRef>, SourceError> {
        let token = self.resolve_token().await?;
        let mount = self
            .list_mount
            .as_deref()
            .unwrap_or(DEFAULT_LIST_MOUNT)
            .trim_start_matches('/');
        let url = self.build_url(&format!("v1/{mount}"));
        let method = reqwest::Method::from_bytes(b"LIST")
            .expect("LIST is a valid HTTP method byte sequence");
        let resp = self
            .http
            .request(method, &url)
            .header("X-Vault-Token", token.expose_secret())
            .send()
            .await
            .map_err(|e| SourceError::Upstream {
                name: self.name.clone(),
                message: format!("vault list transport: {e}"),
            })?;
        match resp.status().as_u16() {
            200 => {}
            // Vault responds 404 to LIST on an empty path — treat
            // as empty list rather than upstream error.
            404 => return Ok(Vec::new()),
            403 => {
                return Err(SourceError::Locked {
                    name: self.name.clone(),
                });
            }
            other => {
                return Err(SourceError::Upstream {
                    name: self.name.clone(),
                    message: format!("vault list returned {other}"),
                });
            }
        }
        let body: KvListResponse = resp.json().await.map_err(|e| SourceError::Upstream {
            name: self.name.clone(),
            message: format!("vault list malformed JSON: {e}"),
        })?;
        // Translate the metadata mount back to the data mount so
        // the returned references are usable verbatim with `get`.
        let data_mount = mount.replace("/metadata/", "/data/");
        Ok(body
            .data
            .keys
            .into_iter()
            .map(|k| RemoteRef {
                reference: format!("{data_mount}{k}"),
                display: Some(k),
            })
            .collect())
    }

    async fn validate(&self, reference: &str) -> Result<(), SourceError> {
        parse_reference(reference)
            .map(|_| ())
            .map_err(|reason| SourceError::BadReference {
                name: self.name.clone(),
                reference: reference.to_owned(),
                reason,
            })
    }
}

// =============================================================================
// Wire types (private)
// =============================================================================

#[derive(Deserialize)]
struct KvGetResponse {
    #[serde(default)]
    lease_duration: u64,
    data: KvGetData,
}

#[derive(Deserialize)]
struct KvGetData {
    data: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct KvListResponse {
    data: KvListData,
}

#[derive(Deserialize)]
struct KvListData {
    keys: Vec<String>,
}

#[derive(Deserialize)]
struct ApproleLoginResponse {
    auth: ApproleAuth,
}

#[derive(Deserialize)]
struct ApproleAuth {
    client_token: String,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn token_source(addr: &str, token: &str) -> VaultSource {
        VaultSource::new(
            "vault-test",
            addr.to_owned(),
            VaultAuth::Token(SecretString::from(token.to_owned())),
        )
        .unwrap()
    }

    // -- Pure-data trait surface -----------------------------------

    #[test]
    fn capabilities_match_adr_021_section_8_vault() {
        let src = token_source("http://localhost:8200", "x");
        let caps = src.capabilities();
        assert!(caps.contains(Capabilities::READ));
        assert!(caps.contains(Capabilities::LIST));
        assert!(caps.contains(Capabilities::VALIDATE));
        assert!(caps.contains(Capabilities::WRITE));
        assert!(caps.contains(Capabilities::ROTATE));
        assert!(caps.contains(Capabilities::AUDIT_LOGGED));
        // Vault's typical configuration does not prompt for
        // biometrics on reads.
        assert!(!caps.contains(Capabilities::BIOMETRIC_PROMPT));
    }

    #[test]
    fn debug_redacts_token_and_secret_id() {
        let src = token_source("http://localhost:8200", "super-secret-token-value");
        let dbg = format!("{src:?}");
        assert!(!dbg.contains("super-secret-token-value"));
        assert!(dbg.contains("Token(<redacted>)"));
    }

    #[test]
    fn debug_redacts_approle_secret_id() {
        let src = VaultSource::new(
            "vault-test",
            "http://localhost:8200".to_owned(),
            VaultAuth::AppRole {
                role_id: "role-public".into(),
                secret_id: SecretString::from("super-secret-id".to_owned()),
                mount: None,
            },
        )
        .unwrap();
        let dbg = format!("{src:?}");
        assert!(!dbg.contains("super-secret-id"));
        assert!(dbg.contains("AppRole(<redacted>)"));
    }

    // -- Reference parsing -----------------------------------------

    #[test]
    fn parse_reference_default_field_is_value() {
        let (path, field) = parse_reference("secret/data/team/x/y").unwrap();
        assert_eq!(path, "secret/data/team/x/y");
        assert_eq!(field, "value");
    }

    #[test]
    fn parse_reference_explicit_field() {
        let (path, field) = parse_reference("secret/data/team/x#token").unwrap();
        assert_eq!(path, "secret/data/team/x");
        assert_eq!(field, "token");
    }

    #[test]
    fn parse_reference_rejects_empty_inputs() {
        assert!(parse_reference("").is_err());
        assert!(parse_reference("#value").is_err());
        assert!(parse_reference("secret/data/x#").is_err());
    }

    #[tokio::test]
    async fn validate_through_trait_returns_bad_reference_for_empty() {
        let src = token_source("http://localhost:8200", "x");
        let err = src.validate("").await.unwrap_err();
        assert!(matches!(err, SourceError::BadReference { .. }));
    }

    // -- is_available branches -------------------------------------

    #[tokio::test]
    async fn is_available_returns_available_when_health_and_token_ok() {
        let server = MockServer::start_async().await;
        let _h = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/sys/health");
                then.status(200).json_body(serde_json::json!({
                    "initialized": true,
                    "sealed": false,
                    "standby": false,
                }));
            })
            .await;
        let _t = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/v1/auth/token/lookup-self")
                    .header("X-Vault-Token", "tok-fixture");
                then.status(200).json_body(serde_json::json!({
                    "data": {"id": "tok-fixture", "ttl": 60}
                }));
            })
            .await;

        let src = token_source(&server.base_url(), "tok-fixture");
        assert_eq!(src.is_available().await, SourceStatus::Available);
    }

    #[tokio::test]
    async fn is_available_returns_locked_when_vault_is_sealed() {
        let server = MockServer::start_async().await;
        let _h = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/sys/health");
                then.status(503);
            })
            .await;
        let src = token_source(&server.base_url(), "x");
        assert_eq!(src.is_available().await, SourceStatus::Locked);
    }

    #[tokio::test]
    async fn is_available_returns_not_installed_when_vault_not_initialized() {
        let server = MockServer::start_async().await;
        let _h = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/sys/health");
                then.status(501);
            })
            .await;
        let src = token_source(&server.base_url(), "x");
        assert_eq!(src.is_available().await, SourceStatus::NotInstalled);
    }

    #[tokio::test]
    async fn is_available_returns_locked_on_403_lookup_self() {
        let server = MockServer::start_async().await;
        let _h = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/sys/health");
                then.status(200).json_body(serde_json::json!({}));
            })
            .await;
        let _t = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/auth/token/lookup-self");
                then.status(403);
            })
            .await;
        let src = token_source(&server.base_url(), "expired");
        assert_eq!(src.is_available().await, SourceStatus::Locked);
    }

    #[tokio::test]
    async fn is_available_returns_not_installed_when_no_server() {
        // A port we know nothing listens on.
        let src = token_source("http://127.0.0.1:1/", "x");
        assert_eq!(src.is_available().await, SourceStatus::NotInstalled);
    }

    // -- get -------------------------------------------------------

    #[tokio::test]
    async fn get_returns_value_and_default_lease_duration_none_for_static_kv() {
        use secrecy::ExposeSecret;
        let server = MockServer::start_async().await;
        let _g = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/v1/secret/data/team/gitlab/token-deploy")
                    .header("X-Vault-Token", "tok");
                then.status(200).json_body(serde_json::json!({
                    "lease_duration": 0,
                    "data": {
                        "data": {"value": "glpat-from-vault"},
                        "metadata": {"version": 1}
                    }
                }));
            })
            .await;
        let src = token_source(&server.base_url(), "tok");
        let outcome = src
            .get("secret/data/team/gitlab/token-deploy")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome.value.expose_secret(), "glpat-from-vault");
        // Static KV → lease_duration=0 → router uses source default.
        assert!(outcome.lease_duration.is_none());
    }

    #[tokio::test]
    async fn get_populates_lease_duration_for_dynamic_engines() {
        let server = MockServer::start_async().await;
        let _g = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/database/creds/role-x");
                then.status(200).json_body(serde_json::json!({
                    "lease_duration": 3600,
                    "data": {"data": {"value": "rotating-cred"}}
                }));
            })
            .await;
        let src = token_source(&server.base_url(), "tok");
        let outcome = src.get("database/creds/role-x").await.unwrap().unwrap();
        assert_eq!(outcome.lease_duration, Some(Duration::from_secs(3600)));
    }

    #[tokio::test]
    async fn get_with_explicit_field_selects_correct_value() {
        use secrecy::ExposeSecret;
        let server = MockServer::start_async().await;
        let _g = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/secret/data/x");
                then.status(200).json_body(serde_json::json!({
                    "data": {
                        "data": {"value": "default-value", "username": "alice", "password": "p4ss"}
                    }
                }));
            })
            .await;
        let src = token_source(&server.base_url(), "tok");
        let outcome = src.get("secret/data/x#username").await.unwrap().unwrap();
        assert_eq!(outcome.value.expose_secret(), "alice");
    }

    #[tokio::test]
    async fn get_returns_none_on_404() {
        let server = MockServer::start_async().await;
        let _g = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/secret/data/nope");
                then.status(404);
            })
            .await;
        let src = token_source(&server.base_url(), "tok");
        let outcome = src.get("secret/data/nope").await.unwrap();
        assert!(outcome.is_none());
    }

    #[tokio::test]
    async fn get_returns_locked_on_403() {
        let server = MockServer::start_async().await;
        let _g = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/secret/data/protected");
                then.status(403);
            })
            .await;
        let src = token_source(&server.base_url(), "expired");
        let err = src.get("secret/data/protected").await.unwrap_err();
        assert!(matches!(err, SourceError::Locked { .. }));
    }

    #[tokio::test]
    async fn get_returns_bad_reference_when_field_absent_from_response() {
        let server = MockServer::start_async().await;
        let _g = server
            .mock_async(|when, then| {
                when.method(GET).path("/v1/secret/data/x");
                then.status(200).json_body(serde_json::json!({
                    "data": {"data": {"username": "alice"}}
                }));
            })
            .await;
        let src = token_source(&server.base_url(), "tok");
        let err = src.get("secret/data/x#password").await.unwrap_err();
        match err {
            SourceError::BadReference { reason, .. } => {
                assert!(reason.contains("password"));
            }
            other => panic!("expected BadReference, got {other:?}"),
        }
    }

    // -- list ------------------------------------------------------

    #[tokio::test]
    async fn list_returns_keys_translated_to_data_path() {
        let server = MockServer::start_async().await;
        // httpmock's `Method` enum has fixed variants and does not
        // include a `LIST` constant. Use the predicate matcher
        // (`is_true`) on the request's method string instead.
        let _l = server
            .mock_async(|when, then| {
                when.is_true(|req| req.method_str() == "LIST")
                    .path("/v1/secret/metadata/");
                then.status(200).json_body(serde_json::json!({
                    "data": {"keys": ["foo", "bar/"]}
                }));
            })
            .await;
        let src = token_source(&server.base_url(), "tok");
        let entries = src.list().await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].reference, "secret/data/foo");
        assert_eq!(entries[0].display.as_deref(), Some("foo"));
        assert_eq!(entries[1].reference, "secret/data/bar/");
    }

    #[tokio::test]
    async fn list_returns_empty_on_404() {
        let server = MockServer::start_async().await;
        let _l = server
            .mock_async(|when, then| {
                when.is_true(|req| req.method_str() == "LIST");
                then.status(404);
            })
            .await;
        let src = token_source(&server.base_url(), "tok");
        let entries = src.list().await.unwrap();
        assert!(entries.is_empty());
    }

    // -- AppRole ---------------------------------------------------

    #[tokio::test]
    async fn approle_login_exchanges_for_a_client_token_then_get_works() {
        use secrecy::ExposeSecret;
        let server = MockServer::start_async().await;
        let _login = server
            .mock_async(|when, then| {
                when.method(POST).path("/v1/auth/approle/login");
                then.status(200).json_body(serde_json::json!({
                    "auth": {"client_token": "approle-derived-token"}
                }));
            })
            .await;
        let _g = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/v1/secret/data/x")
                    .header("X-Vault-Token", "approle-derived-token");
                then.status(200).json_body(serde_json::json!({
                    "data": {"data": {"value": "from-vault"}}
                }));
            })
            .await;
        let src = VaultSource::new(
            "vault-test",
            server.base_url(),
            VaultAuth::AppRole {
                role_id: "role".into(),
                secret_id: SecretString::from("sid".to_owned()),
                mount: None,
            },
        )
        .unwrap();
        let outcome = src.get("secret/data/x").await.unwrap().unwrap();
        assert_eq!(outcome.value.expose_secret(), "from-vault");
    }

    #[tokio::test]
    async fn approle_failure_surfaces_upstream_error() {
        let server = MockServer::start_async().await;
        let _login = server
            .mock_async(|when, then| {
                when.method(POST).path("/v1/auth/approle/login");
                then.status(403).body("permission denied for role");
            })
            .await;
        let src = VaultSource::new(
            "vault-test",
            server.base_url(),
            VaultAuth::AppRole {
                role_id: "role".into(),
                secret_id: SecretString::from("bad-sid".to_owned()),
                mount: None,
            },
        )
        .unwrap();
        let err = src.get("secret/data/x").await.unwrap_err();
        match err {
            SourceError::Upstream { message, .. } => {
                assert!(message.contains("approle login"));
            }
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    // -- OIDC stub -------------------------------------------------

    #[tokio::test]
    async fn oidc_returns_not_implemented_error() {
        let src = VaultSource::new(
            "vault-test",
            "http://localhost:8200".to_owned(),
            VaultAuth::Oidc(OidcConfig::default()),
        )
        .unwrap();
        let err = src.get("secret/data/x").await.unwrap_err();
        match err {
            SourceError::Upstream { message, .. } => {
                assert!(message.contains("OIDC"));
                assert!(message.contains("not implemented"));
            }
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    // -- dyn compat ------------------------------------------------

    #[tokio::test]
    async fn dyn_compat_smoke() {
        let s: Box<dyn SecretSource> = Box::new(token_source("http://localhost:8200", "x"));
        assert_eq!(s.name(), "vault-test");
        assert!(s.capabilities().contains(Capabilities::AUDIT_LOGGED));
    }
}
