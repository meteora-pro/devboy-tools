//! GitHub `LivenessProbe` impl per [ADR-021] §6.
//!
//! Hits `GET /user` with the supplied token. The endpoint
//! authenticates the caller and returns the public profile, so a
//! 200 means the token is live and the response carries enough
//! detail (`login`) for `doctor` to render
//! "live as alice@github.com".
//!
//! Token expiry is reported through the
//! `github-authentication-token-expiration` response header
//! (lowercased per RFC 9110 / hyper conventions). When present
//! it goes into [`LivenessResult::expires_at`] verbatim so the
//! P9.3 expiry-tracking pass can write it back into the
//! global index.
//!
//! Error mapping:
//!
//! | Status | Mapping                            |
//! |--------|------------------------------------|
//! | 200    | `Live` (login captured)            |
//! | 401    | `Revoked`                          |
//! | 403    | `Revoked` (insufficient-permission case still means the token can't probe) |
//! | 429    | `Throttled` (`Retry-After` if any)|
//! | other  | `Error` (status + body)            |
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use async_trait::async_trait;
use devboy_core::{Error, LivenessProbe, LivenessResult, Result, liveness::LivenessStatus};
use secrecy::{ExposeSecret, SecretString};

use crate::client::GitHubClient;

const TOKEN_EXPIRATION_HEADER: &str = "github-authentication-token-expiration";

#[async_trait]
impl LivenessProbe for GitHubClient {
    fn provider_name(&self) -> &str {
        "github"
    }

    async fn test(&self, token: &SecretString) -> Result<LivenessResult> {
        let url = format!("{}/user", self.base_url());
        let resp = self
            .http_client()
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("Authorization", format!("Bearer {}", token.expose_secret()))
            .send()
            .await
            .map_err(|e| Error::Http(format!("github liveness GET /user: {e}")))?;

        let expires_at = resp
            .headers()
            .get(TOKEN_EXPIRATION_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        let status = resp.status();
        match status.as_u16() {
            200 => {
                #[derive(serde::Deserialize)]
                struct User {
                    login: Option<String>,
                }
                let body: User = resp.json().await.unwrap_or(User { login: None });
                let detail = body.login.unwrap_or_else(|| "github user".to_owned());
                Ok(LivenessResult {
                    status: LivenessStatus::Live,
                    detail: Some(detail),
                    expires_at,
                })
            }
            401 => Ok(LivenessResult::revoked(
                "github rejected the token (401 Unauthorized)",
            )),
            403 => Ok(LivenessResult::revoked(
                "github refused the token (403 Forbidden) — revoked or insufficient scope",
            )),
            429 => {
                let retry = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
                    .unwrap_or_else(|| "unknown".to_owned());
                Ok(LivenessResult::throttled(format!(
                    "github rate limit exceeded; retry-after: {retry}"
                )))
            }
            other => {
                let body = resp.text().await.unwrap_or_default();
                Ok(LivenessResult::error(format!(
                    "github GET /user returned {other}: {}",
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

    fn client_against(base_url: &str) -> GitHubClient {
        GitHubClient::with_base_url(
            base_url,
            "owner",
            "repo",
            SecretString::from("ignored-self-token".to_owned()),
        )
    }

    #[tokio::test]
    async fn live_token_returns_login_and_optional_expiry() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/user")
                    .header("Authorization", "Bearer ghp_fixture");
                then.status(200)
                    .header(
                        "github-authentication-token-expiration",
                        "2026-08-01 00:00:00 UTC",
                    )
                    .json_body(serde_json::json!({"login": "octocat"}));
            })
            .await;

        let client = client_against(&server.base_url());
        let r = client
            .test(&SecretString::from("ghp_fixture".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Live);
        assert_eq!(r.detail.as_deref(), Some("octocat"));
        assert_eq!(
            r.expires_at.as_deref(),
            Some("2026-08-01 00:00:00 UTC"),
            "expiry header must be plumbed through to result"
        );
    }

    #[tokio::test]
    async fn revoked_on_401() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/user");
                then.status(401)
                    .json_body(serde_json::json!({"message": "Bad credentials"}));
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
                when.method(GET).path("/user");
                then.status(403);
            })
            .await;
        let client = client_against(&server.base_url());
        let r = client
            .test(&SecretString::from("revoked".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Revoked);
    }

    #[tokio::test]
    async fn throttled_on_429_carries_retry_after() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/user");
                then.status(429).header("Retry-After", "60");
            })
            .await;
        let client = client_against(&server.base_url());
        let r = client
            .test(&SecretString::from("over-limit".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Throttled);
        assert!(r.detail.unwrap().contains("60"));
    }

    #[tokio::test]
    async fn unexpected_status_classified_as_error() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/user");
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
    fn provider_name_is_github() {
        let client = GitHubClient::new("owner", "repo", SecretString::from("any".to_owned()));
        assert_eq!(client.provider_name(), "github");
    }
}
