//! Confluence `LivenessProbe` impl per [ADR-021] §6.
//!
//! Self-hosted / Server / DC probes `GET /rest/api/user/current`.
//! Confluence Cloud uses two paths depending on auth:
//!
//! - Basic auth (`email + API token`): probe
//!   `GET <site>/wiki/rest/api/user/current`
//! - Bearer auth (Atlassian OAuth): discover the matching
//!   `cloud_id` via Atlassian `accessible-resources`, then probe
//!   `GET https://api.atlassian.com/ex/confluence/{cloud_id}/wiki/api/v2/space?limit=1`
//!
//! The Cloud bearer path does not return a user profile directly, so
//! the liveness detail reports the matched site origin / cloud id
//! rather than a username.

use async_trait::async_trait;
use devboy_core::{Error, LivenessProbe, LivenessResult, Result};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::client::{ConfluenceAuth, ConfluenceClient, ConfluenceFlavor};

#[derive(Debug, Deserialize)]
struct ConfluenceCurrentUser {
    #[serde(default, rename = "username")]
    username: Option<String>,
    #[serde(default, rename = "displayName")]
    display_name: Option<String>,
    #[serde(default, rename = "accountId")]
    account_id: Option<String>,
    #[serde(default, rename = "email")]
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AtlassianAccessibleResource {
    id: String,
    #[serde(default)]
    url: Option<String>,
}

#[async_trait]
impl LivenessProbe for ConfluenceClient {
    fn provider_name(&self) -> &str {
        "confluence"
    }

    async fn test(&self, token: &SecretString) -> Result<LivenessResult> {
        let http = reqwest::Client::new();
        match self.flavor() {
            ConfluenceFlavor::SelfHosted => {
                probe_current_user(
                    &http,
                    &format!("{}/rest/api/user/current", self.base_url()),
                    self.auth(),
                    token,
                    "confluence",
                )
                .await
            }
            ConfluenceFlavor::Cloud => match self.auth() {
                ConfluenceAuth::Basic { .. } => {
                    probe_current_user(
                        &http,
                        &format!("{}/wiki/rest/api/user/current", self.base_url()),
                        self.auth(),
                        token,
                        "confluence cloud",
                    )
                    .await
                }
                ConfluenceAuth::BearerToken(_) | ConfluenceAuth::None => {
                    probe_cloud_bearer(&http, self, token).await
                }
            },
        }
    }
}

async fn probe_current_user(
    http: &reqwest::Client,
    url: &str,
    auth: &ConfluenceAuth,
    token: &SecretString,
    provider_label: &str,
) -> Result<LivenessResult> {
    let mut request = http
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json");
    request = apply_auth(request, auth, token);

    let resp = request
        .send()
        .await
        .map_err(|e| Error::Http(format!("{provider_label} liveness GET {url}: {e}")))?;

    let status = resp.status();
    match status.as_u16() {
        200 => {
            let body: ConfluenceCurrentUser = resp.json().await.map_err(|e| {
                Error::InvalidData(format!(
                    "{provider_label} user/current returned non-JSON body: {e}"
                ))
            })?;
            let detail = body
                .display_name
                .or(body.username)
                .or(body.account_id)
                .or(body.email)
                .unwrap_or_else(|| provider_label.to_owned());
            Ok(LivenessResult::live(detail))
        }
        401 => Ok(LivenessResult::revoked(format!(
            "{provider_label} rejected the token (401 Unauthorized)"
        ))),
        403 => Ok(LivenessResult::revoked(format!(
            "{provider_label} refused the token (403 Forbidden)"
        ))),
        429 => {
            let retry = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
                .unwrap_or_else(|| "unknown".to_owned());
            Ok(LivenessResult::throttled(format!(
                "{provider_label} rate limit exceeded; retry-after: {retry}"
            )))
        }
        other => {
            let body = resp.text().await.unwrap_or_default();
            Ok(LivenessResult::error(format!(
                "{provider_label} GET {url} returned {other}: {}",
                body.trim()
            )))
        }
    }
}

async fn probe_cloud_bearer(
    http: &reqwest::Client,
    client: &ConfluenceClient,
    token: &SecretString,
) -> Result<LivenessResult> {
    let accessible_resources_url = format!(
        "{}/oauth/token/accessible-resources",
        client
            .cloud_api_base_url()
            .unwrap_or_else(|| "https://api.atlassian.com".to_string())
    );
    let resp = http
        .get(&accessible_resources_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .bearer_auth(token.expose_secret())
        .send()
        .await
        .map_err(|e| {
            Error::Http(format!(
                "confluence cloud liveness GET accessible-resources: {e}"
            ))
        })?;

    match resp.status().as_u16() {
        200 => {
            let resources: Vec<AtlassianAccessibleResource> = resp.json().await.map_err(|e| {
                Error::InvalidData(format!(
                    "confluence cloud accessible-resources returned non-JSON body: {e}"
                ))
            })?;
            let wanted_origin = url_origin(client.instance_url())
                .or_else(|| url_origin(client.base_url()))
                .ok_or_else(|| {
                    Error::InvalidData(format!(
                        "cannot determine origin from Confluence base URL '{}'",
                        client.base_url()
                    ))
                })?;

            let matched = resources
                .into_iter()
                .find(|resource| {
                    resource
                        .url
                        .as_deref()
                        .and_then(url_origin)
                        .map(|origin| origin == wanted_origin)
                        .unwrap_or(false)
                })
                .ok_or_else(|| {
                    Error::NotFound(format!(
                        "no Atlassian accessible resource matched Confluence base URL '{}'",
                        client.instance_url()
                    ))
                })?;

            let cloud_api_base = client
                .cloud_api_base_url()
                .unwrap_or_else(|| "https://api.atlassian.com".to_string());
            let probe_url = format!(
                "{cloud_api_base}/ex/confluence/{}/wiki/api/v2/space?limit=1",
                matched.id
            );
            let probe = http
                .get(&probe_url)
                .header(reqwest::header::ACCEPT, "application/json")
                .bearer_auth(token.expose_secret())
                .send()
                .await
                .map_err(|e| {
                    Error::Http(format!(
                        "confluence cloud liveness GET /wiki/api/v2/space: {e}"
                    ))
                })?;

            match probe.status().as_u16() {
                200 => Ok(LivenessResult::live(format!(
                    "{} ({})",
                    matched.url.as_deref().unwrap_or(client.instance_url()),
                    matched.id
                ))),
                401 => Ok(LivenessResult::revoked(
                    "confluence cloud rejected the bearer token (401 Unauthorized)",
                )),
                403 => Ok(LivenessResult::revoked(
                    "confluence cloud refused the bearer token (403 Forbidden)",
                )),
                429 => {
                    let retry = probe
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned)
                        .unwrap_or_else(|| "unknown".to_owned());
                    Ok(LivenessResult::throttled(format!(
                        "confluence cloud rate limit exceeded; retry-after: {retry}"
                    )))
                }
                other => {
                    let body = probe.text().await.unwrap_or_default();
                    Ok(LivenessResult::error(format!(
                        "confluence cloud GET {probe_url} returned {other}: {}",
                        body.trim()
                    )))
                }
            }
        }
        401 => Ok(LivenessResult::revoked(
            "confluence cloud rejected the bearer token (401 Unauthorized)",
        )),
        403 => Ok(LivenessResult::revoked(
            "confluence cloud refused the bearer token (403 Forbidden)",
        )),
        429 => {
            let retry = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
                .unwrap_or_else(|| "unknown".to_owned());
            Ok(LivenessResult::throttled(format!(
                "confluence cloud rate limit exceeded; retry-after: {retry}"
            )))
        }
        other => {
            let body = resp.text().await.unwrap_or_default();
            Ok(LivenessResult::error(format!(
                "confluence cloud GET {accessible_resources_url} returned {other}: {}",
                body.trim()
            )))
        }
    }
}

fn apply_auth(
    request: reqwest::RequestBuilder,
    auth: &ConfluenceAuth,
    token: &SecretString,
) -> reqwest::RequestBuilder {
    match auth {
        ConfluenceAuth::None => request,
        ConfluenceAuth::BearerToken(_) => request.bearer_auth(token.expose_secret()),
        ConfluenceAuth::Basic { username, .. } => {
            request.basic_auth(username.as_str(), Some(token.expose_secret()))
        }
    }
}

fn url_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split('/').next()?;
    if host.is_empty() {
        return None;
    }
    Some(format!("{}://{}", scheme.to_ascii_lowercase(), host))
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::liveness::LivenessStatus;
    use httpmock::prelude::*;

    #[tokio::test]
    async fn self_hosted_live_token_returns_user_detail() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/rest/api/user/current")
                    .header("Authorization", "Bearer secret-token");
                then.status(200)
                    .json_body(serde_json::json!({"displayName": "Confluence Admin"}));
            })
            .await;

        let client = ConfluenceClient::new(&server.base_url(), ConfluenceAuth::bearer("ignored"));
        let r = client
            .test(&SecretString::from("secret-token".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Live);
        assert_eq!(r.detail.as_deref(), Some("Confluence Admin"));
    }

    #[tokio::test]
    async fn cloud_basic_live_token_uses_wiki_rest_api() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/wiki/rest/api/user/current").header(
                    "Authorization",
                    "Basic ZGV2QGV4YW1wbGUuY29tOnNlY3JldC10b2tlbg==",
                );
                then.status(200)
                    .json_body(serde_json::json!({"accountId": "acct-123"}));
            })
            .await;

        let client = ConfluenceClient::new(
            &server.base_url(),
            ConfluenceAuth::basic("dev@example.com", "ignored"),
        )
        .with_flavor(ConfluenceFlavor::Cloud);
        let r = client
            .test(&SecretString::from("secret-token".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Live);
        assert_eq!(r.detail.as_deref(), Some("acct-123"));
    }

    #[tokio::test]
    async fn cloud_bearer_live_token_discovers_resource_and_probes_space_api() {
        let server = MockServer::start_async().await;
        let _resources = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/oauth/token/accessible-resources")
                    .header("Authorization", "Bearer secret-token");
                then.status(200).json_body(serde_json::json!([
                    {"id": "cloud-123", "url": "https://team.atlassian.net"}
                ]));
            })
            .await;
        let _spaces = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/ex/confluence/cloud-123/wiki/api/v2/space")
                    .query_param("limit", "1")
                    .header("Authorization", "Bearer secret-token");
                then.status(200)
                    .json_body(serde_json::json!({"results": []}));
            })
            .await;

        let client = ConfluenceClient::new(
            "https://team.atlassian.net",
            ConfluenceAuth::bearer("ignored-self-token"),
        )
        .with_flavor(ConfluenceFlavor::Cloud)
        .with_cloud_api_base_url(server.base_url());
        let r = client
            .test(&SecretString::from("secret-token".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Live);
        let detail = r.detail.unwrap();
        assert!(detail.contains("team.atlassian.net"));
        assert!(detail.contains("cloud-123"));
    }

    #[tokio::test]
    async fn revoked_on_401() {
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET).path("/rest/api/user/current");
                then.status(401);
            })
            .await;
        let client = ConfluenceClient::new(&server.base_url(), ConfluenceAuth::bearer("ignored"));
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
                when.method(GET).path("/rest/api/user/current");
                then.status(429).header("Retry-After", "60");
            })
            .await;
        let client = ConfluenceClient::new(&server.base_url(), ConfluenceAuth::bearer("ignored"));
        let r = client
            .test(&SecretString::from("over-limit".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::Throttled);
        assert!(r.detail.unwrap().contains("60"));
    }

    #[test]
    fn provider_name_is_confluence() {
        let client =
            ConfluenceClient::new("https://example.atlassian.net", ConfluenceAuth::bearer("x"));
        assert_eq!(client.provider_name(), "confluence");
    }
}
