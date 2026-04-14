//! Sentry error reporting integration.
//!
//! Provides optional Sentry integration for devboy-tools.
//! Enabled via `DEVBOY_SENTRY_DSN` environment variable or `[sentry]` config section.
//!
//! # Priority
//!
//! Environment variables take precedence over config file values:
//! - `DEVBOY_SENTRY_DSN` → `sentry.dsn`
//! - `DEVBOY_SENTRY_ENVIRONMENT` → `sentry.environment`
//! - `DEVBOY_SENTRY_SAMPLE_RATE` → `sentry.sample_rate`
//! - `DEVBOY_SENTRY_TRACES_SAMPLE_RATE` → `sentry.traces_sample_rate`

use crate::config::SentryConfig;
use tracing::info;

/// Sensitive header/field names to scrub from Sentry events.
const SENSITIVE_KEYS: &[&str] = &[
    "authorization",
    "x-gitlab-token",
    "x-api-key",
    "cookie",
    "private-token",
    "token",
    "api_key",
    "apikey",
    "secret",
    "password",
    "private_key",
];

/// Initialize Sentry error reporting.
///
/// Returns `Some(guard)` if Sentry was initialized, `None` if disabled.
/// The guard **must** be kept alive for the entire process lifetime —
/// dropping it flushes pending events and shuts down the Sentry client.
///
/// # Arguments
///
/// * `config` - Optional Sentry config from config.toml
/// * `version` - Application version string for the `release` tag
pub fn init_sentry(
    config: Option<&SentryConfig>,
    version: &str,
) -> Option<sentry::ClientInitGuard> {
    let default_config = SentryConfig::default();
    let config = config.unwrap_or(&default_config);

    // DSN: env var overrides config
    let dsn = std::env::var("DEVBOY_SENTRY_DSN")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| config.dsn.clone());

    let dsn = match dsn {
        Some(dsn) if !dsn.is_empty() => dsn,
        _ => return None,
    };

    // Environment: env var overrides config
    let environment = std::env::var("DEVBOY_SENTRY_ENVIRONMENT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| config.environment.clone());

    // Sample rate: env var overrides config, default 1.0
    let sample_rate = std::env::var("DEVBOY_SENTRY_SAMPLE_RATE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .or(config.sample_rate)
        .unwrap_or(1.0);

    // Traces sample rate: env var overrides config, default 0.0
    let traces_sample_rate = std::env::var("DEVBOY_SENTRY_TRACES_SAMPLE_RATE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .or(config.traces_sample_rate)
        .unwrap_or(0.0);

    let release = format!("devboy-tools@{version}");

    let guard = sentry::init(sentry::ClientOptions {
        dsn: dsn.parse().ok(),
        release: Some(release.into()),
        environment: environment.map(Into::into),
        sample_rate,
        traces_sample_rate,
        before_send: Some(std::sync::Arc::new(scrub_sensitive_data)),
        ..Default::default()
    });

    if guard.is_enabled() {
        info!("Sentry error reporting enabled");
    }

    Some(guard)
}

/// Scrub sensitive data from Sentry events before sending.
fn scrub_sensitive_data(
    mut event: sentry::protocol::Event<'static>,
) -> Option<sentry::protocol::Event<'static>> {
    // Scrub request headers
    if let Some(ref mut request) = event.request {
        scrub_map(&mut request.headers);
    }

    // Scrub extra context for sensitive keys
    let keys_to_scrub: Vec<String> = event
        .extra
        .keys()
        .filter(|k| is_sensitive_key(k))
        .cloned()
        .collect();
    for key in keys_to_scrub {
        event
            .extra
            .insert(key, sentry::protocol::Value::String("[Filtered]".to_string()));
    }

    Some(event)
}

/// Check if a key name looks like it contains sensitive data.
fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    SENSITIVE_KEYS.iter().any(|&k| lower.contains(k))
}

/// Scrub sensitive values from a header map.
fn scrub_map(map: &mut std::collections::BTreeMap<String, String>) {
    let keys_to_scrub: Vec<String> = map
        .keys()
        .filter(|k| is_sensitive_key(k))
        .cloned()
        .collect();
    for key in keys_to_scrub {
        map.insert(key, "[Filtered]".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_sensitive_key() {
        assert!(is_sensitive_key("Authorization"));
        assert!(is_sensitive_key("x-gitlab-token"));
        assert!(is_sensitive_key("X-API-KEY"));
        assert!(is_sensitive_key("my_secret_field"));
        assert!(is_sensitive_key("PRIVATE-TOKEN"));
        assert!(!is_sensitive_key("content-type"));
        assert!(!is_sensitive_key("user-agent"));
        assert!(!is_sensitive_key("tool_name"));
    }

    // Note: init_sentry tests that depend on env vars are in devboy-cli
    // integration tests which use temp-env for safe env var manipulation.
    // Here we only test the pure logic that doesn't depend on env state.

    #[test]
    fn test_sentry_config_default() {
        let config = SentryConfig::default();
        assert!(config.dsn.is_none());
        assert!(config.environment.is_none());
        assert!(config.sample_rate.is_none());
        assert!(config.traces_sample_rate.is_none());
    }
}
