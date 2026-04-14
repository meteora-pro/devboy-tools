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
                .and_then(|rc| rc.url.clone())
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
            eprintln!(
                "[devboy] Failed to fetch remote config from {url}: {e}. Using local config."
            );
            local_config
        }
    }
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

    let body = response.text().await.map_err(|e| format!("{e}"))?;

    toml::from_str::<Config>(&body).map_err(|e| format!("TOML parse error: {e}"))
}

/// Merge remote config into local config. Remote values override local values
/// for fields that are `Some` / non-empty in the remote config.
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
