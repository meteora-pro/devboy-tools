//! Per-upstream OAuth token holder with transparent, single-flight refresh
//! (issue #306). Wired into the proxy request path so `auth_type = "oauth2"`
//! upstreams inject a fresh Bearer per request and recover from 401s by
//! refreshing — no manual re-login until the refresh token itself expires.

use std::sync::Arc;

use chrono::{Duration, Utc};
use devboy_core::oauth::{self, OAuthError, OAuthTokens};
use devboy_storage::CredentialStore;
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::{Mutex, RwLock};

/// Refresh this many seconds before the access token actually expires.
const EXPIRY_SKEW_SECS: i64 = 60;

/// Holds the live OAuth token set for one proxy upstream and refreshes it
/// transparently. The DevBoy AS **rotates** refresh_tokens, so:
/// - refreshes are serialized behind a single-flight mutex: concurrent 401s
///   trigger exactly one refresh;
/// - the rotated pair is persisted to the credential store *before* the
///   in-memory swap, so a crash mid-refresh never strands a spent token.
pub struct OAuthAuth {
    tokens: RwLock<OAuthTokens>,
    client_id: String,
    token_endpoint: String,
    /// RFC 8707 resource indicator (the MCP server URL) — sent on refresh so the
    /// rotated token keeps the same audience.
    resource: String,
    gate: Mutex<()>,
    store_key: String,
    http: reqwest::Client,
    store: Arc<dyn CredentialStore>,
}

impl OAuthAuth {
    pub fn new(
        tokens: OAuthTokens,
        client_id: String,
        token_endpoint: String,
        resource: String,
        store_key: String,
        store: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            tokens: RwLock::new(tokens),
            client_id,
            token_endpoint,
            resource,
            gate: Mutex::new(()),
            store_key,
            // No redirect-following: the token endpoint carries the rotating
            // refresh token, so a 302 must never silently move it to another host.
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            store,
        }
    }

    /// Access token for a request's `Authorization` header. Refreshes pre-flight
    /// when the current token is within the expiry skew.
    pub async fn access_token(&self) -> Result<String, OAuthError> {
        let (near, seen) = {
            let t = self.tokens.read().await;
            (
                t.is_near_expiry(Utc::now(), Duration::seconds(EXPIRY_SKEW_SECS)),
                t.access_token.expose_secret().to_string(),
            )
        };
        if near {
            self.refresh(&seen).await?;
        }
        Ok(self
            .tokens
            .read()
            .await
            .access_token
            .expose_secret()
            .to_string())
    }

    /// Refresh the token pair — single-flight AND store-reconciled.
    ///
    /// The DevBoy AS rotates the refresh token, and a multi-agent setup runs
    /// several `devboy` processes (one proxy per agent) against the **same**
    /// store key. So under the gate we first reconcile with the credential
    /// store, keyed on the *refresh* token (holds even if the AS reissues the
    /// same access token): if another task or process already rotated, we adopt
    /// its persisted pair instead of spending our now-dead refresh token and
    /// killing the session. Only if nobody moved do we refresh, persisting
    /// before the in-memory swap. Call on a 401 with the access token actually
    /// sent, or pre-flight near expiry.
    pub async fn refresh(&self, seen: &str) -> Result<(), OAuthError> {
        let _g = self.gate.lock().await;

        let our_refresh = self
            .tokens
            .read()
            .await
            .refresh_token
            .expose_secret()
            .to_string();
        // (1) Cross-task / cross-process reconcile: another holder of this store
        //     key already rotated. Adopt their pair; never re-spend our refresh.
        if let Some(stored) = self.load_stored()
            && stored.refresh_token.expose_secret() != our_refresh
        {
            *self.tokens.write().await = stored;
            return Ok(());
        }
        // (2) Same-process double-check: a concurrent task refreshed in memory
        //     while we waited on the gate (access token already moved past seen).
        if self.tokens.read().await.access_token.expose_secret() != seen {
            return Ok(());
        }

        // (3) We own the refresh.
        let resp = match oauth::refresh(
            &self.http,
            &self.token_endpoint,
            &our_refresh,
            &self.client_id,
            Some(&self.resource),
        )
        .await
        {
            Ok(resp) => resp,
            // Lost a simultaneous cross-process race: our refresh token was
            // spent by the winner. Adopt what it persisted rather than surfacing
            // a spurious re-login prompt.
            Err(OAuthError::Oauth(ref code)) if code.contains("invalid_grant") => {
                if let Some(stored) = self.load_stored()
                    && stored.refresh_token.expose_secret() != our_refresh
                {
                    *self.tokens.write().await = stored;
                    return Ok(());
                }
                return Err(OAuthError::Oauth(code.clone()));
            }
            Err(e) => return Err(e),
        };
        let new = OAuthTokens::from_response(resp, Utc::now(), Some(our_refresh.as_str()))?;
        // Persist FIRST — the old refresh_token is now deactivated server-side,
        // so the new pair must reach durable storage before the in-memory swap.
        // If persist fails we return Err without swapping: the old refresh is
        // already dead upstream, so the next refresh hits `invalid_grant` and
        // surfaces a re-login prompt. No silent corruption, but the session is
        // lost — acceptable for a rare keychain-write failure.
        let json = serde_json::to_string(&new).map_err(|e| OAuthError::Malformed(e.to_string()))?;
        self.store
            .store(&self.store_key, &SecretString::from(json))
            .map_err(|e| OAuthError::Http(format!("persist tokens: {e}")))?;
        *self.tokens.write().await = new;
        Ok(())
    }

    /// Reload the persisted token set, if present and well-formed. Used under
    /// the gate to reconcile with other tasks/processes on the same store key.
    fn load_stored(&self) -> Option<OAuthTokens> {
        match self.store.get(&self.store_key) {
            Ok(Some(secret)) => serde_json::from_str::<OAuthTokens>(secret.expose_secret()).ok(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_storage::MemoryStore;
    use httpmock::prelude::*;

    fn tokens(access: &str, expires_at: chrono::DateTime<Utc>) -> OAuthTokens {
        OAuthTokens {
            access_token: access.into(),
            refresh_token: "rt-old".into(),
            expires_at,
        }
    }

    #[tokio::test]
    async fn refresh_rotates_and_persists_before_swap() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(POST).path("/token");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        r#"{"access_token":"at-new","refresh_token":"rt-new","expires_in":3600}"#,
                    );
            })
            .await;
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
        let auth = OAuthAuth::new(
            tokens("at-old", Utc::now()),
            "cli".into(),
            format!("{}/token", server.base_url()),
            "https://rs.example/mcp".into(),
            "proxy.x.oauth".into(),
            store.clone(),
        );
        auth.refresh("at-old").await.unwrap();
        assert_eq!(auth.access_token().await.unwrap(), "at-new");
        // Rotated pair reached the store (content is a SecretString — presence
        // is enough here; the persist-before-swap ordering is enforced in code).
        assert!(
            store.exists("proxy.x.oauth"),
            "rotated tokens should be persisted"
        );
    }

    #[tokio::test]
    async fn refresh_adopts_store_when_another_process_already_rotated() {
        // The store already holds a newer pair (as if another devboy process
        // sharing this key rotated). The endpoint is unreachable, so if refresh
        // tried the network it would error — it must instead adopt the stored
        // pair, keyed on the refresh token (works even though the access token
        // also differs). Covers the cross-process rotation race + the
        // same-access-token double-check gap.
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
        let newer = OAuthTokens {
            access_token: "at-fromstore".into(),
            refresh_token: "rt-newer".into(),
            expires_at: Utc::now() + Duration::seconds(3600),
        };
        store
            .store(
                "proxy.x.oauth",
                &SecretString::from(serde_json::to_string(&newer).unwrap()),
            )
            .unwrap();
        let auth = OAuthAuth::new(
            tokens("at-old", Utc::now()), // in-memory refresh is "rt-old"
            "cli".into(),
            "http://127.0.0.1:1/token".into(), // unreachable — must not be hit
            "https://rs.example/mcp".into(),
            "proxy.x.oauth".into(),
            store,
        );
        auth.refresh("at-old").await.unwrap();
        assert_eq!(auth.access_token().await.unwrap(), "at-fromstore");
    }

    #[tokio::test]
    async fn refresh_double_check_skips_when_already_rotated() {
        // token far from expiry, endpoint unreachable — if the double-check
        // fails to short-circuit, the bogus URL makes the test error out.
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
        let auth = OAuthAuth::new(
            tokens("at-current", Utc::now() + Duration::seconds(3600)),
            "cli".into(),
            "http://127.0.0.1:1/token".into(),
            "https://rs.example/mcp".into(),
            "k".into(),
            store,
        );
        // seen != current access → treated as already-refreshed → no HTTP call
        auth.refresh("at-stale").await.unwrap();
        assert_eq!(auth.access_token().await.unwrap(), "at-current");
    }

    #[tokio::test]
    async fn on_401_sequence_yields_a_fresh_bearer() {
        // Mirrors exactly what request_http/request_sse do on a 401:
        //   sent = access_token();  // pre-flight (token valid → unchanged)
        //   ...POST returns 401...
        //   refresh(sent);          // single-flight refresh
        //   retry_bearer = access_token();  // now the rotated token
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(POST).path("/token");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        r#"{"access_token":"at-new","refresh_token":"rt-new","expires_in":3600}"#,
                    );
            })
            .await;
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
        let auth = OAuthAuth::new(
            // valid, far from expiry → no pre-flight refresh, mirrors a live 401
            tokens("at-old", Utc::now() + Duration::seconds(3600)),
            "cli".into(),
            format!("{}/token", server.base_url()),
            "https://rs.example/mcp".into(),
            "proxy.x.oauth".into(),
            store,
        );
        let sent = auth.access_token().await.unwrap();
        assert_eq!(sent, "at-old"); // pre-flight leaves the valid token alone
        auth.refresh(&sent).await.unwrap(); // the 401 path
        assert_eq!(auth.access_token().await.unwrap(), "at-new"); // retry uses rotated token
    }
}
