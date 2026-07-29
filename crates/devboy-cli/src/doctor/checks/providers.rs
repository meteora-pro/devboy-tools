use crate::doctor::checks::{resolve_active_provider_context, resolve_secret};
use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};
use async_trait::async_trait;
use devboy_core::{
    ClickUpConfig, ConfluenceConfig, ContextConfig, GitHubConfig, GitLabConfig, JiraConfig,
    SlackConfig,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, USER_AGENT};
use reqwest::{Client, Method};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

pub struct GitHubApiCheck;
pub struct GitLabApiCheck;
pub struct ClickUpApiCheck;
pub struct JiraApiCheck;
pub struct ConfluenceApiCheck;
pub struct SlackApiCheck;

#[derive(Debug, Clone)]
struct RateLimitInfo {
    limit: Option<String>,
    remaining: Option<String>,
    reset: Option<String>,
    used: Option<String>,
    resource: Option<String>,
}

impl RateLimitInfo {
    fn to_json(&self) -> Value {
        json!({
            "limit": self.limit,
            "remaining": self.remaining,
            "reset": self.reset,
            "used": self.used,
            "resource": self.resource,
        })
    }

    fn is_empty(&self) -> bool {
        self.limit.is_none()
            && self.remaining.is_none()
            && self.reset.is_none()
            && self.used.is_none()
            && self.resource.is_none()
    }
}

#[derive(Debug, Clone)]
struct ProviderIdentity {
    username: String,
    name: Option<String>,
    email: Option<String>,
}

impl ProviderIdentity {
    fn to_json(&self) -> Value {
        json!({
            "username": self.username,
            "name": self.name,
            "email": self.email,
        })
    }
}

#[derive(Debug, Clone)]
struct ConnectivityOutcome {
    message: String,
    user: Option<ProviderIdentity>,
    rate_limit: Option<RateLimitInfo>,
}

#[derive(Deserialize)]
struct GitHubUserResponse {
    login: String,
    name: Option<String>,
    email: Option<String>,
}

#[derive(Deserialize)]
struct GitLabUserResponse {
    username: String,
    name: Option<String>,
    email: Option<String>,
}

#[derive(Deserialize)]
struct JiraUserResponse {
    #[serde(default)]
    name: String,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "emailAddress")]
    email_address: Option<String>,
    #[serde(default)]
    #[serde(rename = "accountId")]
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct ClickUpTasksResponse {
    tasks: Vec<Value>,
}

#[derive(Deserialize)]
struct ConfluenceSpaceResponse {
    results: Vec<ConfluenceSpaceResponseItem>,
}

#[derive(Deserialize)]
struct ConfluenceSpaceResponseItem {
    id: String,
    key: String,
    name: String,
}

fn http_client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| format!("Failed to create HTTP client: {error}"))
}

fn header_value(headers: &HeaderMap, key: &str) -> Option<String> {
    headers
        .get(key)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

fn rate_limit_from_headers(headers: &HeaderMap, provider: &str) -> Option<RateLimitInfo> {
    let info = match provider {
        "github" => RateLimitInfo {
            limit: header_value(headers, "x-ratelimit-limit"),
            remaining: header_value(headers, "x-ratelimit-remaining"),
            reset: header_value(headers, "x-ratelimit-reset"),
            used: header_value(headers, "x-ratelimit-used"),
            resource: header_value(headers, "x-ratelimit-resource"),
        },
        "gitlab" => RateLimitInfo {
            limit: header_value(headers, "ratelimit-limit")
                .or_else(|| header_value(headers, "x-ratelimit-limit")),
            remaining: header_value(headers, "ratelimit-remaining")
                .or_else(|| header_value(headers, "x-ratelimit-remaining")),
            reset: header_value(headers, "ratelimit-resettime")
                .or_else(|| header_value(headers, "ratelimit-reset"))
                .or_else(|| header_value(headers, "x-ratelimit-reset")),
            used: header_value(headers, "ratelimit-observed"),
            resource: None,
        },
        "jira" => RateLimitInfo {
            limit: header_value(headers, "x-ratelimit-limit"),
            remaining: header_value(headers, "x-ratelimit-remaining"),
            reset: header_value(headers, "x-ratelimit-reset"),
            used: header_value(headers, "x-ratelimit-nearlimit"),
            resource: None,
        },
        _ => return None,
    };

    (!info.is_empty()).then_some(info)
}

fn parse_error(status: reqwest::StatusCode, body: String) -> (CheckStatus, String) {
    let body = body.trim();
    let detail = if body.is_empty() {
        status
            .canonical_reason()
            .unwrap_or("request failed")
            .to_string()
    } else {
        body.to_string()
    };

    let prefix = match status.as_u16() {
        401 => "authentication failed",
        403 => "authenticated but forbidden",
        429 => "rate limit exceeded",
        500..=599 => "server error",
        _ => "request failed",
    };

    (CheckStatus::Error, format!("{prefix}: {detail}"))
}

fn base64_encode(input: &str) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut result = String::new();

    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARSET[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARSET[((triple >> 12) & 0x3F) as usize] as char);
        result.push(if chunk.len() > 1 {
            CHARSET[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            CHARSET[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }

    result
}

fn connectivity_details(
    provider: &str,
    context_name: &str,
    token_key: &str,
    token_source: &str,
    outcome: &ConnectivityOutcome,
) -> Value {
    json!({
        "provider": provider,
        "context": context_name,
        "token_key": token_key,
        "token_source": token_source,
        "user": outcome.user.as_ref().map(ProviderIdentity::to_json),
        "rate_limit": outcome.rate_limit.as_ref().map(RateLimitInfo::to_json),
    })
}

fn skipped(check: &dyn DiagnosticCheck, message: &str) -> CheckResult {
    CheckResult {
        id: check.id().to_string(),
        category: check.category().to_string(),
        name: check.name().to_string(),
        status: CheckStatus::Skipped,
        message: message.to_string(),
        details: None,
        fix_command: None,
        fix_url: None,
    }
}

async fn github_connectivity(
    config: &GitHubConfig,
    token: &str,
) -> Result<ConnectivityOutcome, String> {
    let client = http_client()?;
    let base_url = config
        .base_url
        .clone()
        .unwrap_or_else(|| "https://api.github.com".to_string())
        .trim_end_matches('/')
        .to_string();
    let response = client
        .request(Method::GET, format!("{base_url}/user"))
        .header(USER_AGENT, "devboy-tools")
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .map_err(|error| format!("Network error: {error}"))?;

    let status = response.status();
    let headers = response.headers().clone();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(parse_error(status, body).1);
    }

    let user: GitHubUserResponse = response
        .json()
        .await
        .map_err(|error| format!("Invalid GitHub response: {error}"))?;

    Ok(ConnectivityOutcome {
        message: format!("GitHub API authenticated as @{}", user.login),
        user: Some(ProviderIdentity {
            username: user.login,
            name: user.name,
            email: user.email,
        }),
        rate_limit: rate_limit_from_headers(&headers, "github"),
    })
}

async fn gitlab_connectivity(
    config: &GitLabConfig,
    token: &str,
) -> Result<ConnectivityOutcome, String> {
    let client = http_client()?;
    let base_url = config.url.trim_end_matches('/');
    let response = client
        .request(Method::GET, format!("{base_url}/api/v4/user"))
        .header("PRIVATE-TOKEN", token)
        .send()
        .await
        .map_err(|error| format!("Network error: {error}"))?;

    let status = response.status();
    let headers = response.headers().clone();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(parse_error(status, body).1);
    }

    let user: GitLabUserResponse = response
        .json()
        .await
        .map_err(|error| format!("Invalid GitLab response: {error}"))?;

    Ok(ConnectivityOutcome {
        message: format!("GitLab API authenticated as @{}", user.username),
        user: Some(ProviderIdentity {
            username: user.username,
            name: user.name,
            email: user.email,
        }),
        rate_limit: rate_limit_from_headers(&headers, "gitlab"),
    })
}

async fn clickup_connectivity(
    config: &ClickUpConfig,
    token: &str,
) -> Result<ConnectivityOutcome, String> {
    let client = http_client()?;
    let response = client
        .request(
            Method::GET,
            format!(
                "https://api.clickup.com/api/v2/list/{}/task?page=0&subtasks=false",
                config.list_id
            ),
        )
        .header(AUTHORIZATION, token)
        .header(CONTENT_TYPE, "application/json")
        .send()
        .await
        .map_err(|error| format!("Network error: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(parse_error(status, body).1);
    }

    let tasks: ClickUpTasksResponse = response
        .json()
        .await
        .map_err(|error| format!("Invalid ClickUp response: {error}"))?;

    Ok(ConnectivityOutcome {
        message: format!("ClickUp API authenticated for list {}", config.list_id),
        user: Some(ProviderIdentity {
            username: "clickup-user".to_string(),
            name: Some(format!("synthetic identity ({})", tasks.tasks.len())),
            email: None,
        }),
        rate_limit: None,
    })
}

async fn jira_connectivity(
    config: &JiraConfig,
    token: &str,
) -> Result<ConnectivityOutcome, String> {
    let client = http_client()?;
    let base_url = config.url.trim_end_matches('/');
    let api_base = if base_url.contains(".atlassian.net") {
        format!("{base_url}/rest/api/3")
    } else {
        format!("{base_url}/rest/api/2")
    };

    let mut request = client
        .request(Method::GET, format!("{api_base}/myself"))
        .header(USER_AGENT, "devboy-tools")
        .header(CONTENT_TYPE, "application/json");

    if base_url.contains(".atlassian.net") {
        request = request.header(
            AUTHORIZATION,
            format!(
                "Basic {}",
                base64_encode(&format!("{}:{token}", config.email))
            ),
        );
    } else if token.contains(':') {
        request = request.header(AUTHORIZATION, format!("Basic {}", base64_encode(token)));
    } else {
        request = request.header(AUTHORIZATION, format!("Bearer {token}"));
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("Network error: {error}"))?;

    let status = response.status();
    let headers = response.headers().clone();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(parse_error(status, body).1);
    }

    let user: JiraUserResponse = response
        .json()
        .await
        .map_err(|error| format!("Invalid Jira response: {error}"))?;

    let username = if !user.name.is_empty() {
        user.name
    } else {
        user.account_id.unwrap_or_else(|| "jira-user".to_string())
    };

    Ok(ConnectivityOutcome {
        message: format!("Jira API authenticated as @{}", username),
        user: Some(ProviderIdentity {
            username,
            name: user.display_name,
            email: user.email_address,
        }),
        rate_limit: rate_limit_from_headers(&headers, "jira"),
    })
}

async fn slack_connectivity(
    config: &SlackConfig,
    token: &str,
) -> Result<ConnectivityOutcome, String> {
    let mut client = devboy_slack::SlackClient::new(SecretString::from(token.to_string()))
        .with_required_scopes(config.required_scopes.clone());
    if let Some(base_url) = &config.base_url {
        client = client.with_base_url(base_url);
    }

    let info = client
        .auth_info()
        .await
        .map_err(|error| error.to_string())?;

    if !info.missing_scopes.is_empty() {
        return Err(format!(
            "missing required scopes: {}",
            info.missing_scopes.join(", ")
        ));
    }

    Ok(ConnectivityOutcome {
        message: format!("Slack API authenticated for workspace {}", info.team_name),
        user: Some(ProviderIdentity {
            username: info.user_id,
            name: info.user_name,
            email: None,
        }),
        rate_limit: None,
    })
}

async fn confluence_connectivity(
    config: &ConfluenceConfig,
    token: &str,
) -> Result<ConnectivityOutcome, String> {
    let client = http_client()?;
    let base_url = config.base_url.trim_end_matches('/');
    if confluence_is_cloud(config) {
        return confluence_cloud_connectivity(
            &client,
            base_url,
            config,
            token,
            "https://api.atlassian.com",
        )
        .await;
    }
    let api_paths = confluence_space_api_paths(config.api_version.as_deref());

    // Doctor probes every candidate API path concurrently rather than
    // walking them as a sequential fallback chain. Trade-off: one extra
    // HTTP request when v1 succeeds (the v2 probe runs anyway), bought
    // for ~half the wall-clock when v2 is the working version. Doctor
    // is a diagnostic, not the hot path — speed of feedback for the
    // operator wins.
    let probes = api_paths
        .iter()
        .map(|api_path| probe_confluence_endpoint(&client, base_url, api_path, config, token));

    let results = futures::future::join_all(probes).await;

    let mut last_error: Option<String> = None;
    for result in results {
        match result {
            Ok(outcome) => return Ok(outcome),
            Err(ProbeError::AuthOrApi(msg)) => last_error = Some(msg),
            Err(ProbeError::Transport(msg)) => return Err(msg),
        }
    }
    Err(last_error.unwrap_or_else(|| "request failed".to_string()))
}

/// One probe = one (base_url, api_path) HTTP call. Returns the
/// connectivity outcome on success; on auth / API errors returns a
/// recoverable [`ProbeError::AuthOrApi`] so the caller can fall through
/// to the next probe; on transport errors returns
/// [`ProbeError::Transport`] (no point trying the next probe — DNS /
/// TLS failures hit every endpoint equally).
async fn probe_confluence_endpoint(
    client: &Client,
    base_url: &str,
    api_path: &str,
    config: &ConfluenceConfig,
    token: &str,
) -> Result<ConnectivityOutcome, ProbeError> {
    let url = format!("{base_url}{api_path}/space?limit=1");
    let mut request = client
        .request(Method::GET, &url)
        .header(USER_AGENT, "devboy-tools")
        .header(ACCEPT, "application/json");

    request = if config.username.is_some() {
        let username = config.username.as_deref().unwrap_or_default();
        request.basic_auth(username, Some(token))
    } else {
        request.bearer_auth(token)
    };

    let response = request
        .send()
        .await
        .map_err(|error| ProbeError::Transport(format!("Network error: {error}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ProbeError::AuthOrApi(parse_error(status, body).1));
    }

    let payload: ConfluenceSpaceResponse = response
        .json()
        .await
        .map_err(|error| ProbeError::Transport(format!("Invalid Confluence response: {error}")))?;

    let message = match payload.results.into_iter().next() {
        Some(space) => format!(
            "Confluence API reachable for space {} ({}) [id: {}]",
            space.key, space.name, space.id
        ),
        None => "Confluence API reachable".to_string(),
    };

    Ok(ConnectivityOutcome {
        message,
        user: None,
        rate_limit: None,
    })
}

fn confluence_is_cloud(config: &ConfluenceConfig) -> bool {
    match config.flavor {
        Some(devboy_core::ConfluenceFlavor::Cloud) => true,
        Some(devboy_core::ConfluenceFlavor::SelfHosted) => false,
        None => config.cloud_id.is_some() || config.base_url.contains(".atlassian.net"),
    }
}

#[derive(Debug, Deserialize)]
struct AtlassianAccessibleResource {
    id: String,
    #[serde(default)]
    url: Option<String>,
}

async fn confluence_cloud_connectivity(
    client: &Client,
    base_url: &str,
    config: &ConfluenceConfig,
    token: &str,
    cloud_api_base: &str,
) -> Result<ConnectivityOutcome, String> {
    let cloud_id = match config.cloud_id.as_deref() {
        Some(cloud_id) => cloud_id.to_string(),
        None if config.username.is_some() => {
            return Err(
                "Confluence Cloud with basic auth requires explicit confluence.cloud_id"
                    .to_string(),
            );
        }
        None => discover_confluence_cloud_id(client, base_url, token, cloud_api_base).await?,
    };

    let url = format!("{cloud_api_base}/ex/confluence/{cloud_id}/wiki/api/v2/space?limit=1");
    let mut request = client
        .request(Method::GET, &url)
        .header(USER_AGENT, "devboy-tools")
        .header(ACCEPT, "application/json");

    request = if let Some(username) = config.username.as_deref() {
        request.basic_auth(username, Some(token))
    } else {
        request.bearer_auth(token)
    };

    let response = request
        .send()
        .await
        .map_err(|error| format!("Network error: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(parse_error(status, body).1);
    }

    let payload: ConfluenceSpaceResponse = response
        .json()
        .await
        .map_err(|error| format!("Invalid Confluence response: {error}"))?;

    let message = match payload.results.into_iter().next() {
        Some(space) => format!(
            "Confluence Cloud API reachable for space {} ({}) [id: {}]",
            space.key, space.name, space.id
        ),
        None => "Confluence Cloud API reachable".to_string(),
    };

    Ok(ConnectivityOutcome {
        message,
        user: None,
        rate_limit: None,
    })
}

async fn discover_confluence_cloud_id(
    client: &Client,
    base_url: &str,
    token: &str,
    cloud_api_base: &str,
) -> Result<String, String> {
    let response = client
        .request(
            Method::GET,
            format!("{cloud_api_base}/oauth/token/accessible-resources"),
        )
        .header(USER_AGENT, "devboy-tools")
        .header(ACCEPT, "application/json")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| format!("Network error: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(parse_error(status, body).1);
    }

    let resources: Vec<AtlassianAccessibleResource> = response
        .json()
        .await
        .map_err(|error| format!("Invalid Atlassian accessible-resources response: {error}"))?;

    let wanted_origin = url_origin(base_url).ok_or_else(|| {
        format!(
            "Could not determine origin from Confluence base URL '{}'",
            base_url
        )
    })?;

    resources
        .into_iter()
        .find(|resource| {
            resource
                .url
                .as_deref()
                .and_then(url_origin)
                .map(|origin| origin == wanted_origin)
                .unwrap_or(false)
        })
        .map(|resource| resource.id)
        .ok_or_else(|| {
            format!(
                "No Atlassian accessible resource matched Confluence base URL '{}'",
                base_url
            )
        })
}

fn url_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split('/').next()?;
    if host.is_empty() {
        return None;
    }
    Some(format!("{}://{}", scheme.to_ascii_lowercase(), host))
}

enum ProbeError {
    /// HTTP-level error (4xx / 5xx) or non-JSON response from the server.
    /// The caller can fall through to the next candidate API path.
    AuthOrApi(String),
    /// Transport / DNS / TLS failure. Same error will hit every endpoint,
    /// so propagate immediately.
    Transport(String),
}

fn confluence_space_api_paths(api_version: Option<&str>) -> Vec<&'static str> {
    match api_version.map(str::trim).filter(|value| !value.is_empty()) {
        Some("v2") => vec!["/api/v2", "/rest/api"],
        _ => vec!["/rest/api"],
    }
}

use std::pin::Pin;

type ConnectFuture<'a> =
    Pin<Box<dyn std::future::Future<Output = Result<ConnectivityOutcome, String>> + Send + 'a>>;

async fn run_provider_check<C, F>(
    check: &dyn DiagnosticCheck,
    ctx: &DiagnosticContext,
    provider: &'static str,
    extract_config: F,
    connect: impl for<'a> FnOnce(&'a C, &'a str) -> ConnectFuture<'a>,
) -> CheckResult
where
    F: FnOnce(&ContextConfig) -> Option<C>,
{
    let Some(config) = &ctx.config else {
        return skipped(check, "Skipped because config could not be loaded");
    };

    let Some(active) = resolve_active_provider_context(config) else {
        return skipped(check, "Skipped because no active context could be resolved");
    };

    let Some(provider_config) = extract_config(&active.config) else {
        return skipped(
            check,
            &format!(
                "Skipped because {} is not configured in context '{}'",
                provider, active.name
            ),
        );
    };

    let secret = match resolve_secret(ctx, Some(&active.name), provider) {
        Ok(Some(secret)) => secret,
        Ok(None) => {
            return skipped(
                check,
                &format!(
                    "Skipped because {provider} credentials are missing for context '{}'",
                    active.name
                ),
            );
        }
        Err(error) => {
            return CheckResult {
                id: check.id().to_string(),
                category: check.category().to_string(),
                name: check.name().to_string(),
                status: CheckStatus::Error,
                message: format!("Could not read {provider} credentials: {error}"),
                details: ctx
                    .verbose
                    .then(|| json!({ "provider": provider, "error": error })),
                fix_command: None,
                fix_url: None,
            };
        }
    };

    match connect(&provider_config, secret.value.expose_secret()).await {
        Ok(outcome) => {
            let details = ctx.verbose.then(|| {
                connectivity_details(provider, &active.name, &secret.key, secret.source, &outcome)
            });

            CheckResult {
                id: check.id().to_string(),
                category: check.category().to_string(),
                name: check.name().to_string(),
                status: CheckStatus::Pass,
                message: outcome.message,
                details,
                fix_command: None,
                fix_url: None,
            }
        }
        Err(error) => CheckResult {
            id: check.id().to_string(),
            category: check.category().to_string(),
            name: check.name().to_string(),
            status: CheckStatus::Error,
            message: format!("{provider} connectivity failed: {error}"),
            details: ctx.verbose.then(|| {
                json!({
                    "provider": provider,
                    "context": active.name,
                    "token_key": secret.key,
                    "token_source": secret.source,
                    "error": error,
                })
            }),
            fix_command: None,
            fix_url: None,
        },
    }
}

#[async_trait]
impl DiagnosticCheck for GitHubApiCheck {
    fn id(&self) -> &'static str {
        "providers.github"
    }

    fn name(&self) -> &'static str {
        "GitHub API connectivity"
    }

    fn category(&self) -> &'static str {
        "Provider Connectivity"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        run_provider_check(
            self,
            ctx,
            "github",
            |c| c.github.clone(),
            |cfg, token| Box::pin(github_connectivity(cfg, token)),
        )
        .await
    }
}

#[async_trait]
impl DiagnosticCheck for GitLabApiCheck {
    fn id(&self) -> &'static str {
        "providers.gitlab"
    }

    fn name(&self) -> &'static str {
        "GitLab API connectivity"
    }

    fn category(&self) -> &'static str {
        "Provider Connectivity"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        run_provider_check(
            self,
            ctx,
            "gitlab",
            |c| c.gitlab.clone(),
            |cfg, token| Box::pin(gitlab_connectivity(cfg, token)),
        )
        .await
    }
}

#[async_trait]
impl DiagnosticCheck for ClickUpApiCheck {
    fn id(&self) -> &'static str {
        "providers.clickup"
    }

    fn name(&self) -> &'static str {
        "ClickUp API connectivity"
    }

    fn category(&self) -> &'static str {
        "Provider Connectivity"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        run_provider_check(
            self,
            ctx,
            "clickup",
            |c| c.clickup.clone(),
            |cfg, token| Box::pin(clickup_connectivity(cfg, token)),
        )
        .await
    }
}

#[async_trait]
impl DiagnosticCheck for JiraApiCheck {
    fn id(&self) -> &'static str {
        "providers.jira"
    }

    fn name(&self) -> &'static str {
        "Jira API connectivity"
    }

    fn category(&self) -> &'static str {
        "Provider Connectivity"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        run_provider_check(
            self,
            ctx,
            "jira",
            |c| c.jira.clone(),
            |cfg, token| Box::pin(jira_connectivity(cfg, token)),
        )
        .await
    }
}

#[async_trait]
impl DiagnosticCheck for SlackApiCheck {
    fn id(&self) -> &'static str {
        "providers.slack"
    }

    fn name(&self) -> &'static str {
        "Slack API connectivity"
    }

    fn category(&self) -> &'static str {
        "Provider Connectivity"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        run_provider_check(
            self,
            ctx,
            "slack",
            |c| c.slack.clone(),
            |cfg, token| Box::pin(slack_connectivity(cfg, token)),
        )
        .await
    }
}

#[async_trait]
impl DiagnosticCheck for ConfluenceApiCheck {
    fn id(&self) -> &'static str {
        "providers.confluence"
    }

    fn name(&self) -> &'static str {
        "Confluence API connectivity"
    }

    fn category(&self) -> &'static str {
        "Provider Connectivity"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        run_provider_check(
            self,
            ctx,
            "confluence",
            |c| c.confluence.clone(),
            |cfg, token| Box::pin(confluence_connectivity(cfg, token)),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::DiagnosticContext;
    use devboy_core::{
        ClickUpConfig, Config, ConfluenceConfig, ContextConfig, Error, GitHubConfig, GitLabConfig,
        JiraConfig,
    };
    use devboy_storage::{CredentialStore, MemoryStore};
    use httpmock::Method::GET;
    use httpmock::MockServer;
    use reqwest::header::HeaderValue;
    use secrecy::SecretString;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[derive(Debug)]
    struct FailingStore;

    impl CredentialStore for FailingStore {
        fn store(&self, _key: &str, _value: &SecretString) -> devboy_core::Result<()> {
            Err(Error::Storage("store failed".to_string()))
        }

        fn get(&self, _key: &str) -> devboy_core::Result<Option<SecretString>> {
            Err(Error::Storage("provider store unavailable".to_string()))
        }

        fn delete(&self, _key: &str) -> devboy_core::Result<()> {
            Err(Error::Storage("delete failed".to_string()))
        }
    }

    fn context_with_provider(
        store: Arc<dyn CredentialStore>,
        context: ContextConfig,
    ) -> DiagnosticContext {
        let mut contexts = BTreeMap::new();
        contexts.insert("workspace".to_string(), context);

        DiagnosticContext {
            config: Some(Config {
                contexts,
                active_context: Some("workspace".to_string()),
                ..Default::default()
            }),
            config_path: Some(PathBuf::from("config.toml")),
            config_exists: true,
            config_source: "test",
            config_path_error: None,
            config_load_error: None,
            credential_store: store,
            verbose: true,
        }
    }

    #[tokio::test]
    async fn github_connectivity_collects_user_and_rate_limit() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/user");
            then.status(200)
                .header("x-ratelimit-limit", "5000")
                .header("x-ratelimit-remaining", "4999")
                .json_body(json!({
                    "login": "octocat",
                    "name": "The Octocat",
                    "email": "octo@example.com"
                }));
        });

        let outcome = github_connectivity(
            &GitHubConfig {
                owner: "o".to_string(),
                repo: "r".to_string(),
                base_url: Some(server.base_url()),
            },
            "ghp_test_token_1234567890",
        )
        .await
        .unwrap();

        assert_eq!(outcome.user.as_ref().unwrap().username, "octocat");
        assert_eq!(
            outcome.rate_limit.as_ref().unwrap().remaining.as_deref(),
            Some("4999")
        );
    }

    #[tokio::test]
    async fn github_connectivity_returns_authentication_error() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/user");
            then.status(401).body("bad token");
        });

        let error = github_connectivity(
            &GitHubConfig {
                owner: "o".to_string(),
                repo: "r".to_string(),
                base_url: Some(server.base_url()),
            },
            "bad-token",
        )
        .await
        .unwrap_err();

        assert_eq!(error, "authentication failed: bad token");
    }

    #[tokio::test]
    async fn github_connectivity_reports_invalid_payload() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/user");
            then.status(200).body("not-json");
        });

        let error = github_connectivity(
            &GitHubConfig {
                owner: "o".to_string(),
                repo: "r".to_string(),
                base_url: Some(server.base_url()),
            },
            "ghp_token_12345678901234567890",
        )
        .await
        .unwrap_err();

        assert!(error.contains("Invalid GitHub response"));
    }

    #[tokio::test]
    async fn gitlab_connectivity_collects_identity_and_rate_limits() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/api/v4/user");
            then.status(200)
                .header("ratelimit-limit", "600")
                .header("ratelimit-remaining", "599")
                .json_body(json!({
                    "username": "gitlab-user",
                    "name": "GitLab User",
                    "email": "gitlab@example.com"
                }));
        });

        let outcome = gitlab_connectivity(
            &GitLabConfig {
                url: server.base_url(),
                project_id: "group/project".to_string(),
            },
            "glpat-12345678901234567890",
        )
        .await
        .unwrap();

        assert_eq!(outcome.user.unwrap().username, "gitlab-user");
        assert_eq!(
            outcome.rate_limit.unwrap().remaining.as_deref(),
            Some("599")
        );
    }

    #[tokio::test]
    async fn jira_connectivity_supports_self_hosted_basic_auth() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/2/myself")
                .header("authorization", "Basic dXNlcjp0b2tlbg==");
            then.status(200).json_body(json!({
                "name": "jira-user",
                "displayName": "Jira User",
                "emailAddress": "jira@example.com"
            }));
        });

        let outcome = jira_connectivity(
            &JiraConfig {
                url: server.base_url(),
                project_key: "PROJ".to_string(),
                email: "jira@example.com".to_string(),
            },
            "user:token",
        )
        .await
        .unwrap();

        assert_eq!(outcome.user.unwrap().username, "jira-user");
    }

    #[tokio::test]
    async fn jira_connectivity_falls_back_to_account_id() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/rest/api/2/myself");
            then.status(200).json_body(json!({
                "name": "",
                "displayName": "Jira User",
                "accountId": "acct-123"
            }));
        });

        let outcome = jira_connectivity(
            &JiraConfig {
                url: server.base_url(),
                project_key: "PROJ".to_string(),
                email: "jira@example.com".to_string(),
            },
            "bearer-token-value",
        )
        .await
        .unwrap();

        assert_eq!(outcome.user.unwrap().username, "acct-123");
    }

    #[tokio::test]
    async fn confluence_connectivity_reaches_space_endpoint() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/space")
                .header(
                    "authorization",
                    "Basic ZGV2QGV4YW1wbGUuY29tOnNlY3JldC10b2tlbg==",
                )
                .query_param("limit", "1");
            then.status(200).json_body(json!({
                "results": [
                    {
                        "id": "1",
                        "key": "ENG",
                        "name": "Engineering"
                    }
                ]
            }));
        });

        let outcome = confluence_connectivity(
            &ConfluenceConfig {
                base_url: server.base_url(),
                flavor: None,
                cloud_id: None,
                api_version: Some("v1".to_string()),
                username: Some("dev@example.com".to_string()),
                client_id: None,
                redirect_uri: None,
                space_key: Some("ENG".to_string()),
            },
            "secret-token",
        )
        .await
        .unwrap();

        assert!(outcome.message.contains("Confluence API reachable"));
    }

    #[tokio::test]
    async fn confluence_connectivity_falls_back_to_rest_api_when_v2_unavailable() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/space")
                .header(
                    "authorization",
                    "Basic ZGV2QGV4YW1wbGUuY29tOnNlY3JldC10b2tlbg==",
                )
                .query_param("limit", "1");
            then.status(404);
        });
        let rest_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/space")
                .header(
                    "authorization",
                    "Basic ZGV2QGV4YW1wbGUuY29tOnNlY3JldC10b2tlbg==",
                )
                .query_param("limit", "1");
            then.status(200).json_body(json!({
                "results": [
                    {
                        "id": "1",
                        "key": "ENG",
                        "name": "Engineering"
                    }
                ]
            }));
        });

        let outcome = confluence_connectivity(
            &ConfluenceConfig {
                base_url: server.base_url(),
                flavor: None,
                cloud_id: None,
                api_version: Some("v2".to_string()),
                username: Some("dev@example.com".to_string()),
                client_id: None,
                redirect_uri: None,
                space_key: Some("ENG".to_string()),
            },
            "secret-token",
        )
        .await
        .unwrap();

        assert!(outcome.message.contains("Confluence API reachable"));
        rest_mock.assert();
    }

    #[test]
    fn confluence_is_cloud_respects_explicit_self_hosted_override() {
        let config = ConfluenceConfig {
            base_url: "https://team.atlassian.net".to_string(),
            flavor: Some(devboy_core::ConfluenceFlavor::SelfHosted),
            cloud_id: Some("cloud-123".to_string()),
            api_version: None,
            username: None,
            client_id: None,
            redirect_uri: None,
            space_key: None,
        };

        assert!(!confluence_is_cloud(&config));
    }

    #[tokio::test]
    async fn confluence_cloud_connectivity_discovers_cloud_id_for_bearer_auth() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/oauth/token/accessible-resources")
                .header("authorization", "Bearer secret-token");
            then.status(200).json_body(json!([
                {
                    "id": "cloud-123",
                    "url": "https://team.atlassian.net"
                }
            ]));
        });
        let space_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/ex/confluence/cloud-123/wiki/api/v2/space")
                .header("authorization", "Bearer secret-token")
                .query_param("limit", "1");
            then.status(200).json_body(json!({
                "results": [
                    {
                        "id": "1",
                        "key": "ENG",
                        "name": "Engineering"
                    }
                ]
            }));
        });

        let client = http_client().unwrap();
        let outcome = confluence_cloud_connectivity(
            &client,
            "https://team.atlassian.net",
            &ConfluenceConfig {
                base_url: "https://team.atlassian.net".to_string(),
                flavor: Some(devboy_core::ConfluenceFlavor::Cloud),
                cloud_id: None,
                api_version: None,
                username: None,
                client_id: None,
                redirect_uri: None,
                space_key: Some("ENG".to_string()),
            },
            "secret-token",
            &server.base_url(),
        )
        .await
        .unwrap();

        assert!(outcome.message.contains("Confluence Cloud API reachable"));
        space_mock.assert();
    }

    #[tokio::test]
    async fn confluence_cloud_connectivity_uses_explicit_cloud_id_for_basic_auth() {
        let server = MockServer::start();
        let space_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/ex/confluence/cloud-123/wiki/api/v2/space")
                .header(
                    "authorization",
                    "Basic ZGV2QGV4YW1wbGUuY29tOnNlY3JldC10b2tlbg==",
                )
                .query_param("limit", "1");
            then.status(200).json_body(json!({
                "results": [
                    {
                        "id": "1",
                        "key": "ENG",
                        "name": "Engineering"
                    }
                ]
            }));
        });

        let client = http_client().unwrap();
        let outcome = confluence_cloud_connectivity(
            &client,
            "https://team.atlassian.net",
            &ConfluenceConfig {
                base_url: "https://team.atlassian.net".to_string(),
                flavor: Some(devboy_core::ConfluenceFlavor::Cloud),
                cloud_id: Some("cloud-123".to_string()),
                api_version: None,
                username: Some("dev@example.com".to_string()),
                client_id: None,
                redirect_uri: None,
                space_key: Some("ENG".to_string()),
            },
            "secret-token",
            &server.base_url(),
        )
        .await
        .unwrap();

        assert!(outcome.message.contains("Confluence Cloud API reachable"));
        space_mock.assert();
    }

    #[tokio::test]
    async fn confluence_cloud_connectivity_requires_cloud_id_for_basic_auth() {
        let client = http_client().unwrap();
        let err = confluence_cloud_connectivity(
            &client,
            "https://team.atlassian.net",
            &ConfluenceConfig {
                base_url: "https://team.atlassian.net".to_string(),
                flavor: Some(devboy_core::ConfluenceFlavor::Cloud),
                cloud_id: None,
                api_version: None,
                username: Some("dev@example.com".to_string()),
                client_id: None,
                redirect_uri: None,
                space_key: Some("ENG".to_string()),
            },
            "secret-token",
            "https://api.atlassian.com",
        )
        .await
        .unwrap_err();

        assert!(err.contains("explicit confluence.cloud_id"));
    }

    #[tokio::test]
    async fn confluence_api_check_passes_when_connected() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/space")
                .header(
                    "authorization",
                    "Basic ZGV2QGV4YW1wbGUuY29tOnNlY3JldC10b2tlbg==",
                )
                .query_param("limit", "1");
            then.status(200).json_body(json!({
                "results": [
                    {
                        "id": "1",
                        "key": "ENG",
                        "name": "Engineering"
                    }
                ]
            }));
        });

        let ctx = context_with_provider(
            Arc::new(MemoryStore::with_credentials([(
                "contexts.workspace.confluence.token".to_string(),
                "secret-token".to_string(),
            )])),
            ContextConfig {
                confluence: Some(ConfluenceConfig {
                    base_url: server.base_url(),
                    flavor: None,
                    cloud_id: None,
                    api_version: Some("v1".to_string()),
                    username: Some("dev@example.com".to_string()),
                    client_id: None,
                    redirect_uri: None,
                    space_key: Some("ENG".to_string()),
                }),
                ..Default::default()
            },
        );

        let result = ConfluenceApiCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("Confluence API reachable"));
    }

    fn github_extractor(c: &ContextConfig) -> Option<GitHubConfig> {
        c.github.clone()
    }

    fn dummy_connect<'a>(_cfg: &'a GitHubConfig, _token: &'a str) -> ConnectFuture<'a> {
        Box::pin(async {
            Ok(ConnectivityOutcome {
                message: "ok".to_string(),
                user: None,
                rate_limit: None,
            })
        })
    }

    #[tokio::test]
    async fn run_provider_check_covers_all_generic_paths() {
        let ctx_without_config = DiagnosticContext {
            config: None,
            config_path: Some(PathBuf::from("config.toml")),
            config_exists: true,
            config_source: "test",
            config_path_error: None,
            config_load_error: None,
            credential_store: Arc::new(MemoryStore::new()),
            verbose: true,
        };

        let skipped_result = run_provider_check(
            &GitHubApiCheck,
            &ctx_without_config,
            "github",
            github_extractor,
            dummy_connect,
        )
        .await;
        assert_eq!(skipped_result.status, CheckStatus::Skipped);

        let no_active_ctx = DiagnosticContext {
            config: Some(Config::default()),
            ..ctx_without_config
        };
        let skipped_result = run_provider_check(
            &GitHubApiCheck,
            &no_active_ctx,
            "github",
            github_extractor,
            dummy_connect,
        )
        .await;
        assert_eq!(skipped_result.status, CheckStatus::Skipped);

        // not-configured: extractor returns None
        let configured_ctx = context_with_provider(
            Arc::new(MemoryStore::new()),
            ContextConfig {
                github: Some(GitHubConfig {
                    owner: "o".to_string(),
                    repo: "r".to_string(),
                    base_url: None,
                }),
                ..Default::default()
            },
        );
        let skipped_result = run_provider_check(
            &GitHubApiCheck,
            &configured_ctx,
            "github",
            |_| None::<GitHubConfig>,
            dummy_connect,
        )
        .await;
        assert_eq!(skipped_result.status, CheckStatus::Skipped);

        // missing secret
        let missing_secret = run_provider_check(
            &GitHubApiCheck,
            &configured_ctx,
            "github",
            github_extractor,
            dummy_connect,
        )
        .await;
        assert_eq!(missing_secret.status, CheckStatus::Skipped);

        let store_error_ctx = context_with_provider(
            Arc::new(FailingStore),
            ContextConfig {
                github: Some(GitHubConfig {
                    owner: "o".to_string(),
                    repo: "r".to_string(),
                    base_url: None,
                }),
                ..Default::default()
            },
        );
        let error_result = run_provider_check(
            &GitHubApiCheck,
            &store_error_ctx,
            "github",
            github_extractor,
            dummy_connect,
        )
        .await;
        assert_eq!(error_result.status, CheckStatus::Error);

        let success_ctx = context_with_provider(
            Arc::new(MemoryStore::with_credentials([(
                "contexts.workspace.github.token".to_string(),
                "ghp_12345678901234567890".to_string(),
            )])),
            ContextConfig {
                github: Some(GitHubConfig {
                    owner: "o".to_string(),
                    repo: "r".to_string(),
                    base_url: None,
                }),
                ..Default::default()
            },
        );
        let success_result = run_provider_check(
            &GitHubApiCheck,
            &success_ctx,
            "github",
            github_extractor,
            |_cfg: &GitHubConfig, _token: &str| -> ConnectFuture<'_> {
                Box::pin(async {
                    Ok(ConnectivityOutcome {
                        message: "connected".to_string(),
                        user: Some(ProviderIdentity {
                            username: "octocat".to_string(),
                            name: Some("Octo Cat".to_string()),
                            email: None,
                        }),
                        rate_limit: Some(RateLimitInfo {
                            limit: Some("5000".to_string()),
                            remaining: Some("4999".to_string()),
                            reset: None,
                            used: None,
                            resource: None,
                        }),
                    })
                })
            },
        )
        .await;
        assert_eq!(success_result.status, CheckStatus::Pass);
        assert_eq!(success_result.details.unwrap()["token_source"], "context");

        let failure_result = run_provider_check(
            &GitHubApiCheck,
            &success_ctx,
            "github",
            github_extractor,
            |_cfg: &GitHubConfig, _token: &str| -> ConnectFuture<'_> {
                Box::pin(async { Err("boom".to_string()) })
            },
        )
        .await;
        assert_eq!(failure_result.status, CheckStatus::Error);
        assert_eq!(failure_result.details.unwrap()["error"], "boom");
    }

    #[tokio::test]
    async fn provider_run_methods_cover_skip_success_and_store_error_paths() {
        let no_active = DiagnosticContext {
            config: Some(Config::default()),
            config_path: Some(PathBuf::from("config.toml")),
            config_exists: true,
            config_source: "test",
            config_path_error: None,
            config_load_error: None,
            credential_store: Arc::new(MemoryStore::new()),
            verbose: true,
        };
        assert_eq!(
            GitHubApiCheck.run(&no_active).await.status,
            CheckStatus::Skipped
        );

        let github_server = MockServer::start();
        github_server.mock(|when, then| {
            when.method(GET).path("/user");
            then.status(200).json_body(json!({
                "login": "octocat",
                "name": "The Octocat",
                "email": "octo@example.com"
            }));
        });
        let github_ctx = context_with_provider(
            Arc::new(MemoryStore::with_credentials([(
                "contexts.workspace.github.token".to_string(),
                "ghp_12345678901234567890".to_string(),
            )])),
            ContextConfig {
                github: Some(GitHubConfig {
                    owner: "o".to_string(),
                    repo: "r".to_string(),
                    base_url: Some(github_server.base_url()),
                }),
                ..Default::default()
            },
        );
        assert_eq!(
            GitHubApiCheck.run(&github_ctx).await.status,
            CheckStatus::Pass
        );

        let gitlab_server = MockServer::start();
        gitlab_server.mock(|when, then| {
            when.method(GET).path("/api/v4/user");
            then.status(200).json_body(json!({
                "username": "gitlab-user",
                "name": "GitLab User"
            }));
        });
        let gitlab_ctx = context_with_provider(
            Arc::new(MemoryStore::with_credentials([(
                "contexts.workspace.gitlab.token".to_string(),
                "glpat-12345678901234567890".to_string(),
            )])),
            ContextConfig {
                gitlab: Some(GitLabConfig {
                    url: gitlab_server.base_url(),
                    project_id: "group/project".to_string(),
                }),
                ..Default::default()
            },
        );
        assert_eq!(
            GitLabApiCheck.run(&gitlab_ctx).await.status,
            CheckStatus::Pass
        );

        let jira_server = MockServer::start();
        jira_server.mock(|when, then| {
            when.method(GET).path("/rest/api/2/myself");
            then.status(200).json_body(json!({
                "name": "jira-user",
                "displayName": "Jira User"
            }));
        });
        let jira_ctx = context_with_provider(
            Arc::new(MemoryStore::with_credentials([(
                "contexts.workspace.jira.token".to_string(),
                "user:token".to_string(),
            )])),
            ContextConfig {
                jira: Some(JiraConfig {
                    url: jira_server.base_url(),
                    project_key: "PROJ".to_string(),
                    email: "jira@example.com".to_string(),
                }),
                ..Default::default()
            },
        );
        assert_eq!(JiraApiCheck.run(&jira_ctx).await.status, CheckStatus::Pass);

        let clickup_error_ctx = context_with_provider(
            Arc::new(FailingStore),
            ContextConfig {
                clickup: Some(ClickUpConfig {
                    list_id: "123".to_string(),
                    team_id: None,
                }),
                ..Default::default()
            },
        );
        let clickup_result = ClickUpApiCheck.run(&clickup_error_ctx).await;
        assert_eq!(clickup_result.status, CheckStatus::Error);
        assert!(
            clickup_result
                .message
                .contains("Could not read clickup credentials")
        );
    }

    #[test]
    fn jira_base64_encoder_matches_expected() {
        assert_eq!(base64_encode("user:token"), "dXNlcjp0b2tlbg==");
    }

    #[test]
    fn rate_limit_from_headers_reads_gitlab_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("ratelimit-limit", HeaderValue::from_static("600"));
        headers.insert("ratelimit-remaining", HeaderValue::from_static("598"));

        let info = rate_limit_from_headers(&headers, "gitlab").unwrap();
        assert_eq!(info.limit.as_deref(), Some("600"));
        assert_eq!(info.remaining.as_deref(), Some("598"));
    }

    #[test]
    fn helper_functions_cover_parsing_and_serialization_paths() {
        let mut github_headers = HeaderMap::new();
        github_headers.insert("x-ratelimit-limit", HeaderValue::from_static("5000"));
        github_headers.insert("x-ratelimit-resource", HeaderValue::from_static("core"));
        let info = rate_limit_from_headers(&github_headers, "github").unwrap();
        assert_eq!(info.to_json()["resource"], "core");
        assert!(!info.is_empty());

        let mut jira_headers = HeaderMap::new();
        jira_headers.insert("x-ratelimit-limit", HeaderValue::from_static("100"));
        jira_headers.insert("x-ratelimit-nearlimit", HeaderValue::from_static("false"));
        assert_eq!(
            rate_limit_from_headers(&jira_headers, "jira")
                .unwrap()
                .to_json()["used"],
            "false"
        );
        assert!(rate_limit_from_headers(&HeaderMap::new(), "unknown").is_none());

        let identity = ProviderIdentity {
            username: "user".to_string(),
            name: Some("User".to_string()),
            email: Some("user@example.com".to_string()),
        };
        let details = connectivity_details(
            "github",
            "workspace",
            "contexts.workspace.github.token",
            "context",
            &ConnectivityOutcome {
                message: "connected".to_string(),
                user: Some(identity),
                rate_limit: Some(info),
            },
        );
        assert_eq!(details["provider"], "github");
        assert_eq!(details["user"]["username"], "user");

        assert_eq!(
            parse_error(reqwest::StatusCode::UNAUTHORIZED, "bad".to_string()).1,
            "authentication failed: bad"
        );
        assert_eq!(
            parse_error(reqwest::StatusCode::TOO_MANY_REQUESTS, "".to_string()).1,
            "rate limit exceeded: Too Many Requests"
        );
        assert_eq!(
            parse_error(
                reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                "boom".to_string()
            )
            .1,
            "server error: boom"
        );
        assert_eq!(
            parse_error(reqwest::StatusCode::BAD_REQUEST, "".to_string()).1,
            "request failed: Bad Request"
        );

        let skipped_result = skipped(&GitHubApiCheck, "skip me");
        assert_eq!(skipped_result.status, CheckStatus::Skipped);
        assert_eq!(skipped_result.message, "skip me");
    }
}
