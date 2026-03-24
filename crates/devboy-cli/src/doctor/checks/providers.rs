use crate::doctor::checks::{resolve_active_provider_context, resolve_secret};
use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};
use async_trait::async_trait;
use devboy_core::{ClickUpConfig, GitHubConfig, GitLabConfig, JiraConfig};
use reqwest::header::{HeaderMap, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use reqwest::{Client, Method};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

pub struct GitHubApiCheck;
pub struct GitLabApiCheck;
pub struct ClickUpApiCheck;
pub struct JiraApiCheck;

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
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct ClickUpTasksResponse {
    tasks: Vec<Value>,
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

async fn run_provider_check(
    check: &dyn DiagnosticCheck,
    ctx: &DiagnosticContext,
    provider: &'static str,
    configured: bool,
    connect: impl std::future::Future<Output = Result<ConnectivityOutcome, String>>,
) -> CheckResult {
    let Some(config) = &ctx.config else {
        return skipped(check, "Skipped because config could not be loaded");
    };

    let Some(active) = resolve_active_provider_context(config) else {
        return skipped(check, "Skipped because no active context could be resolved");
    };

    if !configured {
        return skipped(
            check,
            &format!(
                "Skipped because {} is not configured in context '{}'",
                provider, active.name
            ),
        );
    }

    let secret = match resolve_secret(ctx, Some(&active.name), provider) {
        Ok(Some(secret)) => secret,
        Ok(None) => {
            return skipped(
                check,
                &format!(
                    "Skipped because {provider} credentials are missing for context '{}'",
                    active.name
                ),
            )
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
            }
        }
    };

    match connect.await {
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
        let Some(active) = ctx
            .config
            .as_ref()
            .and_then(resolve_active_provider_context)
        else {
            return skipped(self, "Skipped because no active context could be resolved");
        };

        let Some(config) = active.config.github else {
            return skipped(
                self,
                &format!(
                    "Skipped because github is not configured in context '{}'",
                    active.name
                ),
            );
        };

        let token = match resolve_secret(ctx, Some(&active.name), "github") {
            Ok(Some(secret)) => secret.value,
            Ok(None) => {
                return skipped(
                    self,
                    &format!(
                        "Skipped because github credentials are missing for context '{}'",
                        active.name
                    ),
                )
            }
            Err(error) => {
                return CheckResult {
                    id: self.id().to_string(),
                    category: self.category().to_string(),
                    name: self.name().to_string(),
                    status: CheckStatus::Error,
                    message: format!("Could not read github credentials: {error}"),
                    details: ctx
                        .verbose
                        .then(|| json!({ "provider": "github", "error": error })),
                    fix_command: None,
                    fix_url: None,
                }
            }
        };

        run_provider_check(
            self,
            ctx,
            "github",
            true,
            github_connectivity(&config, &token),
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
        let Some(active) = ctx
            .config
            .as_ref()
            .and_then(resolve_active_provider_context)
        else {
            return skipped(self, "Skipped because no active context could be resolved");
        };

        let Some(config) = active.config.gitlab else {
            return skipped(
                self,
                &format!(
                    "Skipped because gitlab is not configured in context '{}'",
                    active.name
                ),
            );
        };

        let token = match resolve_secret(ctx, Some(&active.name), "gitlab") {
            Ok(Some(secret)) => secret.value,
            Ok(None) => {
                return skipped(
                    self,
                    &format!(
                        "Skipped because gitlab credentials are missing for context '{}'",
                        active.name
                    ),
                )
            }
            Err(error) => {
                return CheckResult {
                    id: self.id().to_string(),
                    category: self.category().to_string(),
                    name: self.name().to_string(),
                    status: CheckStatus::Error,
                    message: format!("Could not read gitlab credentials: {error}"),
                    details: ctx
                        .verbose
                        .then(|| json!({ "provider": "gitlab", "error": error })),
                    fix_command: None,
                    fix_url: None,
                }
            }
        };

        run_provider_check(
            self,
            ctx,
            "gitlab",
            true,
            gitlab_connectivity(&config, &token),
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
        let Some(active) = ctx
            .config
            .as_ref()
            .and_then(resolve_active_provider_context)
        else {
            return skipped(self, "Skipped because no active context could be resolved");
        };

        let Some(config) = active.config.clickup else {
            return skipped(
                self,
                &format!(
                    "Skipped because clickup is not configured in context '{}'",
                    active.name
                ),
            );
        };

        let token = match resolve_secret(ctx, Some(&active.name), "clickup") {
            Ok(Some(secret)) => secret.value,
            Ok(None) => {
                return skipped(
                    self,
                    &format!(
                        "Skipped because clickup credentials are missing for context '{}'",
                        active.name
                    ),
                )
            }
            Err(error) => {
                return CheckResult {
                    id: self.id().to_string(),
                    category: self.category().to_string(),
                    name: self.name().to_string(),
                    status: CheckStatus::Error,
                    message: format!("Could not read clickup credentials: {error}"),
                    details: ctx
                        .verbose
                        .then(|| json!({ "provider": "clickup", "error": error })),
                    fix_command: None,
                    fix_url: None,
                }
            }
        };

        run_provider_check(
            self,
            ctx,
            "clickup",
            true,
            clickup_connectivity(&config, &token),
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
        let Some(active) = ctx
            .config
            .as_ref()
            .and_then(resolve_active_provider_context)
        else {
            return skipped(self, "Skipped because no active context could be resolved");
        };

        let Some(config) = active.config.jira else {
            return skipped(
                self,
                &format!(
                    "Skipped because jira is not configured in context '{}'",
                    active.name
                ),
            );
        };

        let token = match resolve_secret(ctx, Some(&active.name), "jira") {
            Ok(Some(secret)) => secret.value,
            Ok(None) => {
                return skipped(
                    self,
                    &format!(
                        "Skipped because jira credentials are missing for context '{}'",
                        active.name
                    ),
                )
            }
            Err(error) => {
                return CheckResult {
                    id: self.id().to_string(),
                    category: self.category().to_string(),
                    name: self.name().to_string(),
                    status: CheckStatus::Error,
                    message: format!("Could not read jira credentials: {error}"),
                    details: ctx
                        .verbose
                        .then(|| json!({ "provider": "jira", "error": error })),
                    fix_command: None,
                    fix_url: None,
                }
            }
        };

        run_provider_check(self, ctx, "jira", true, jira_connectivity(&config, &token)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::GET;
    use httpmock::MockServer;
    use reqwest::header::HeaderValue;

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
}
