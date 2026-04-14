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
/// Returns `Some(guard)` if Sentry was initialized and enabled, `None` otherwise.
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

    // DSN: env var overrides config, trim whitespace
    let dsn = std::env::var("DEVBOY_SENTRY_DSN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| config.dsn.as_ref().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())?;

    // Validate DSN is parseable (don't log the full DSN — it may contain credentials).
    // Use eprintln! instead of tracing::warn! because this runs before the tracing
    // subscriber is initialized, so tracing events would be silently dropped.
    let parsed_dsn = match dsn.parse::<sentry::types::Dsn>() {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("[devboy] Invalid Sentry DSN: {e}. Sentry will be disabled.");
            return None;
        }
    };

    // Environment: env var overrides config
    let environment = std::env::var("DEVBOY_SENTRY_ENVIRONMENT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| config.environment.clone());

    // Sample rate: env var overrides config, default 1.0, clamped to 0.0–1.0
    let sample_rate = std::env::var("DEVBOY_SENTRY_SAMPLE_RATE")
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .or(config.sample_rate)
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);

    // Traces sample rate: env var overrides config, default 0.0, clamped to 0.0–1.0
    let traces_sample_rate = std::env::var("DEVBOY_SENTRY_TRACES_SAMPLE_RATE")
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .or(config.traces_sample_rate)
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);

    let release = format!("devboy-tools@{version}");

    let guard = sentry::init(sentry::ClientOptions {
        dsn: parsed_dsn,
        release: Some(release.into()),
        environment: environment.map(Into::into),
        sample_rate,
        traces_sample_rate,
        before_send: Some(std::sync::Arc::new(scrub_sensitive_data)),
        before_breadcrumb: Some(std::sync::Arc::new(scrub_breadcrumb)),
        ..Default::default()
    });

    if guard.is_enabled() {
        // Use eprintln! because tracing subscriber is not yet initialized at this point.
        eprintln!("[devboy] Sentry error reporting enabled");
    }

    Some(guard)
}

/// Scrub sensitive data from Sentry events before sending.
fn scrub_sensitive_data(
    mut event: sentry::protocol::Event<'static>,
) -> Option<sentry::protocol::Event<'static>> {
    // Scrub event message (e.g., from capture_message with error strings)
    if let Some(ref mut message) = event.message {
        *message = scrub_url_credentials(message);
    }

    // Scrub exception values (error messages may contain URLs with tokens)
    for exception in &mut event.exception.values {
        if let Some(ref mut value) = exception.value {
            *value = scrub_url_credentials(value);
        }
    }

    // Scrub request headers, URL and query string
    if let Some(ref mut request) = event.request {
        scrub_map(&mut request.headers);
        if let Some(ref url) = request.url {
            let scrubbed = scrub_url_credentials(url.as_str());
            if let Ok(new_url) = scrubbed.parse() {
                request.url = Some(new_url);
            }
        }
        if let Some(ref mut query) = request.query_string {
            *query = scrub_url_credentials(query);
        }
    }

    // Scrub extra context for sensitive keys
    let keys_to_scrub: Vec<String> = event
        .extra
        .keys()
        .filter(|k| is_sensitive_key(k))
        .cloned()
        .collect();
    for key in keys_to_scrub {
        event.extra.insert(
            key,
            sentry::protocol::Value::String("[Filtered]".to_string()),
        );
    }

    Some(event)
}

/// Scrub sensitive data from breadcrumbs before attaching to events.
///
/// Breadcrumbs are created from `tracing` info/warn logs and may contain
/// URLs with embedded credentials or query tokens (e.g., proxy URLs).
fn scrub_breadcrumb(
    mut breadcrumb: sentry::protocol::Breadcrumb,
) -> Option<sentry::protocol::Breadcrumb> {
    if let Some(ref mut message) = breadcrumb.message {
        *message = scrub_url_credentials(message);
    }
    // Scrub breadcrumb data values
    for value in breadcrumb.data.values_mut() {
        if let sentry::protocol::Value::String(s) = value {
            *s = scrub_url_credentials(s);
        }
    }
    Some(breadcrumb)
}

/// Scrub credentials from URLs in a string.
///
/// Replaces `user:password@host` patterns and sensitive query parameters
/// (token, key, secret, password) with `[Filtered]`.
fn scrub_url_credentials(input: &str) -> String {
    let mut result = input.to_string();
    // Scrub userinfo in URLs: https://user:pass@host → https://[Filtered]@host
    if let Some(start) = result.find("://") {
        let after_scheme = start + 3;
        if let Some(at_pos) = result[after_scheme..].find('@') {
            let abs_at = after_scheme + at_pos;
            // Only scrub if there's a colon before @ (looks like user:pass)
            if result[after_scheme..abs_at].contains(':') {
                result = format!("{}[Filtered]{}", &result[..after_scheme], &result[abs_at..]);
            }
        }
    }
    // Scrub sensitive query params: ?token=xxx&key=yyy
    // Single case-insensitive pass per param (no separate uppercase pattern needed).
    for param in &["token", "key", "secret", "password", "api_key", "apikey"] {
        let pat = format!("{param}=");
        let pat_lower = pat.to_lowercase();
        let mut search_from = 0;
        // Precompute lowercase once, rebuild only when the string changes
        let mut result_lower = result.to_lowercase();
        while let Some(rel_pos) = result_lower[search_from..].find(&pat_lower) {
            let pos = search_from + rel_pos;
            let value_start = pos + pat.len();
            let value_end = result[value_start..]
                .find(['&', '#', ' '])
                .map(|i| value_start + i)
                .unwrap_or(result.len());
            // Preserve original case of the param name from the input
            let original_param = result[pos..value_start].to_string();
            result = format!(
                "{}{}[Filtered]{}",
                &result[..pos],
                original_param,
                &result[value_end..]
            );
            // Advance past the replacement to avoid re-matching
            search_from = pos + original_param.len() + "[Filtered]".len();
            result_lower = result.to_lowercase();
        }
    }
    result
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

    #[test]
    fn test_scrub_url_credentials() {
        // Userinfo in URL
        assert_eq!(
            scrub_url_credentials("https://user:pass@sentry.io/123"),
            "https://[Filtered]@sentry.io/123"
        );
        // Query param token
        assert_eq!(
            scrub_url_credentials("https://host.com/api?token=secret123&foo=bar"),
            "https://host.com/api?token=[Filtered]&foo=bar"
        );
        // Multiple sensitive params
        assert_eq!(
            scrub_url_credentials("https://host.com?key=abc&password=xyz"),
            "https://host.com?key=[Filtered]&password=[Filtered]"
        );
        // No credentials — unchanged
        assert_eq!(
            scrub_url_credentials("https://host.com/path?page=1"),
            "https://host.com/path?page=1"
        );
        // Plain text without URL — unchanged
        assert_eq!(
            scrub_url_credentials("Connected to proxy at host:8080"),
            "Connected to proxy at host:8080"
        );
        // Case-insensitive param matching (TOKEN=, Token=)
        assert_eq!(
            scrub_url_credentials("https://host.com?TOKEN=secret"),
            "https://host.com?TOKEN=[Filtered]"
        );
        assert_eq!(
            scrub_url_credentials("https://host.com?Token=abc123"),
            "https://host.com?Token=[Filtered]"
        );
        // api_key param
        assert_eq!(
            scrub_url_credentials("https://host.com?api_key=xyz&page=1"),
            "https://host.com?api_key=[Filtered]&page=1"
        );
    }

    #[test]
    fn test_sentry_config_default() {
        let config = SentryConfig::default();
        assert!(config.dsn.is_none());
        assert!(config.environment.is_none());
        assert!(config.sample_rate.is_none());
        assert!(config.traces_sample_rate.is_none());
    }
}
