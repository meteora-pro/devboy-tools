//! GitLab `LivenessProbe` impl per [ADR-021] §6.
//!
//! Hits `GET /api/v4/personal_access_tokens/self` with the
//! supplied token. The endpoint returns the token's own metadata
//! when authenticated:
//!
//! ```json
//! {
//!   "id": 1,
//!   "name": "deploy",
//!   "active": true,
//!   "expires_at": "2026-08-01",
//!   "scopes": ["api", "read_repository"]
//! }
//! ```
//!
//! `expires_at` (when present) is plumbed straight into
//! [`LivenessResult::expires_at`] so P9.3 can write it back into
//! the global index. `active = false` maps to
//! [`LivenessStatus::Revoked`].
//!
//! Error mapping:
//!
//! | Status | Mapping                            |
//! |--------|------------------------------------|
//! | 200, `active=true` | `Live`                  |
//! | 200, `active=false` | `Revoked`              |
//! | 401    | `Revoked`                          |
//! | 403    | `Revoked`                          |
//! | 429    | `Throttled` (`Retry-After`)        |
//! | other  | `Error`                            |
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use async_trait::async_trait;
use devboy_core::{Error, LivenessProbe, LivenessResult, Result, liveness::LivenessStatus};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::client::GitLabClient;

#[derive(Deserialize)]
struct TokenSelf {
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "default_active")]
    active: bool,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

fn default_active() -> bool {
    // GitLab returns `active` on every token-self payload; default
    // is defensive in case a fork omits the field.
    true
}

#[async_trait]
impl LivenessProbe for GitLabClient {
    fn provider_name(&self) -> &str {
        "gitlab"
    }

    async fn test(&self, token: &SecretString) -> Result<LivenessResult> {
        let url = format!("{}/api/v4/personal_access_tokens/self", self.base_url());
        let resp = self
            .http_client()
            .get(&url)
            .header("PRIVATE-TOKEN", token.expose_secret())
            .send()
            .await
            .map_err(|e| {
                Error::Http(format!(
                    "gitlab liveness GET /personal_access_tokens/self: {e}"
                ))
            })?;

        let status = resp.status();
        match status.as_u16() {
            200 => {
                let body: TokenSelf = resp.json().await.map_err(|e| {
                    Error::InvalidData(format!(
                        "gitlab personal_access_tokens/self returned non-JSON body: {e}"
                    ))
                })?;
                if !body.active {
                    return Ok(LivenessResult {
                        status: LivenessStatus::Revoked,
                        detail: Some(
                            "gitlab marked the token as inactive (active=false)".to_owned(),
                        ),
                        expires_at: body.expires_at,
                    });
                }
                let scope_summary = body
                    .scopes
                    .as_ref()
                    .map(|s| s.join(","))
                    .unwrap_or_default();
                let detail = match (body.name.as_deref(), scope_summary.as_str()) {
                    (Some(n), s) if !s.is_empty() => Some(format!("{n} ({s})")),
                    (Some(n), _) => Some(n.to_owned()),
                    (None, s) if !s.is_empty() => Some(s.to_owned()),
                    _ => Some("gitlab personal access token".to_owned()),
                };
                Ok(LivenessResult {
                    status: LivenessStatus::Live,
                    detail,
                    expires_at: body.expires_at,
                })
            }
            401 => Ok(LivenessResult::revoked(
                "gitlab rejected the token (401 Unauthorized)",
            )),
            403 => Ok(LivenessResult::revoked(
                "gitlab refused the token (403 Forbidden) — revoked or scope-restricted",
            )),
            429 => {
                let retry = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
                    .unwrap_or_else(|| "unknown".to_owned());
                Ok(LivenessResult::throttled(format!(
                    "gitlab rate limit exceeded; retry-after: {retry}"
                )))
            }
            other => {
                let body = resp.text().await.unwrap_or_default();
                Ok(LivenessResult::error(format!(
                    "gitlab GET /personal_access_tokens/self returned {other}: {}",
                    body.trim()
                )))
            }
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn client_against(base_url: &str) -> GitLabClient {
        GitLabClient::with_base_url(
            base_url,
            "1",
            SecretString::from("ignored-self-token".to_owned()),
        )
    }

    #[tokio::test]
    async fn live_token_returns_name_scopes_and_expiry() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api/v4/personal_access_tokens/self")
                    .header("PRIVATE-TOKEN", "glpat-fixture");
                then.status(200).json_body(serde_json::json!({
                    "id": 7,
                    "name": "deploy",
                    "active": true,
                    "expires_at": "2026-08-01",
                    "scopes": ["api", "read_repository"]
                }));
            })
            .await;

        let client = client_against(&server.base_url());
        let r = client
            .test(&SecretString::from("glpat-fixture".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Live);
        let detail = r.detail.unwrap();
        assert!(detail.contains("deploy"));
        assert!(detail.contains("api"));
        assert!(detail.contains("read_repository"));
        assert_eq!(r.expires_at.as_deref(), Some("2026-08-01"));
    }

    #[tokio::test]
    async fn inactive_token_returns_revoked_with_expiry() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/api/v4/personal_access_tokens/self");
                then.status(200).json_body(serde_json::json!({
                    "id": 7,
                    "name": "old",
                    "active": false,
                    "expires_at": "2025-12-01",
                    "scopes": []
                }));
            })
            .await;

        let client = client_against(&server.base_url());
        let r = client
            .test(&SecretString::from("revoked".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Revoked);
        // Even on revoked, surface the expiry — useful for
        // doctor and the migration flow.
        assert_eq!(r.expires_at.as_deref(), Some("2025-12-01"));
    }

    #[tokio::test]
    async fn revoked_on_401() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/api/v4/personal_access_tokens/self");
                then.status(401)
                    .json_body(serde_json::json!({"message": "401 Unauthorized"}));
            })
            .await;
        let client = client_against(&server.base_url());
        let r = client
            .test(&SecretString::from("bad".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Revoked);
    }

    #[tokio::test]
    async fn revoked_on_403() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/api/v4/personal_access_tokens/self");
                then.status(403);
            })
            .await;
        let client = client_against(&server.base_url());
        let r = client
            .test(&SecretString::from("bad".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Revoked);
    }

    #[tokio::test]
    async fn throttled_on_429_carries_retry_after() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/api/v4/personal_access_tokens/self");
                then.status(429).header("Retry-After", "30");
            })
            .await;
        let client = client_against(&server.base_url());
        let r = client
            .test(&SecretString::from("over-limit".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Throttled);
        assert!(r.detail.unwrap().contains("30"));
    }

    #[tokio::test]
    async fn unexpected_status_classified_as_error() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/api/v4/personal_access_tokens/self");
                then.status(503).body("upstream down");
            })
            .await;
        let client = client_against(&server.base_url());
        let r = client
            .test(&SecretString::from("any".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Error);
    }

    #[test]
    fn provider_name_is_gitlab() {
        let client = GitLabClient::new("1", SecretString::from("any".to_owned()));
        assert_eq!(client.provider_name(), "gitlab");
    }
}
