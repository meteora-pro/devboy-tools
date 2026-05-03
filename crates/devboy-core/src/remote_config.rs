//! Remote configuration fetching.
//!
//! Fetches TOML configuration from a remote URL and merges it with the local
//! config. This provides a generic mechanism for centralized configuration
//! management — any server can serve TOML config over HTTP with Bearer auth.
//!
//! # Configuration
//!
//! Via `config.toml`:
//! ```toml
//! [remote_config]
//! url = "https://example.com/api/devboy-config"
//! token_key = "remote_config.token"
//! ```
//!
//! Via environment variables (take priority over config file):
//! - `DEVBOY_REMOTE_CONFIG_URL` — URL to fetch config from
//! - `DEVBOY_REMOTE_CONFIG_TOKEN` — Bearer token for authentication
//!
//! # Behavior
//!
//! - Remote values override local values (remote wins)
//! - If fetch fails, a warning is printed and local config is used unchanged
//! - Timeout: 10 seconds
//! - Response must be valid TOML that deserializes into `Config`

use crate::config::Config;

/// Fetch remote config and merge it into the provided local config.
///
/// Returns the merged config. If fetch fails for any reason, returns the
/// original config unchanged (with a warning printed to stderr).
///
/// # Arguments
///
/// Resolved remote-config URL from env var or `[remote_config]` config
/// block. Returns `None` if neither source provides a non-empty URL.
///
/// Used by `devboy doctor` and `devboy context list` to detect the
/// "thin client / proxy" mode regardless of whether the URL came from
/// the env var (which `fetch_and_merge` honours but doesn't write into
/// `Config`) or the on-disk config file.
pub fn resolve_url(local_config: &Config) -> Option<String> {
    std::env::var("DEVBOY_REMOTE_CONFIG_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            local_config
                .remote_config
                .as_ref()
                .and_then(|rc| rc.url.as_ref().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
        })
}

/// Redact a URL for safe display in diagnostic messages: drop userinfo
/// (basic-auth credentials in `https://user:pass@host/...`) and any
/// query string or fragment. Scheme + host + port + path are preserved.
///
/// Lightweight string-level parser (no `url` crate dep) that mirrors
/// the redaction we already do when logging remote-config fetch
/// failures. Anything that doesn't look like an `<scheme>://...` URL
/// passes through with only the query/fragment stripped — we'd rather
/// echo a malformed value than panic, but credentials in non-URL
/// strings are not detected.
pub fn redact_url_for_display(raw: &str) -> String {
    let raw = raw.trim();
    let (scheme_with_sep, rest) = match raw.find("://") {
        Some(idx) => (&raw[..idx + 3], &raw[idx + 3..]),
        None => {
            // Not a `scheme://` URL — strip query/fragment and return.
            let stripped = raw.split_once('?').map(|(p, _)| p).unwrap_or(raw);
            let stripped = stripped.split_once('#').map(|(p, _)| p).unwrap_or(stripped);
            return stripped.to_string();
        }
    };

    // Authority ends at the first `/`, `?`, or `#`. Userinfo is
    // everything before the rightmost `@` inside that authority.
    let auth_end = rest
        .find(|c: char| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let (auth, tail) = rest.split_at(auth_end);
    let host = match auth.rfind('@') {
        Some(at) => &auth[at + 1..],
        None => auth,
    };

    // Strip query string and fragment from tail.
    let tail = tail.split_once('?').map(|(p, _)| p).unwrap_or(tail);
    let tail = tail.split_once('#').map(|(p, _)| p).unwrap_or(tail);

    format!("{scheme_with_sep}{host}{tail}")
}

/// * `local_config` - The locally loaded config
/// * `token_from_keychain` - Optional token resolved from keychain via `token_key`
pub async fn fetch_and_merge(local_config: Config, token_from_keychain: Option<&str>) -> Config {
    // Resolve URL: env var overrides config
    let url = std::env::var("DEVBOY_REMOTE_CONFIG_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            local_config
                .remote_config
                .as_ref()
                .and_then(|rc| rc.url.as_ref().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
        });

    let url = match url {
        Some(url) => url,
        None => return local_config,
    };

    // Resolve token: env var → keychain → none
    let token = std::env::var("DEVBOY_REMOTE_CONFIG_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| token_from_keychain.map(|s| s.to_string()));

    match fetch_remote_toml(&url, token.as_deref()).await {
        Ok(remote_config) => merge_configs(local_config, remote_config),
        Err(e) => {
            // Strip query params AND userinfo to avoid leaking credentials
            let safe_url = redact_url(&url);
            eprintln!(
                "[devboy] Failed to fetch remote config from {safe_url}: {e}. Using local config."
            );
            local_config
        }
    }
}

/// Maximum response size for remote config (1 MB). Prevents OOM from malicious endpoints.
const MAX_REMOTE_CONFIG_SIZE: u64 = 1_024 * 1_024;

/// Redact URL for safe logging: strip query params and userinfo.
/// `https://user:pass@host.com/path?token=x` → `https://host.com/path`
fn redact_url(url: &str) -> String {
    let without_query = url.split('?').next().unwrap_or(url);
    // Strip userinfo (user:pass@)
    if let Some(scheme_end) = without_query.find("://") {
        let after_scheme = &without_query[scheme_end + 3..];
        if let Some(at_pos) = after_scheme.find('@') {
            return format!(
                "{}://{}",
                &without_query[..scheme_end],
                &after_scheme[at_pos + 1..]
            );
        }
    }
    without_query.to_string()
}

/// Fetch TOML config from a remote URL.
async fn fetch_remote_toml(url: &str, token: Option<&str>) -> Result<Config, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let mut request = client
        .get(url)
        .header("Accept", "application/toml, text/plain");

    if let Some(token) = token {
        request = request.bearer_auth(token);
    }

    let response = request.send().await.map_err(|e| format!("{e}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }

    // Check Content-Length if available
    if let Some(len) = response.content_length()
        && len > MAX_REMOTE_CONFIG_SIZE
    {
        return Err(format!(
            "Response too large: {len} bytes (max {MAX_REMOTE_CONFIG_SIZE})"
        ));
    }

    let body = response.text().await.map_err(|e| format!("{e}"))?;

    // Also check actual body size (Content-Length may be absent)
    if body.len() as u64 > MAX_REMOTE_CONFIG_SIZE {
        return Err(format!(
            "Response too large: {} bytes (max {MAX_REMOTE_CONFIG_SIZE})",
            body.len()
        ));
    }

    toml::from_str::<Config>(&body).map_err(|e| format!("TOML parse error: {e}"))
}

/// Merge remote config into local config.
///
/// Only fields that are present (Some/non-empty) in the remote config override
/// local values. Remote config cannot clear/reset a local value — omitting a
/// field in remote config preserves the local value.
fn merge_configs(mut local: Config, remote: Config) -> Config {
    // Provider configs: remote overrides if present
    if remote.github.is_some() {
        local.github = remote.github;
    }
    if remote.gitlab.is_some() {
        local.gitlab = remote.gitlab;
    }
    if remote.clickup.is_some() {
        local.clickup = remote.clickup;
    }
    if remote.jira.is_some() {
        local.jira = remote.jira;
    }
    if remote.fireflies.is_some() {
        local.fireflies = remote.fireflies;
    }
    if remote.slack.is_some() {
        local.slack = remote.slack;
    }

    // Contexts: merge by name (remote contexts override local ones with same name)
    for (name, context) in remote.contexts {
        local.contexts.insert(name, context);
    }

    if remote.active_context.is_some() {
        local.active_context = remote.active_context;
    }

    // Proxy servers: append remote proxies (don't replace local ones)
    if !remote.proxy_mcp_servers.is_empty() {
        local.proxy_mcp_servers.extend(remote.proxy_mcp_servers);
    }

    // Builtin tools: remote overrides if non-empty
    if !remote.builtin_tools.is_empty() {
        local.builtin_tools = remote.builtin_tools;
    }

    // Format pipeline: remote overrides if present
    if remote.format_pipeline.is_some() {
        local.format_pipeline = remote.format_pipeline;
    }

    // Sentry: remote overrides if present
    if remote.sentry.is_some() {
        local.sentry = remote.sentry;
    }

    // Don't copy remote_config from remote (avoid recursive fetching)

    local
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RemoteConfigSettings, SentryConfig};

    #[test]
    fn redact_url_strips_userinfo_and_query() {
        assert_eq!(
            redact_url_for_display("https://user:pass@example.com/api/config?token=abc"),
            "https://example.com/api/config"
        );
    }

    #[test]
    fn redact_url_keeps_path_and_port() {
        assert_eq!(
            redact_url_for_display("https://host.example:8443/api/config/mcp"),
            "https://host.example:8443/api/config/mcp"
        );
    }

    #[test]
    fn redact_url_strips_only_userinfo_when_no_query() {
        assert_eq!(
            redact_url_for_display("https://alice@example.com/p"),
            "https://example.com/p"
        );
    }

    #[test]
    fn redact_url_strips_only_query_when_no_userinfo() {
        assert_eq!(
            redact_url_for_display("https://example.com/p?secret=xyz#frag"),
            "https://example.com/p"
        );
    }

    #[test]
    fn redact_url_handles_non_url_string_without_panic() {
        // No scheme://, no panic — just strip query/fragment if any.
        assert_eq!(redact_url_for_display("not-a-url"), "not-a-url");
        assert_eq!(redact_url_for_display("not-a-url?q=secret"), "not-a-url");
    }

    #[test]
    fn redact_url_handles_at_in_path() {
        // The `@` in `/users/foo@bar/items` is part of the path, not
        // userinfo — must not be stripped.
        assert_eq!(
            redact_url_for_display("https://example.com/users/foo@bar/items"),
            "https://example.com/users/foo@bar/items"
        );
    }

    #[test]
    fn resolve_url_returns_config_url_when_set() {
        let cfg = Config {
            remote_config: Some(RemoteConfigSettings {
                url: Some("https://from-config.example/".to_string()),
                token_key: None,
            }),
            ..Default::default()
        };
        // Note: env var precedence (DEVBOY_REMOTE_CONFIG_URL > config)
        // is exercised end-to-end via the existing remote_config
        // integration test fixtures; not unit-tested here because
        // `unsafe_code=forbid` blocks `set_var`.
        assert_eq!(
            resolve_url(&cfg).as_deref(),
            Some("https://from-config.example/")
        );
    }

    #[test]
    fn resolve_url_returns_none_for_default_config() {
        let cfg = Config::default();
        // May still return Some(...) if a stray DEVBOY_REMOTE_CONFIG_URL
        // is set in the test process environment — assert "none, OR
        // exactly the env var value" so the test is order-independent.
        let got = resolve_url(&cfg);
        match (std::env::var("DEVBOY_REMOTE_CONFIG_URL").ok(), got) {
            (None, None) => {}
            (Some(env), Some(got)) => assert_eq!(env.trim(), got),
            (None, Some(got)) => panic!("expected None, got Some({got})"),
            (Some(env), None) => panic!("expected Some({env}), got None"),
        }
    }

    #[test]
    fn test_merge_configs_remote_overrides_sentry() {
        let local = Config::default();
        let remote = Config {
            sentry: Some(SentryConfig {
                dsn: Some("https://key@sentry.io/1".to_string()),
                environment: Some("production".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let merged = merge_configs(local, remote);
        let sentry = merged.sentry.unwrap();
        assert_eq!(sentry.dsn.unwrap(), "https://key@sentry.io/1");
        assert_eq!(sentry.environment.unwrap(), "production");
    }

    #[test]
    fn test_merge_configs_local_preserved_when_remote_empty() {
        let local = Config {
            sentry: Some(SentryConfig {
                dsn: Some("https://local@sentry.io/1".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let remote = Config::default();

        let merged = merge_configs(local, remote);
        let sentry = merged.sentry.unwrap();
        assert_eq!(sentry.dsn.unwrap(), "https://local@sentry.io/1");
    }

    #[test]
    fn test_merge_configs_contexts_merged() {
        let mut local = Config::default();
        local.contexts.insert(
            "local-ctx".to_string(),
            crate::config::ContextConfig::default(),
        );

        let mut remote = Config::default();
        remote.contexts.insert(
            "remote-ctx".to_string(),
            crate::config::ContextConfig::default(),
        );

        let merged = merge_configs(local, remote);
        assert!(merged.contexts.contains_key("local-ctx"));
        assert!(merged.contexts.contains_key("remote-ctx"));
    }

    #[test]
    fn test_merge_configs_remote_config_not_copied() {
        let local = Config {
            remote_config: Some(RemoteConfigSettings {
                url: Some("https://local.com/config".to_string()),
                token_key: None,
            }),
            ..Default::default()
        };
        let remote = Config {
            remote_config: Some(RemoteConfigSettings {
                url: Some("https://should-not-be-copied.com".to_string()),
                token_key: None,
            }),
            ..Default::default()
        };

        let merged = merge_configs(local, remote);
        // remote_config should stay as the local one (not overwritten)
        assert_eq!(
            merged.remote_config.unwrap().url.unwrap(),
            "https://local.com/config"
        );
    }
}
