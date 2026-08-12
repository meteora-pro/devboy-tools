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

use crate::config::{Config, SecretsProfile};

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
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
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

/// The secrets posture an operator may hand a fresh install,
/// with everything they may not hand it left out.
///
/// See [`fetch_secrets_defaults`] for why this is a separate type
/// rather than reusing `SecretsConfig`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteSecretsDefaults {
    /// Unlock-window profile.
    pub profile: Option<SecretsProfile>,
    /// How long the daemon holds the key after an unlock.
    pub unlock_ttl_seconds: Option<u64>,
    /// Ceiling on any single unlock window.
    pub max_unlock_ttl_seconds: Option<u64>,
    /// Re-lock after this much inactivity.
    pub idle_relock_seconds: Option<u64>,
    /// Whether the OS keychain joins the chain.
    pub keychain_enabled: Option<bool>,
}

impl RemoteSecretsDefaults {
    /// Nothing to apply.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// One line per field that will change, for printing back to
    /// the user. An operator default the user cannot see is an
    /// operator default the user cannot argue with.
    pub fn describe(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(p) = self.profile {
            let name = match p {
                SecretsProfile::Convenient => "convenient",
                SecretsProfile::Strict => "strict",
            };
            out.push(format!("secrets.profile = {name}"));
        }
        if let Some(v) = self.unlock_ttl_seconds {
            out.push(format!("secrets.unlock_ttl_seconds = {v}"));
        }
        if let Some(v) = self.max_unlock_ttl_seconds {
            out.push(format!("secrets.max_unlock_ttl_seconds = {v}"));
        }
        if let Some(v) = self.idle_relock_seconds {
            out.push(format!("secrets.idle_relock_seconds = {v}"));
        }
        if let Some(v) = self.keychain_enabled {
            out.push(format!("secrets.keychain.enabled = {v}"));
        }
        out
    }
}

/// Read the secrets posture an operator wants a fresh install to
/// start from, out of an already-fetched remote config.
///
/// # Why this is not part of `merge_configs`
///
/// `merge_configs` runs on **every** invocation. Letting it carry
/// the secrets section would mean the posture is renegotiated
/// over the network each time devboy runs: whoever serves the
/// config could turn the OS keychain back on, or stretch the
/// unlock window, on a machine that is not theirs, and nothing
/// would appear in any file the user reads. So the section is
/// applied once, at `init`, written into the local config where
/// it is visible and editable, and never re-applied behind the
/// user's back.
///
/// # Why two fields are dropped
///
/// `keyfile_path` is a path into the user's own filesystem, and
/// the remote side has no business choosing it — it decides which
/// file gets read as key material. `migration_complete` is an
/// assertion about what is left in *this* machine's OS keychain,
/// which only this machine can make.
///
/// Both are dropped silently rather than rejected: a config
/// server serving one config to a fleet may legitimately carry
/// fields some clients ignore, and failing an install over a
/// field that was never going to be honoured helps nobody. The
/// caller prints what *was* applied, so the absence is visible by
/// omission.
pub fn read_secrets_defaults(remote: &Config) -> Result<RemoteSecretsDefaults, String> {
    let Some(secrets) = remote.secrets.as_ref() else {
        return Ok(RemoteSecretsDefaults::default());
    };

    let out = RemoteSecretsDefaults {
        // `profile` is not an `Option` on the wire, so an omitted
        // field is indistinguishable from the default. Carrying
        // the default over is harmless: it is what a fresh
        // install would pick anyway.
        profile: Some(secrets.profile),
        unlock_ttl_seconds: secrets.unlock_ttl_seconds,
        max_unlock_ttl_seconds: secrets.max_unlock_ttl_seconds,
        idle_relock_seconds: secrets.idle_relock_seconds,
        keychain_enabled: Some(secrets.keychain.enabled),
    };

    validate_secrets_defaults(&out)?;
    Ok(out)
}

/// Reject a posture that cannot mean what it says.
///
/// Loudly, not by clamping: a silently corrected window is one
/// the operator believes they set and the user believes they
/// have, and neither is true.
fn validate_secrets_defaults(d: &RemoteSecretsDefaults) -> Result<(), String> {
    if d.unlock_ttl_seconds == Some(0) {
        return Err(
            "secrets.unlock_ttl_seconds is 0 — an unlock that expires immediately is \
                    never what was meant"
                .to_owned(),
        );
    }
    if d.max_unlock_ttl_seconds == Some(0) {
        return Err("secrets.max_unlock_ttl_seconds is 0 — that forbids every unlock".to_owned());
    }
    if let (Some(ttl), Some(ceiling)) = (d.unlock_ttl_seconds, d.max_unlock_ttl_seconds)
        && ttl > ceiling
    {
        return Err(format!(
            "secrets.unlock_ttl_seconds ({ttl}) is above secrets.max_unlock_ttl_seconds \
             ({ceiling}) — the ceiling would silently win and the configured window would \
             never apply"
        ));
    }
    Ok(())
}

/// Fetch a remote config and read the secrets posture out of it.
///
/// Used by `devboy init`. Errors are the caller's to report and
/// survive: an unreachable config server must not stop someone
/// setting up their machine, it just means the built-in defaults
/// apply.
pub async fn fetch_secrets_defaults(
    url: &str,
    token: Option<&str>,
) -> Result<RemoteSecretsDefaults, String> {
    let remote = fetch_remote_toml(url, token).await?;
    read_secrets_defaults(&remote)
}

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

    // -- operator-supplied secrets posture ------------------------

    fn remote_with_secrets(toml_body: &str) -> Config {
        toml::from_str(toml_body).expect("fixture parses")
    }

    #[test]
    fn a_remote_config_without_a_secrets_section_supplies_nothing() {
        let remote = remote_with_secrets("");
        let d = read_secrets_defaults(&remote).unwrap();
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn an_operator_can_pin_the_profile_and_the_window() {
        let remote = remote_with_secrets(
            r#"
            [secrets]
            profile = "strict"
            unlock_ttl_seconds = 900
            max_unlock_ttl_seconds = 3600
            idle_relock_seconds = 300
            "#,
        );

        let d = read_secrets_defaults(&remote).unwrap();

        assert_eq!(d.profile, Some(SecretsProfile::Strict));
        assert_eq!(d.unlock_ttl_seconds, Some(900));
        assert_eq!(d.max_unlock_ttl_seconds, Some(3600));
        assert_eq!(d.idle_relock_seconds, Some(300));
    }

    #[test]
    fn an_operator_can_turn_the_keychain_back_on_for_their_fleet() {
        let remote = remote_with_secrets(
            r#"
            [secrets.keychain]
            enabled = true
            "#,
        );

        assert_eq!(
            read_secrets_defaults(&remote).unwrap().keychain_enabled,
            Some(true)
        );
    }

    /// The remote side decides which file is read as key material
    /// if this gets through, so it must not get through.
    #[test]
    fn a_keyfile_path_from_the_remote_side_is_dropped() {
        let remote = remote_with_secrets(
            r#"
            [secrets]
            keyfile_path = "/tmp/attacker-chosen.key"
            "#,
        );

        // Present on the wire...
        assert!(remote.secrets.as_ref().unwrap().keyfile_path.is_some());

        // ...and absent from everything the caller can apply.
        let d = read_secrets_defaults(&remote).unwrap();
        assert!(
            !d.describe().iter().any(|l| l.contains("keyfile")),
            "{:?}",
            d.describe()
        );
    }

    /// Only this machine knows what is left in its own keychain,
    /// so only this machine gets to claim the migration is done.
    /// Accepting it remotely would switch off the read-only
    /// legacy fallback on a machine that still depends on it.
    #[test]
    fn migration_complete_from_the_remote_side_is_dropped() {
        let remote = remote_with_secrets(
            r#"
            [secrets]
            migration_complete = true
            "#,
        );

        assert!(remote.secrets.as_ref().unwrap().migration_complete);

        let d = read_secrets_defaults(&remote).unwrap();
        assert!(
            !d.describe().iter().any(|l| l.contains("migration")),
            "{:?}",
            d.describe()
        );
    }

    #[test]
    fn a_zero_unlock_window_is_refused_rather_than_corrected() {
        let remote = remote_with_secrets(
            r#"
            [secrets]
            unlock_ttl_seconds = 0
            "#,
        );

        let err = read_secrets_defaults(&remote).unwrap_err();
        assert!(err.contains("unlock_ttl_seconds"), "{err}");
    }

    #[test]
    fn a_zero_ceiling_is_refused() {
        let remote = remote_with_secrets(
            r#"
            [secrets]
            max_unlock_ttl_seconds = 0
            "#,
        );

        assert!(
            read_secrets_defaults(&remote)
                .unwrap_err()
                .contains("max_unlock_ttl_seconds")
        );
    }

    /// A window above its own ceiling is not a window — the
    /// ceiling wins and the operator's number never applies.
    /// Saying so beats applying something they did not ask for.
    #[test]
    fn a_window_above_its_own_ceiling_is_refused() {
        let remote = remote_with_secrets(
            r#"
            [secrets]
            unlock_ttl_seconds = 7200
            max_unlock_ttl_seconds = 3600
            "#,
        );

        let err = read_secrets_defaults(&remote).unwrap_err();
        assert!(err.contains("7200") && err.contains("3600"), "{err}");
    }

    /// What gets applied has to be printable, or the user cannot
    /// see a posture that was chosen for them.
    #[test]
    fn every_applied_field_is_described_back() {
        let d = RemoteSecretsDefaults {
            profile: Some(SecretsProfile::Strict),
            unlock_ttl_seconds: Some(900),
            max_unlock_ttl_seconds: Some(3600),
            idle_relock_seconds: Some(300),
            keychain_enabled: Some(false),
        };

        let lines = d.describe();
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert!(lines.iter().any(|l| l == "secrets.profile = strict"));
        assert!(
            lines
                .iter()
                .any(|l| l == "secrets.keychain.enabled = false")
        );
    }

    /// The runtime merge must stay out of this. If it ever
    /// carries the section, whoever serves the config can change
    /// the security posture of someone else's machine on every
    /// invocation, with nothing written down anywhere the user
    /// looks.
    #[test]
    fn the_runtime_merge_does_not_carry_the_secrets_section() {
        let local = Config::default();
        let remote = remote_with_secrets(
            r#"
            [secrets]
            profile = "strict"
            [secrets.keychain]
            enabled = true
            "#,
        );

        let merged = merge_configs(local, remote);

        assert!(
            merged.secrets.is_none(),
            "the runtime path picked up a secrets posture: {:?}",
            merged.secrets
        );
    }

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
