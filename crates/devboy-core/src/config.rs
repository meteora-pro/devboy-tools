//! Configuration management for devboy-tools.
//!
//! Handles loading and saving configuration from TOML files.
//! Config files are stored in platform-specific locations:
//!
//! - **macOS/Linux**: `~/.config/devboy-tools/config.toml`
//! - **Windows**: `%APPDATA%\devboy-tools\config.toml`
//!
//! # Example
//!
//! ```ignore
//! use devboy_core::config::{Config, GitHubConfig};
//!
//! // Load config
//! let config = Config::load()?;
//!
//! // Modify config
//! let mut config = config;
//! config.github = Some(GitHubConfig {
//!     owner: "meteora-pro".to_string(),
//!     repo: "devboy-tools".to_string(),
//! });
//!
//! // Save config
//! config.save()?;
//! ```

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use tracing::{debug, info};

const CONFIG_FILE_NAME: &str = "config.toml";

/// Config directory name.
const CONFIG_DIR_NAME: &str = "devboy-tools";

/// Environment variable that replaces the platform's config
/// directory outright.
///
/// The value is used as-is — `CONFIG_DIR_NAME` is *not* appended,
/// because a caller pointing this at a scratch directory wants that
/// directory, not a subdirectory of it.
///
/// Primarily for tests that spawn the real binary: see
/// [`Config::config_dir`] for why `HOME` and `XDG_CONFIG_HOME` are
/// not enough. Also usable for running two configurations side by
/// side, which is why it is documented rather than hidden.
pub const CONFIG_DIR_ENV: &str = "DEVBOY_CONFIG_DIR";

// =============================================================================
// Configuration structures
// =============================================================================

/// Main configuration structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gitlab: Option<GitLabConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clickup: Option<ClickUpConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jira: Option<JiraConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear: Option<LinearConfig>,
    pub yougile: Option<YouGileConfig>,

    /// Fireflies.ai configuration (meeting notes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fireflies: Option<FirefliesConfig>,

    /// Confluence self-hosted configuration (knowledge base)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confluence: Option<ConfluenceConfig>,

    /// Slack configuration (messenger)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<SlackConfig>,

    /// Telegram configuration (messenger)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telegram: Option<TelegramConfig>,

    /// Named contexts (profiles) configuration.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contexts: BTreeMap<String, ContextConfig>,

    /// Currently active context name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_context: Option<String>,

    /// Upstream MCP servers to proxy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proxy_mcp_servers: Vec<ProxyMcpServerConfig>,

    /// Built-in tools filtering configuration.
    #[serde(default, skip_serializing_if = "BuiltinToolsConfig::is_empty")]
    pub builtin_tools: BuiltinToolsConfig,

    /// Format pipeline configuration (TOON encoding, budget trimming, strategies).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_pipeline: Option<FormatPipelineConfig>,

    /// Transparent proxy configuration: routing strategy, secrets cache, telemetry.
    /// Applies across all upstream MCP servers unless overridden per-server.
    #[serde(default, skip_serializing_if = "ProxyConfig::is_default")]
    pub proxy: ProxyConfig,

    /// Sentry error reporting configuration (optional, disabled by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentry: Option<SentryConfig>,

    /// Remote configuration endpoint (optional).
    /// Fetches TOML config from a URL on startup and merges with local config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_config: Option<RemoteConfigSettings>,

    /// Secret-framework knobs (ADR-020 / ADR-021 / ADR-023 /
    /// ADR-024): migration state, the unlock-window profile, and
    /// the opt-in OS keychain switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<SecretsConfig>,

    /// Process-level switches that belong to no single provider
    /// or subsystem (ADR-024 §6). Currently carries the explicit
    /// CI / env-only flag that `detect_ci_mode` consults after
    /// `--ci` and `DEVBOY_CI`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeConfig>,
}

impl Config {
    /// `true` when the user has flipped
    /// `[secrets] migration_complete = true`. Defaults to `false`
    /// for any config that doesn't carry the section at all.
    pub fn is_secrets_migration_complete(&self) -> bool {
        self.secrets
            .as_ref()
            .map(|s| s.migration_complete)
            .unwrap_or(false)
    }

    /// Active unlock-window profile, defaulting to
    /// [`SecretsProfile::Convenient`] when unset (ADR-024 §2).
    pub fn secrets_profile(&self) -> SecretsProfile {
        self.secrets.as_ref().map(|s| s.profile).unwrap_or_default()
    }

    /// `true` only when the user explicitly opted the OS keychain
    /// back in (ADR-024 §6). Absent config means disabled, on
    /// every platform.
    pub fn is_keychain_enabled(&self) -> bool {
        self.secrets
            .as_ref()
            .map(|s| s.keychain.enabled)
            .unwrap_or(false)
    }

    /// Ceiling on any single unlock window, in seconds. Falls
    /// back to the active profile's default.
    pub fn max_unlock_ttl_seconds(&self) -> u64 {
        self.secrets
            .as_ref()
            .and_then(|s| s.max_unlock_ttl_seconds)
            .unwrap_or_else(|| self.secrets_profile().default_max_unlock_ttl_seconds())
    }

    /// Unlock window in seconds, falling back to the profile
    /// default and **clamped** to [`Self::max_unlock_ttl_seconds`].
    ///
    /// Clamping rather than erroring keeps a misconfigured file
    /// usable; [`Self::secrets_config_warnings`] surfaces the
    /// inconsistency to `doctor` instead of failing the process.
    pub fn unlock_ttl_seconds(&self) -> u64 {
        let configured = self
            .secrets
            .as_ref()
            .and_then(|s| s.unlock_ttl_seconds)
            .unwrap_or_else(|| self.secrets_profile().default_unlock_ttl_seconds());
        configured.min(self.max_unlock_ttl_seconds())
    }

    /// Idle re-lock in seconds, or `None` when idle re-locking is
    /// off. Falls back to the profile default.
    pub fn idle_relock_seconds(&self) -> Option<u64> {
        match self.secrets.as_ref() {
            Some(s) if s.idle_relock_seconds.is_some() => s.idle_relock_seconds,
            _ => self.secrets_profile().default_idle_relock_seconds(),
        }
    }

    /// Configured keyfile path for `Envelope::Keyfile`, if any.
    pub fn secrets_keyfile_path(&self) -> Option<&std::path::Path> {
        self.secrets
            .as_ref()
            .and_then(|s| s.keyfile_path.as_deref())
    }

    /// `true` when `[runtime] ci = true`. This is the
    /// lowest-priority explicit CI signal; `--ci` and `DEVBOY_CI`
    /// take precedence, and heuristic variables never reach here.
    pub fn is_ci_forced(&self) -> bool {
        self.runtime.as_ref().map(|r| r.ci).unwrap_or(false)
    }

    /// Human-readable inconsistencies in the `[secrets]` section.
    ///
    /// These are reported rather than enforced, because a config
    /// that is merely odd should not stop the process — but a
    /// silently clamped window is exactly the kind of thing a
    /// user should be told about rather than discover later.
    pub fn secrets_config_warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        let Some(secrets) = self.secrets.as_ref() else {
            return out;
        };

        let max = self.max_unlock_ttl_seconds();

        if let Some(ttl) = secrets.unlock_ttl_seconds
            && ttl > max
        {
            out.push(format!(
                "[secrets] unlock_ttl_seconds = {ttl} exceeds max_unlock_ttl_seconds = {max}; \
                 the effective window is clamped to {max}s"
            ));
        }

        if let Some(idle) = secrets.idle_relock_seconds {
            let effective = self.unlock_ttl_seconds();
            if idle > effective {
                out.push(format!(
                    "[secrets] idle_relock_seconds = {idle} is longer than the unlock window \
                     ({effective}s), so idle re-locking can never fire"
                ));
            }
        }

        if secrets.unlock_ttl_seconds == Some(0) {
            out.push(
                "[secrets] unlock_ttl_seconds = 0 re-locks the vault immediately after every \
                 unlock; use `devboy secrets lock` for an explicit lock instead"
                    .to_string(),
            );
        }

        out
    }
}

/// `[runtime]` section (ADR-024 §6).
///
/// Holds process-level switches that belong to no single
/// provider. `ci` is the lowest-priority of the three explicit
/// CI signals — `--ci` beats `DEVBOY_CI` beats this — and is the
/// one that lives with the project rather than with the
/// invocation.
///
/// Heuristic variables (`CI`, `GITLAB_CI`, …) deliberately do
/// **not** feed this field: they raise a doctor notice instead,
/// because a security posture must not change because an
/// unrelated tool exported `CI=1`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Force CI / env-only mode: the environment becomes the sole
    /// secret source, and no vault, daemon, keychain or prompt is
    /// involved.
    #[serde(default)]
    pub ci: bool,
}

/// Unlock-window profile (ADR-024 §2).
///
/// A wide unlock window and per-call approval pull in opposite
/// directions, so the coherent combinations ship as named
/// profiles rather than four independent knobs the user has to
/// keep consistent.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SecretsProfile {
    /// A developer laptop running a trusted agent: unlock once
    /// for the working day, honour each path's own
    /// `approve_on_use`.
    #[default]
    Convenient,
    /// Shared hosts, high-value paths, untrusted or unattended
    /// agents: short window, idle re-lock, and every access a
    /// separate human decision.
    Strict,
}

impl SecretsProfile {
    /// Default `unlock_ttl` for this profile, in seconds.
    pub fn default_unlock_ttl_seconds(self) -> u64 {
        match self {
            Self::Convenient => 8 * 60 * 60,
            Self::Strict => 15 * 60,
        }
    }

    /// Default `max_unlock_ttl` ceiling for this profile, in
    /// seconds. A per-unlock `duration` may not exceed it.
    pub fn default_max_unlock_ttl_seconds(self) -> u64 {
        match self {
            Self::Convenient => 24 * 60 * 60,
            Self::Strict => 60 * 60,
        }
    }

    /// Default idle re-lock for this profile, in seconds.
    /// `None` under `convenient`, which is what preserves the
    /// daily-unlock intent.
    pub fn default_idle_relock_seconds(self) -> Option<u64> {
        match self {
            Self::Convenient => None,
            Self::Strict => Some(5 * 60),
        }
    }

    /// Whether this profile forces every path to `per-call`
    /// approval regardless of its manifest setting. This is the
    /// part of `strict` that is not merely "smaller numbers" —
    /// it is the only mitigation for an agent waiting out a
    /// legitimate unlock.
    pub fn forces_per_call_approval(self) -> bool {
        matches!(self, Self::Strict)
    }

    /// Whether this profile needs a surface on which the daemon
    /// can ask the user something. `strict` does, because
    /// per-call approval is meaningless with nobody to ask, and
    /// selecting it on a headless host must fail at configuration
    /// time rather than at the first secret access.
    pub fn requires_prompt_surface(self) -> bool {
        matches!(self, Self::Strict)
    }
}

/// `[secrets.keychain]` section (ADR-024 §6).
///
/// The OS keychain is **disabled by default on every platform**,
/// including macOS. It only exceeds the protection of a `0600`
/// file on macOS, where item ACLs bind to the reading process's
/// code signature; on Linux the Secret Service hands a stored
/// secret to any process in the user's session, and on Windows
/// DPAPI is scoped to the user.
///
/// Enabling it is therefore a deliberate choice — most usefully
/// on macOS as an anti-tamper binding (ADR-024 §7) rather than as
/// a general secret store.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeychainConfig {
    /// Whether the in-tree `keychain` source may be used at all.
    #[serde(default)]
    pub enabled: bool,
}

impl KeychainConfig {
    /// `true` when this section carries nothing worth writing.
    pub fn is_default(&self) -> bool {
        !self.enabled
    }
}

/// `[secrets]` section per ADR-020 §7 (migration story),
/// ADR-021 §6 (validation framework) and ADR-024 §2/§6
/// (unlock window, keychain demotion).
///
/// Secret-framework knobs live here rather than in [`Config`]
/// directly so they travel together.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretsConfig {
    /// `true` when the user has confirmed every legacy
    /// pre-ADR-020 keychain entry has been migrated. Once set,
    /// the doctor escalates any remaining legacy entries from
    /// "migrate these" to "migration_complete is set but legacy
    /// entries remain — clear the flag or finish the move."
    ///
    /// Independent of [`KeychainConfig::enabled`]: the legacy
    /// reader stays available until migration is complete, so the
    /// ADR-024 default flip cannot strand a user whose tokens
    /// still live in the OS keychain.
    #[serde(default)]
    pub migration_complete: bool,

    /// Unlock-window profile. Supplies the defaults for the three
    /// TTL fields below; each may still be overridden
    /// individually.
    #[serde(default)]
    pub profile: SecretsProfile,

    /// How long the daemon holds the vault key after a successful
    /// unlock. `None` takes the profile's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlock_ttl_seconds: Option<u64>,

    /// Hard ceiling on any single unlock window, including a
    /// per-unlock `duration`. `None` takes the profile's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_unlock_ttl_seconds: Option<u64>,

    /// Re-lock after this much *inactivity* even inside the
    /// unlock window. `None` takes the profile's default, which
    /// is off under `convenient`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_relock_seconds: Option<u64>,

    /// Opt-in OS keychain source.
    #[serde(default, skip_serializing_if = "KeychainConfig::is_default")]
    pub keychain: KeychainConfig,

    /// Path to the keyfile whose HKDF output wraps the vault key
    /// (ADR-024 §6, `Envelope::Keyfile`), enabling an unattended
    /// cold start.
    ///
    /// Deliberately defaults **outside** the vault's own
    /// directory so that a backup, a cloud sync, or an accidental
    /// `git add` of the config tree does not carry both halves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyfile_path: Option<PathBuf>,
}

/// Configuration for an upstream MCP server to proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyMcpServerConfig {
    /// Server name (used as tool prefix if tool_prefix not set)
    pub name: String,
    /// Server URL (SSE or Streamable HTTP endpoint)
    pub url: String,
    /// Auth type: "bearer", "api_key", "none", "oauth2"
    #[serde(default = "default_auth_none")]
    pub auth_type: String,
    /// Keychain key for auth token
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_key: Option<String>,
    /// Tool name prefix override (default: name)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_prefix: Option<String>,
    /// Transport type: "sse" (default) or "streamable-http"
    #[serde(default = "default_transport_sse")]
    pub transport: String,
    /// Per-server routing override. Only the fields explicitly set here win over the
    /// global `[proxy.routing]`; omitted fields inherit from the global config (so a
    /// per-server block that just sets `strategy` does **not** silently reset
    /// `fallback_on_error` to its default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<ProxyRoutingOverride>,
    /// OAuth 2.1 settings (used when `auth_type = "oauth2"`). Optional — a minimal
    /// config sets only `auth_type = "oauth2"` and lets discovery (RFC 9728/8414)
    /// plus dynamic registration (RFC 7591) fill the rest on first `devboy login`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<ProxyOAuthConfig>,
}

/// OAuth 2.1 client settings for a proxy upstream (`auth_type = "oauth2"`).
///
/// Every field is optional so a minimal config just sets `auth_type = "oauth2"`;
/// the missing pieces are resolved at `devboy login` time:
/// - `authorization_server` — discovered from the upstream's RFC 9728
///   `WWW-Authenticate: Bearer resource_metadata="…"` challenge, then its
///   RFC 8414 authorization-server metadata;
/// - `client_id` — obtained via RFC 7591 dynamic client registration and
///   persisted back;
/// - `scopes` — default to the server's advertised scopes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyOAuthConfig {
    /// Registered OAuth `client_id`. Obtained via dynamic registration if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Requested scopes. Falls back to the server's advertised scopes if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    /// Authorization Server base URL. Discovered from the upstream's RFC 9728
    /// `WWW-Authenticate` challenge if unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_server: Option<String>,
    /// Token endpoint, cached from discovery at `devboy login` time so the proxy
    /// refreshes without re-running discovery on every startup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_endpoint: Option<String>,
}

fn default_transport_sse() -> String {
    "sse".to_string()
}

fn default_auth_none() -> String {
    "none".to_string()
}

fn default_linear_url() -> String {
    "https://api.linear.app/graphql".to_string()
}

/// Per-context provider configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gitlab: Option<GitLabConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clickup: Option<ClickUpConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jira: Option<JiraConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear: Option<LinearConfig>,
    pub yougile: Option<YouGileConfig>,

    /// Fireflies.ai configuration (meeting notes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fireflies: Option<FirefliesConfig>,

    /// Confluence self-hosted configuration (knowledge base)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confluence: Option<ConfluenceConfig>,

    /// Slack configuration (messenger)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<SlackConfig>,

    /// Telegram configuration (messenger)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telegram: Option<TelegramConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubConfig {
    /// Repository owner (user or organization)
    pub owner: String,
    pub repo: String,
    /// GitHub API base URL (for GitHub Enterprise)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabConfig {
    /// GitLab instance URL
    #[serde(default = "default_gitlab_url")]
    pub url: String,
    /// Project ID (numeric or path)
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpConfig {
    pub list_id: String,
    /// ClickUp team (workspace) ID — required for custom task ID resolution
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraConfig {
    /// Jira instance URL
    pub url: String,
    /// Project key (e.g., "PROJ")
    pub project_key: String,
    /// User email (required for Jira auth)
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearConfig {
    /// Linear GraphQL endpoint.
    #[serde(default = "default_linear_url")]
    pub url: String,
    /// Default Linear team UUID used for issue operations.
    pub team_id: String,
    /// Optional human-readable team key (e.g. `ENG`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_key: Option<String>,
}

/// YouGile provider configuration (issue tracker).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YouGileConfig {
    /// YouGile API base URL.
    #[serde(default = "default_yougile_url")]
    pub url: String,
    /// Default board ID used as the provider scope.
    pub board_id: String,
}

/// Fireflies.ai provider configuration (meeting notes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirefliesConfig {
    // API key is stored in OS keychain (key: "fireflies.token")
    // No fields needed — config just enables the provider
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfluenceConfig {
    /// Confluence base URL, e.g. `https://wiki.example.com`.
    pub base_url: String,
    /// Deployment flavor. Defaults to self-hosted when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavor: Option<ConfluenceFlavor>,
    /// Atlassian Cloud site id used by `api.atlassian.com` routes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_id: Option<String>,
    /// Preferred REST API generation when the instance supports multiple.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
    /// Username/email for basic auth when that auth mode is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// OAuth app client ID for Atlassian Cloud 3LO.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// OAuth redirect URI registered in the Atlassian app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
    /// Optional default space hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfluenceFlavor {
    SelfHosted,
    Cloud,
}

/// Slack provider configuration (messenger).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    /// Optional Slack workspace/team ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// Optional human-readable workspace name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Slack API base URL override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// OAuth app client ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
    /// Required bot scopes expected by devboy Slack integration.
    #[serde(
        default = "default_slack_required_scopes",
        skip_serializing_if = "is_default_slack_required_scopes"
    )]
    pub required_scopes: Vec<String>,
}

impl Default for SlackConfig {
    fn default() -> Self {
        Self {
            team_id: None,
            workspace: None,
            base_url: None,
            client_id: None,
            redirect_uri: None,
            required_scopes: default_slack_required_scopes(),
        }
    }
}

/// Telegram provider configuration (messenger).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// Optional Telegram API base URL override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Optional bot username for diagnostics and UX.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_username: Option<String>,
}

fn default_yougile_url() -> String {
    "https://yougile.com/api-v2".to_string()
}

pub fn default_slack_required_scopes() -> Vec<String> {
    vec![
        "channels:read".to_string(),
        "channels:history".to_string(),
        "groups:read".to_string(),
        "groups:history".to_string(),
        "im:read".to_string(),
        "im:history".to_string(),
        "mpim:read".to_string(),
        "mpim:history".to_string(),
        "chat:write".to_string(),
        "users:read".to_string(),
    ]
}

fn parse_confluence_flavor(value: &str) -> Result<ConfluenceFlavor> {
    match value.trim().to_ascii_lowercase().as_str() {
        "self_hosted" | "self-hosted" | "selfhosted" | "server" | "dc" | "data_center"
        | "data-center" => Ok(ConfluenceFlavor::SelfHosted),
        "cloud" => Ok(ConfluenceFlavor::Cloud),
        other => Err(Error::Config(format!(
            "Unknown Confluence config field value for flavor: {}",
            other
        ))),
    }
}

fn confluence_flavor_slug(flavor: ConfluenceFlavor) -> String {
    match flavor {
        ConfluenceFlavor::SelfHosted => "self_hosted".to_string(),
        ConfluenceFlavor::Cloud => "cloud".to_string(),
    }
}

fn is_default_slack_required_scopes(scopes: &[String]) -> bool {
    scopes == default_slack_required_scopes().as_slice()
}

/// Configuration for controlling which built-in tools are available.
///
/// Supports two mutually exclusive modes:
/// - `disabled`: blacklist specific tools (all others remain enabled)
/// - `enabled`: whitelist specific tools (all others are disabled)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuiltinToolsConfig {
    /// List of tool names to disable (blacklist mode).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled: Vec<String>,

    /// List of tool names to enable (whitelist mode). All others are disabled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled: Vec<String>,
}

impl BuiltinToolsConfig {
    /// Check whether the config is empty (no filtering).
    pub fn is_empty(&self) -> bool {
        self.disabled.is_empty() && self.enabled.is_empty()
    }

    /// Validate the config: `disabled` and `enabled` must not both be set.
    pub fn validate(&self) -> Result<()> {
        if !self.disabled.is_empty() && !self.enabled.is_empty() {
            return Err(Error::Config(
                "builtin_tools: 'disabled' and 'enabled' are mutually exclusive, use only one"
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// Check whether a tool with the given name should be available.
    pub fn is_tool_allowed(&self, name: &str) -> bool {
        if !self.enabled.is_empty() {
            return self.enabled.iter().any(|n| n == name);
        }
        if !self.disabled.is_empty() {
            return !self.disabled.iter().any(|n| n == name);
        }
        true
    }

    /// Log warnings for tool names that are not in the known set.
    pub fn warn_unknown_tools(&self, known: &[&str]) {
        for name in self.disabled.iter().chain(self.enabled.iter()) {
            if !known.iter().any(|k| k == name) {
                tracing::warn!(
                    "builtin_tools: unknown tool name '{}', it will have no effect",
                    name
                );
            }
        }
    }
}

// ============================================================================
// Format Pipeline Config
// ============================================================================

/// Configuration for the format pipeline (TOON encoding, budget trimming, strategies).
///
/// All fields have sensible defaults — the pipeline works out of the box without config.
///
/// # Example TOML
///
/// ```toml
/// [format_pipeline]
/// budget_tokens = 8000
/// margin = 0.20
/// max_iterations = 3
/// default_format = "toon"
///
/// [format_pipeline.strategies]
/// get_issues = "element_count"
/// "cloud__get_tasks" = "element_count"
///
/// [format_pipeline.proxy_matching]
/// enabled = true
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatPipelineConfig {
    /// Maximum token budget per tool response (default: 8000).
    /// ~6% of a 128K context window.
    #[serde(default = "default_budget_tokens")]
    pub budget_tokens: usize,

    /// Safety margin for token estimation inaccuracy (default: 0.20).
    /// Covers up to 25% deviation in compression ratio after trimming.
    #[serde(default = "default_margin")]
    pub margin: f64,

    /// Maximum trim-encode-verify iterations (default: 3).
    /// 2 is sufficient in 99% of cases; 3 is a safety net.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,

    /// Default output format: "toon" or "json" (default: "toon").
    #[serde(default = "default_format_toon")]
    pub default_format: String,

    /// Strategy overrides by tool name.
    /// Keys are tool names (including proxy-prefixed), values are strategy names.
    /// Available strategies: element_count, cascading, size_proportional,
    /// thread_level, head_tail, default.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub strategies: HashMap<String, String>,

    #[serde(default)]
    pub proxy_matching: ProxyMatchingConfig,
}

impl Default for FormatPipelineConfig {
    fn default() -> Self {
        Self {
            budget_tokens: default_budget_tokens(),
            margin: default_margin(),
            max_iterations: default_max_iterations(),
            default_format: default_format_toon(),
            strategies: HashMap::new(),
            proxy_matching: ProxyMatchingConfig::default(),
        }
    }
}

fn default_budget_tokens() -> usize {
    8000
}

fn default_margin() -> f64 {
    0.20
}

fn default_max_iterations() -> usize {
    3
}

fn default_format_toon() -> String {
    "toon".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyMatchingConfig {
    /// When true, strip proxy prefix (e.g. `cloud__get_issues` → `get_issues`)
    /// and look up hardcoded defaults (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for ProxyMatchingConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Sentry error reporting configuration.
///
/// By default Sentry is disabled. Setting `dsn` (or the `DEVBOY_SENTRY_DSN` env var)
/// is sufficient to enable error reporting.
///
/// # Example
///
/// ```toml
/// [sentry]
/// dsn = "https://examplePublicKey@o0.ingest.sentry.io/0"
/// environment = "production"
/// sample_rate = 1.0
/// traces_sample_rate = 0.0
/// ```
///
/// `Debug` is implemented manually so the `dsn` (which contains an auth token
/// in its userinfo segment) does not leak through `tracing::debug!` /
/// `dbg!()` /  panic backtraces. Serialization preserves the value because
/// the DSN must round-trip back to the on-disk TOML config.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SentryConfig {
    /// Sentry DSN endpoint. When empty, Sentry is disabled (no-op).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsn: Option<String>,

    /// Environment tag (e.g., "production", "staging", "development").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,

    /// Error sample rate (0.0 - 1.0). Default: 1.0 (send all errors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<f32>,

    /// Performance tracing sample rate (0.0 - 1.0). Default: 0.0 (disabled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traces_sample_rate: Option<f32>,
}

impl std::fmt::Debug for SentryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SentryConfig")
            .field("dsn", &self.dsn.as_ref().map(|_| "<redacted>"))
            .field("environment", &self.environment)
            .field("sample_rate", &self.sample_rate)
            .field("traces_sample_rate", &self.traces_sample_rate)
            .finish()
    }
}

/// Remote configuration endpoint settings.
///
/// Fetches TOML configuration from a remote URL on startup and merges it
/// with the local config. Remote values override local values.
///
/// # Example
///
/// ```toml
/// [remote_config]
/// url = "https://example.com/api/devboy-config"
/// token_key = "remote_config.token"
/// ```
///
/// Or via environment variables:
/// - `DEVBOY_REMOTE_CONFIG_URL` — Remote config URL
/// - `DEVBOY_REMOTE_CONFIG_TOKEN` — Bearer token for authentication
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoteConfigSettings {
    /// URL to fetch remote TOML config from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Keychain key for the Bearer token (e.g., "remote_config.token").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_key: Option<String>,

    /// Where to trade a short-lived onboarding token for a durable
    /// one, when the config server offers that.
    ///
    /// Read from the server's *response*, not written by the user:
    /// declaring it is how a server opts into
    /// [`crate::token_exchange`]. A server that declares nothing
    /// keeps the old behaviour, where the token supplied on the
    /// command line is stored as-is.
    ///
    /// Only honoured when it shares an origin with the config URL —
    /// acting on it means posting a live credential to wherever it
    /// points, and that decision belongs to the person who chose the
    /// config server, not to the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_exchange_url: Option<String>,
}

fn default_gitlab_url() -> String {
    "https://gitlab.com".to_string()
}

// =============================================================================
// Transparent Proxy Config (routing, secrets, telemetry)
// =============================================================================

/// Routing strategy — how a tool invocation is dispatched when both the local executor
/// and a connected upstream MCP server can handle the same tool.
///
/// Cloud has priority by design: the default strategy is `Remote`, so behavior is unchanged
/// for existing deployments unless the user explicitly opts in to local routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingStrategy {
    /// Route every matched call to the upstream server. Local executor stays idle for
    /// matched tools (still used for local-only tools that have no upstream counterpart).
    #[default]
    Remote,
    /// Route matched calls to the local executor. If a tool has no local implementation,
    /// fall through to upstream.
    Local,
    /// Try the local executor first; on error, fall back to upstream (requires
    /// `fallback_on_error`).
    #[serde(rename = "local-first")]
    LocalFirst,
    /// Try upstream first; on error, fall back to the local executor (requires
    /// `fallback_on_error`).
    #[serde(rename = "remote-first")]
    RemoteFirst,
}

impl RoutingStrategy {
    /// Parse a string token, tolerating both kebab-case and snake_case.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "remote" => Some(Self::Remote),
            "local" => Some(Self::Local),
            "local-first" | "local_first" | "localfirst" => Some(Self::LocalFirst),
            "remote-first" | "remote_first" | "remotefirst" => Some(Self::RemoteFirst),
            _ => None,
        }
    }
}

/// Per-tool override: maps a tool-name glob pattern to a specific routing strategy.
/// Patterns are matched against the tool name *without* the upstream prefix
/// (e.g., `get_issues`, not `cloud__get_issues`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyToolRule {
    /// Glob-like pattern: `*` matches any sequence (including empty).
    /// Examples: `get_*`, `*_issue`, `gitlab.*`, `create_*`.
    pub pattern: String,
    /// Strategy to apply for tools whose name matches this pattern.
    pub strategy: RoutingStrategy,
}

/// Routing policy: global default strategy plus per-tool overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyRoutingConfig {
    /// Default strategy applied to tools without a matching override.
    #[serde(default)]
    pub strategy: RoutingStrategy,
    /// For `LocalFirst` / `RemoteFirst`: when the primary executor errors, retry with
    /// the other executor. No-op for `Remote` / `Local` strategies.
    #[serde(default = "default_true")]
    pub fallback_on_error: bool,
    /// First-match-wins list of per-tool overrides.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_overrides: Vec<ProxyToolRule>,
}

impl Default for ProxyRoutingConfig {
    fn default() -> Self {
        Self {
            strategy: RoutingStrategy::default(),
            fallback_on_error: true,
            tool_overrides: Vec::new(),
        }
    }
}

impl ProxyRoutingConfig {
    /// Resolve the effective strategy for a tool name (without upstream prefix).
    /// First-match wins across `tool_overrides`; falls back to the global `strategy`.
    pub fn strategy_for(&self, tool_name: &str) -> RoutingStrategy {
        for rule in &self.tool_overrides {
            if matches_glob(&rule.pattern, tool_name) {
                return rule.strategy;
            }
        }
        self.strategy
    }

    /// Merge a per-server override on top of this global config.
    ///
    /// Only `Some` fields of the override win over the global config — omitted fields
    /// are inherited. `tool_overrides` from the override are prepended so they match
    /// before global rules; `None` there means "use the global list as-is".
    pub fn merged_with(&self, override_cfg: Option<&ProxyRoutingOverride>) -> ProxyRoutingConfig {
        let Some(o) = override_cfg else {
            return self.clone();
        };
        let mut merged = self.clone();
        if let Some(strategy) = o.strategy {
            merged.strategy = strategy;
        }
        if let Some(fallback_on_error) = o.fallback_on_error {
            merged.fallback_on_error = fallback_on_error;
        }
        if let Some(extra) = &o.tool_overrides
            && !extra.is_empty()
        {
            let mut combined = extra.clone();
            combined.extend(self.tool_overrides.iter().cloned());
            merged.tool_overrides = combined;
        }
        merged
    }

    /// True iff this config equals the default — used for `skip_serializing_if`.
    pub fn is_default(&self) -> bool {
        self.strategy == RoutingStrategy::default()
            && self.fallback_on_error
            && self.tool_overrides.is_empty()
    }
}

/// Per-server partial override for [`ProxyRoutingConfig`].
///
/// Every field is `Option` so that an override block touches only what it explicitly
/// sets — omitted fields inherit from the global `[proxy.routing]`. This matches the
/// "override what you want, keep what you don't" intuition a reviewer would expect
/// from the merge semantics described in the docs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyRoutingOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<RoutingStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_on_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_overrides: Option<Vec<ProxyToolRule>>,
}

/// Secure-store configuration for proxy authentication tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxySecretsConfig {
    /// TTL (seconds) for the in-memory cache on top of the OS keychain.
    /// `0` disables caching and forces a keychain lookup on every call
    /// (safer, but slower and may trigger repeated UI prompts on macOS).
    /// Default: 300 (5 minutes).
    #[serde(default = "default_secrets_cache_ttl")]
    pub cache_ttl_secs: u64,
}

impl Default for ProxySecretsConfig {
    fn default() -> Self {
        Self {
            cache_ttl_secs: default_secrets_cache_ttl(),
        }
    }
}

impl ProxySecretsConfig {
    pub fn is_default(&self) -> bool {
        self.cache_ttl_secs == default_secrets_cache_ttl()
    }
}

fn default_secrets_cache_ttl() -> u64 {
    300
}

/// Telemetry pipeline configuration — reports routing decisions to a configurable
/// HTTP endpoint even when the call is executed locally.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyTelemetryConfig {
    /// When false, no telemetry events are collected or uploaded.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Flush when this many events accumulate in the buffer.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// Flush at least once per interval even if the buffer is smaller than `batch_size`.
    #[serde(default = "default_batch_interval_secs")]
    pub batch_interval_secs: u64,
    /// Upload endpoint URL. If unset, events are collected but never uploaded (dry-run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Keychain key for the telemetry auth token. Falls back to the first upstream
    /// server's `token_key` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_key: Option<String>,
    /// Maximum events held in the offline queue (when upload is unavailable). Oldest
    /// events are dropped when the queue is full.
    #[serde(default = "default_offline_queue_max")]
    pub offline_queue_max: usize,
}

impl Default for ProxyTelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            batch_size: default_batch_size(),
            batch_interval_secs: default_batch_interval_secs(),
            endpoint: None,
            token_key: None,
            offline_queue_max: default_offline_queue_max(),
        }
    }
}

impl ProxyTelemetryConfig {
    pub fn is_default(&self) -> bool {
        self.enabled
            && self.batch_size == default_batch_size()
            && self.batch_interval_secs == default_batch_interval_secs()
            && self.endpoint.is_none()
            && self.token_key.is_none()
            && self.offline_queue_max == default_offline_queue_max()
    }
}

fn default_batch_size() -> usize {
    100
}

fn default_batch_interval_secs() -> u64 {
    30
}

fn default_offline_queue_max() -> usize {
    10_000
}

/// Container for global proxy configuration — wired under `[proxy]` in TOML.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    #[serde(default, skip_serializing_if = "ProxyRoutingConfig::is_default")]
    pub routing: ProxyRoutingConfig,

    #[serde(default, skip_serializing_if = "ProxySecretsConfig::is_default")]
    pub secrets: ProxySecretsConfig,

    #[serde(default, skip_serializing_if = "ProxyTelemetryConfig::is_default")]
    pub telemetry: ProxyTelemetryConfig,
}

impl ProxyConfig {
    pub fn is_default(&self) -> bool {
        self.routing.is_default() && self.secrets.is_default() && self.telemetry.is_default()
    }
}

/// Match `name` against a glob-like `pattern` where `*` is a wildcard matching any
/// run of characters (including empty). No character classes, escapes, or `?`.
///
/// Examples:
/// - `get_*` matches `get_issues`, `get_merge_requests`
/// - `*_issue` matches `create_issue`, `update_issue`
/// - `*` matches everything
/// - `exact` matches only `exact`
pub fn matches_glob(pattern: &str, name: &str) -> bool {
    // Trivial cases
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == name;
    }

    let segments: Vec<&str> = pattern.split('*').collect();
    let mut cursor = 0usize;
    let last_idx = segments.len() - 1;

    // First segment must be a prefix unless empty (leading *).
    if !segments[0].is_empty() {
        if !name.starts_with(segments[0]) {
            return false;
        }
        cursor = segments[0].len();
    }

    // Middle segments must appear in order, each consuming a position in `name`.
    for seg in &segments[1..last_idx] {
        if seg.is_empty() {
            continue; // "**" collapses
        }
        match name[cursor..].find(seg) {
            Some(pos) => cursor += pos + seg.len(),
            None => return false,
        }
    }

    // Last segment must be a suffix unless empty (trailing *).
    let last = segments[last_idx];
    if last.is_empty() {
        return true;
    }
    if cursor > name.len() {
        return false;
    }
    name[cursor..].ends_with(last)
}

// =============================================================================
// Config implementation
// =============================================================================

impl Config {
    /// Name of the implicit context for legacy top-level provider configuration.
    pub const DEFAULT_CONTEXT_NAME: &'static str = "default";

    /// Get the configuration directory path.
    ///
    /// [`CONFIG_DIR_ENV`] overrides the platform default, and exists
    /// for one reason: without it, nothing that runs the real binary
    /// can be isolated from the developer's own configuration on
    /// Windows.
    ///
    /// `dirs::config_dir()` reads `XDG_CONFIG_HOME` on Linux and
    /// `$HOME/Library/Application Support` on macOS, both of which a
    /// test can redirect. On Windows it goes through the Known Folder
    /// API, which no environment variable reaches — so a test that
    /// spawns `devboy` there writes to the config of whoever ran
    /// `cargo test`. That is not a hypothetical: it is why the
    /// keyfile-enrolment tests had to be gated to UNIX.
    pub fn config_dir() -> Result<PathBuf> {
        if let Ok(overridden) = std::env::var(CONFIG_DIR_ENV) {
            let trimmed = overridden.trim();
            if !trimmed.is_empty() {
                return Ok(PathBuf::from(trimmed));
            }
        }

        dirs::config_dir()
            .map(|p| p.join(CONFIG_DIR_NAME))
            .ok_or_else(|| Error::Config("Could not determine config directory".to_string()))
    }

    /// Get the configuration file path.
    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join(CONFIG_FILE_NAME))
    }

    /// Load configuration from the default location.
    ///
    /// Returns a default (empty) config if the file doesn't exist.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        Self::load_from(&path)
    }

    /// Load configuration from a specific path.
    ///
    /// Returns a default (empty) config if the file doesn't exist.
    pub fn load_from(path: &PathBuf) -> Result<Self> {
        if !path.exists() {
            debug!(path = ?path, "Config file does not exist, using defaults");
            return Ok(Self::default());
        }

        debug!(path = ?path, "Loading config");

        let contents = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("Failed to read config file: {}", e)))?;

        let mut config: Config = toml::from_str(&contents)
            .map_err(|e| Error::Config(format!("Failed to parse config file: {}", e)))?;

        // First collapse cosmetic empties (e.g. `endpoint = ""`) so they behave like the
        // CLI's "empty value clears the field" semantics, then validate semantics that
        // serde cannot enforce on its own (URL shape, etc.). `Config::set` already
        // applies these at write time — we re-run them on load so hand-edited TOML
        // cannot sneak invalid values past the API surface.
        config.sanitize();
        config.validate()?;

        info!(path = ?path, "Config loaded successfully");
        Ok(config)
    }

    /// Normalize cosmetic "null-equivalents" that TOML/serde can't express on their
    /// own — currently just: `proxy.telemetry.endpoint = ""` collapses to `None`, so
    /// hand-edited TOML matches the CLI semantics (where an empty value clears the
    /// field rather than leaving an invalid URL in place). Called by [`Self::load_from`]
    /// immediately before [`Self::validate`].
    pub fn sanitize(&mut self) {
        if let Some(endpoint) = self.proxy.telemetry.endpoint.as_deref()
            && endpoint.is_empty()
        {
            self.proxy.telemetry.endpoint = None;
        }
    }

    /// Run post-deserialization validation on the config.
    ///
    /// Covers invariants that TOML/serde deserializers can't express by themselves:
    /// URL shape for telemetry endpoint, bool coercions, etc. Safe to call at any time.
    /// Note: an empty-string endpoint is rejected here — callers that want "empty
    /// means clear" semantics should run [`Self::sanitize`] first (which `load_from`
    /// does automatically).
    pub fn validate(&self) -> Result<()> {
        if let Some(endpoint) = self.proxy.telemetry.endpoint.as_deref() {
            validate_http_url(endpoint, "proxy.telemetry.endpoint")?;
        }
        Ok(())
    }

    /// Save configuration to the default location.
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        self.save_to(&path)
    }

    /// Save configuration to a specific path.
    pub fn save_to(&self, path: &PathBuf) -> Result<()> {
        // Ensure directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Config(format!("Failed to create config directory: {}", e)))?;
        }

        debug!(path = ?path, "Saving config");

        let contents = toml::to_string_pretty(self)
            .map_err(|e| Error::Config(format!("Failed to serialize config: {}", e)))?;

        std::fs::write(path, contents)
            .map_err(|e| Error::Config(format!("Failed to write config file: {}", e)))?;

        info!(path = ?path, "Config saved successfully");
        Ok(())
    }

    /// Check if any provider is configured.
    pub fn has_any_provider(&self) -> bool {
        self.github.is_some()
            || self.gitlab.is_some()
            || self.clickup.is_some()
            || self.jira.is_some()
            || self.linear.is_some()
            || self.yougile.is_some()
            || self.fireflies.is_some()
            || self.confluence.is_some()
            || self.slack.is_some()
            || self.telegram.is_some()
            || self.contexts.values().any(ContextConfig::has_any_provider)
    }

    /// Get a list of configured provider names.
    pub fn configured_providers(&self) -> Vec<&'static str> {
        let mut providers = Vec::new();
        if self.github.is_some() {
            providers.push("github");
        }
        if self.gitlab.is_some() {
            providers.push("gitlab");
        }
        if self.clickup.is_some() {
            providers.push("clickup");
        }
        if self.jira.is_some() {
            providers.push("jira");
        }
        if self.linear.is_some() {
            providers.push("linear");
        }
        if self.yougile.is_some() {
            providers.push("yougile");
        }
        if self.confluence.is_some() {
            providers.push("confluence");
        }
        if self.slack.is_some() {
            providers.push("slack");
        }
        if self.telegram.is_some() {
            providers.push("telegram");
        }
        providers
    }

    /// Get all context names, including implicit legacy `default` context when applicable.
    pub fn context_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.contexts.keys().cloned().collect();
        if self.legacy_default_context().is_some()
            && !names.iter().any(|n| n == Self::DEFAULT_CONTEXT_NAME)
        {
            names.push(Self::DEFAULT_CONTEXT_NAME.to_string());
        }
        names.sort();
        names
    }

    /// Get context config by name, including implicit legacy `default` context.
    pub fn get_context(&self, name: &str) -> Option<ContextConfig> {
        if name == Self::DEFAULT_CONTEXT_NAME {
            return self
                .contexts
                .get(name)
                .cloned()
                .or_else(|| self.legacy_default_context());
        }

        self.contexts.get(name).cloned()
    }

    /// Resolve the currently active context name.
    pub fn resolve_active_context_name(&self) -> Option<String> {
        if let Some(active) = &self.active_context
            && self.get_context(active).is_some()
        {
            return Some(active.clone());
        }

        if self.get_context(Self::DEFAULT_CONTEXT_NAME).is_some() {
            return Some(Self::DEFAULT_CONTEXT_NAME.to_string());
        }

        self.context_names().into_iter().next()
    }

    /// Set active context if it exists.
    pub fn set_active_context(&mut self, name: &str) -> Result<()> {
        if self.get_context(name).is_none() {
            return Err(Error::Config(format!("Unknown context: {}", name)));
        }
        self.active_context = Some(name.to_string());
        Ok(())
    }

    /// Return the implicit legacy context from top-level provider fields.
    pub fn legacy_default_context(&self) -> Option<ContextConfig> {
        let ctx = ContextConfig {
            github: self.github.clone(),
            gitlab: self.gitlab.clone(),
            clickup: self.clickup.clone(),
            jira: self.jira.clone(),
            linear: self.linear.clone(),
            yougile: self.yougile.clone(),
            fireflies: self.fireflies.clone(),
            confluence: self.confluence.clone(),
            slack: self.slack.clone(),
            telegram: self.telegram.clone(),
        };

        if ctx.has_any_provider() {
            Some(ctx)
        } else {
            None
        }
    }

    /// Set a configuration value by key path.
    ///
    /// Supported key formats:
    /// - `provider.field` — e.g., `github.owner`, `gitlab.url`
    /// - `proxy.{routing|secrets|telemetry}.{field}` — e.g., `proxy.routing.strategy`
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let parts: Vec<&str> = key.split('.').collect();

        // Three-part paths belong to nested sections: `proxy.*`
        // and, since ADR-024 §6, `secrets.keychain.*`.
        if parts.len() == 3 {
            match parts[0] {
                "proxy" => return self.set_proxy_field(parts[1], parts[2], value),
                "secrets" => return self.set_secrets_subsection(parts[1], parts[2], value),
                _ => {}
            }
        }

        if parts.len() != 2 {
            return Err(Error::Config(format!(
                "Invalid config key '{}'. Expected formats: provider.field, \
                 proxy.section.field, or secrets.keychain.field",
                key
            )));
        }

        let (provider, field) = (parts[0], parts[1]);

        match provider {
            "github" => {
                let config = self.github.get_or_insert_with(|| GitHubConfig {
                    owner: String::new(),
                    repo: String::new(),
                    base_url: None,
                });
                match field {
                    "owner" => config.owner = value.to_string(),
                    "repo" => config.repo = value.to_string(),
                    "base_url" | "url" => config.base_url = Some(value.to_string()),
                    _ => {
                        return Err(Error::Config(format!(
                            "Unknown GitHub config field: {}",
                            field
                        )));
                    }
                }
            }
            "gitlab" => {
                let config = self.gitlab.get_or_insert_with(|| GitLabConfig {
                    url: default_gitlab_url(),
                    project_id: String::new(),
                });
                match field {
                    "url" => config.url = value.to_string(),
                    "project_id" | "project" => config.project_id = value.to_string(),
                    _ => {
                        return Err(Error::Config(format!(
                            "Unknown GitLab config field: {}",
                            field
                        )));
                    }
                }
            }
            "clickup" => {
                let config = self.clickup.get_or_insert_with(|| ClickUpConfig {
                    list_id: String::new(),
                    team_id: None,
                });
                match field {
                    "list_id" | "list" => config.list_id = value.to_string(),
                    "team_id" | "team" => config.team_id = Some(value.to_string()),
                    _ => {
                        return Err(Error::Config(format!(
                            "Unknown ClickUp config field: {}",
                            field
                        )));
                    }
                }
            }
            "jira" => {
                let config = self.jira.get_or_insert_with(|| JiraConfig {
                    url: String::new(),
                    project_key: String::new(),
                    email: String::new(),
                });
                match field {
                    "url" => config.url = value.to_string(),
                    "project_key" | "project" => config.project_key = value.to_string(),
                    "email" => config.email = value.to_string(),
                    _ => {
                        return Err(Error::Config(format!(
                            "Unknown Jira config field: {}",
                            field
                        )));
                    }
                }
            }
            "linear" => {
                let config = self.linear.get_or_insert_with(|| LinearConfig {
                    url: default_linear_url(),
                    team_id: String::new(),
                    team_key: None,
                });
                match field {
                    "url" | "base_url" => config.url = value.to_string(),
                    "team_id" | "team" => config.team_id = value.to_string(),
                    "team_key" | "key" => config.team_key = Some(value.to_string()),
                    _ => {
                        return Err(Error::Config(format!(
                            "Unknown Linear config field: {}",
                            field
                        )));
                    }
                }
            }
            "yougile" => {
                let config = self.yougile.get_or_insert_with(|| YouGileConfig {
                    url: default_yougile_url(),
                    board_id: String::new(),
                });
                match field {
                    "url" | "base_url" => config.url = value.to_string(),
                    "board_id" | "board" => config.board_id = value.to_string(),
                    _ => {
                        return Err(Error::Config(format!(
                            "Unknown YouGile config field: {}",
                            field
                        )));
                    }
                }
            }
            "confluence" => {
                let config = self.confluence.get_or_insert_with(|| ConfluenceConfig {
                    base_url: String::new(),
                    flavor: None,
                    cloud_id: None,
                    api_version: None,
                    username: None,
                    client_id: None,
                    redirect_uri: None,
                    space_key: None,
                });
                match field {
                    "base_url" | "url" => config.base_url = value.to_string(),
                    "flavor" => config.flavor = Some(parse_confluence_flavor(value)?),
                    "cloud_id" | "cloud" => config.cloud_id = Some(value.to_string()),
                    "api_version" | "api" | "version" => {
                        config.api_version = Some(value.to_string())
                    }
                    "username" | "email" | "user" => config.username = Some(value.to_string()),
                    "client_id" => config.client_id = Some(value.to_string()),
                    "redirect_uri" => config.redirect_uri = Some(value.to_string()),
                    "space_key" | "space" => config.space_key = Some(value.to_string()),
                    _ => {
                        return Err(Error::Config(format!(
                            "Unknown Confluence config field: {}",
                            field
                        )));
                    }
                }
            }
            "slack" => {
                let config = self.slack.get_or_insert_with(SlackConfig::default);
                match field {
                    "team_id" | "team" => config.team_id = Some(value.to_string()),
                    "workspace" => config.workspace = Some(value.to_string()),
                    "base_url" | "url" => config.base_url = Some(value.to_string()),
                    "client_id" => config.client_id = Some(value.to_string()),
                    "redirect_uri" => config.redirect_uri = Some(value.to_string()),
                    _ => {
                        return Err(Error::Config(format!(
                            "Unknown Slack config field: {}",
                            field
                        )));
                    }
                }
            }
            "telegram" => {
                let config = self.telegram.get_or_insert_with(TelegramConfig::default);
                match field {
                    "base_url" | "url" => config.base_url = Some(value.to_string()),
                    "bot_username" | "bot" | "username" => {
                        config.bot_username = Some(value.to_string())
                    }
                    _ => {
                        return Err(Error::Config(format!(
                            "Unknown Telegram config field: {}",
                            field
                        )));
                    }
                }
            }
            "secrets" => {
                let config = self.secrets.get_or_insert_with(SecretsConfig::default);
                match field {
                    "migration_complete" => {
                        config.migration_complete = parse_bool(value)?;
                    }
                    "profile" => {
                        config.profile = match value.trim().to_ascii_lowercase().as_str() {
                            "convenient" => SecretsProfile::Convenient,
                            "strict" => SecretsProfile::Strict,
                            other => {
                                return Err(Error::Config(format!(
                                    "Unknown secrets profile '{other}'. Expected 'convenient' \
                                     or 'strict'."
                                )));
                            }
                        };
                    }
                    "unlock_ttl_seconds" => {
                        config.unlock_ttl_seconds = Some(parse_u64(value, field)?);
                    }
                    "max_unlock_ttl_seconds" => {
                        config.max_unlock_ttl_seconds = Some(parse_u64(value, field)?);
                    }
                    "idle_relock_seconds" => {
                        config.idle_relock_seconds = Some(parse_u64(value, field)?);
                    }
                    "keyfile_path" | "keyfile" => {
                        config.keyfile_path = Some(PathBuf::from(value));
                    }
                    _ => {
                        return Err(Error::Config(format!(
                            "Unknown secrets config field: {field}. Known fields: \
                             migration_complete, profile, unlock_ttl_seconds, \
                             max_unlock_ttl_seconds, idle_relock_seconds, keyfile_path, \
                             keychain.enabled"
                        )));
                    }
                }
            }
            "runtime" => {
                let config = self.runtime.get_or_insert_with(RuntimeConfig::default);
                match field {
                    "ci" => config.ci = parse_bool(value)?,
                    _ => {
                        return Err(Error::Config(format!(
                            "Unknown runtime config field: {field}. Known fields: ci"
                        )));
                    }
                }
            }
            _ => {
                return Err(Error::Config(format!("Unknown provider: {}", provider)));
            }
        }

        Ok(())
    }

    /// Nested `secrets.<section>.<field>` setter. Currently only
    /// `secrets.keychain.enabled` (ADR-024 §6).
    fn set_secrets_subsection(&mut self, section: &str, field: &str, value: &str) -> Result<()> {
        let config = self.secrets.get_or_insert_with(SecretsConfig::default);
        match section {
            "keychain" => match field {
                "enabled" => {
                    config.keychain.enabled = parse_bool(value)?;
                    Ok(())
                }
                _ => Err(Error::Config(format!(
                    "Unknown secrets.keychain field: {field}. Known fields: enabled"
                ))),
            },
            _ => Err(Error::Config(format!(
                "Unknown secrets subsection: {section}. Known subsections: keychain"
            ))),
        }
    }

    /// Get a configuration value by key path.
    ///
    /// Supported key formats:
    /// - `provider.field` — e.g., `github.owner`, `gitlab.url`
    /// - `proxy.{routing|secrets|telemetry}.{field}` — e.g., `proxy.routing.strategy`
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        let parts: Vec<&str> = key.split('.').collect();

        if parts.len() == 3 {
            match parts[0] {
                "proxy" => return self.get_proxy_field(parts[1], parts[2]),
                "secrets" => return self.get_secrets_subsection(parts[1], parts[2]),
                _ => {}
            }
        }

        if parts.len() != 2 {
            return Err(Error::Config(format!(
                "Invalid config key '{}'. Expected formats: provider.field, \
                 proxy.section.field, or secrets.keychain.field",
                key
            )));
        }

        let (provider, field) = (parts[0], parts[1]);

        match provider {
            "github" => {
                let Some(config) = &self.github else {
                    return Ok(None);
                };
                match field {
                    "owner" => Ok(Some(config.owner.clone())),
                    "repo" => Ok(Some(config.repo.clone())),
                    "base_url" | "url" => Ok(config.base_url.clone()),
                    _ => Err(Error::Config(format!(
                        "Unknown GitHub config field: {}",
                        field
                    ))),
                }
            }
            "gitlab" => {
                let Some(config) = &self.gitlab else {
                    return Ok(None);
                };
                match field {
                    "url" => Ok(Some(config.url.clone())),
                    "project_id" | "project" => Ok(Some(config.project_id.clone())),
                    _ => Err(Error::Config(format!(
                        "Unknown GitLab config field: {}",
                        field
                    ))),
                }
            }
            "clickup" => {
                let Some(config) = &self.clickup else {
                    return Ok(None);
                };
                match field {
                    "list_id" | "list" => Ok(Some(config.list_id.clone())),
                    "team_id" | "team" => Ok(config.team_id.clone()),
                    _ => Err(Error::Config(format!(
                        "Unknown ClickUp config field: {}",
                        field
                    ))),
                }
            }
            "jira" => {
                let Some(config) = &self.jira else {
                    return Ok(None);
                };
                match field {
                    "url" => Ok(Some(config.url.clone())),
                    "project_key" | "project" => Ok(Some(config.project_key.clone())),
                    "email" => Ok(Some(config.email.clone())),
                    _ => Err(Error::Config(format!(
                        "Unknown Jira config field: {}",
                        field
                    ))),
                }
            }
            "linear" => {
                let Some(config) = &self.linear else {
                    return Ok(None);
                };
                match field {
                    "url" | "base_url" => Ok(Some(config.url.clone())),
                    "team_id" | "team" => Ok(Some(config.team_id.clone())),
                    "team_key" | "key" => Ok(config.team_key.clone()),
                    _ => Err(Error::Config(format!(
                        "Unknown Linear config field: {}",
                        field
                    ))),
                }
            }
            "yougile" => {
                let Some(config) = &self.yougile else {
                    return Ok(None);
                };
                match field {
                    "url" | "base_url" => Ok(Some(config.url.clone())),
                    "board_id" | "board" => Ok(Some(config.board_id.clone())),
                    _ => Err(Error::Config(format!(
                        "Unknown YouGile config field: {}",
                        field
                    ))),
                }
            }
            "confluence" => {
                let Some(config) = &self.confluence else {
                    return Ok(None);
                };
                match field {
                    "base_url" | "url" => Ok(Some(config.base_url.clone())),
                    "flavor" => Ok(config.flavor.map(confluence_flavor_slug)),
                    "cloud_id" | "cloud" => Ok(config.cloud_id.clone()),
                    "api_version" | "api" | "version" => Ok(config.api_version.clone()),
                    "username" | "email" | "user" => Ok(config.username.clone()),
                    "client_id" => Ok(config.client_id.clone()),
                    "redirect_uri" => Ok(config.redirect_uri.clone()),
                    "space_key" | "space" => Ok(config.space_key.clone()),
                    _ => Err(Error::Config(format!(
                        "Unknown Confluence config field: {}",
                        field
                    ))),
                }
            }
            "slack" => {
                let Some(config) = &self.slack else {
                    return Ok(None);
                };
                match field {
                    "team_id" | "team" => Ok(config.team_id.clone()),
                    "workspace" => Ok(config.workspace.clone()),
                    "base_url" | "url" => Ok(config.base_url.clone()),
                    "client_id" => Ok(config.client_id.clone()),
                    "redirect_uri" => Ok(config.redirect_uri.clone()),
                    _ => Err(Error::Config(format!(
                        "Unknown Slack config field: {}",
                        field
                    ))),
                }
            }
            "telegram" => {
                let Some(config) = &self.telegram else {
                    return Ok(None);
                };
                match field {
                    "base_url" | "url" => Ok(config.base_url.clone()),
                    "bot_username" | "bot" | "username" => Ok(config.bot_username.clone()),
                    _ => Err(Error::Config(format!(
                        "Unknown Telegram config field: {}",
                        field
                    ))),
                }
            }
            // Secrets fields report the *effective* value, so a
            // `get` after a fresh install shows the profile
            // defaults rather than an empty string. The exception
            // is `keyfile_path`, which has no default.
            "secrets" => match field {
                "migration_complete" => Ok(Some(self.is_secrets_migration_complete().to_string())),
                "profile" => Ok(Some(
                    match self.secrets_profile() {
                        SecretsProfile::Convenient => "convenient",
                        SecretsProfile::Strict => "strict",
                    }
                    .to_string(),
                )),
                "unlock_ttl_seconds" => Ok(Some(self.unlock_ttl_seconds().to_string())),
                "max_unlock_ttl_seconds" => Ok(Some(self.max_unlock_ttl_seconds().to_string())),
                "idle_relock_seconds" => Ok(self.idle_relock_seconds().map(|v| v.to_string())),
                "keyfile_path" | "keyfile" => {
                    Ok(self.secrets_keyfile_path().map(|p| p.display().to_string()))
                }
                _ => Err(Error::Config(format!(
                    "Unknown secrets config field: {field}"
                ))),
            },
            "runtime" => match field {
                "ci" => Ok(Some(self.is_ci_forced().to_string())),
                _ => Err(Error::Config(format!(
                    "Unknown runtime config field: {field}"
                ))),
            },
            _ => Err(Error::Config(format!("Unknown provider: {}", provider))),
        }
    }

    /// Nested `secrets.<section>.<field>` getter, mirroring
    /// [`Self::set_secrets_subsection`].
    fn get_secrets_subsection(&self, section: &str, field: &str) -> Result<Option<String>> {
        match section {
            "keychain" => match field {
                "enabled" => Ok(Some(self.is_keychain_enabled().to_string())),
                _ => Err(Error::Config(format!(
                    "Unknown secrets.keychain field: {field}"
                ))),
            },
            _ => Err(Error::Config(format!(
                "Unknown secrets subsection: {section}"
            ))),
        }
    }

    /// Set a `proxy.{section}.{field}` value. Extracted so [`Self::set`] stays small.
    fn set_proxy_field(&mut self, section: &str, field: &str, value: &str) -> Result<()> {
        match section {
            "routing" => match field {
                "strategy" => {
                    let strat = RoutingStrategy::parse(value).ok_or_else(|| {
                        Error::Config(format!(
                            "Invalid routing strategy '{}'. Allowed (case-insensitive): \
                             remote, local, local-first, remote-first",
                            value
                        ))
                    })?;
                    self.proxy.routing.strategy = strat;
                    Ok(())
                }
                "fallback_on_error" => {
                    self.proxy.routing.fallback_on_error = parse_bool(value)?;
                    Ok(())
                }
                _ => Err(Error::Config(format!(
                    "Unknown proxy.routing field: {}",
                    field
                ))),
            },
            "secrets" => match field {
                "cache_ttl_secs" => {
                    self.proxy.secrets.cache_ttl_secs = parse_u64(value, field)?;
                    Ok(())
                }
                _ => Err(Error::Config(format!(
                    "Unknown proxy.secrets field: {}",
                    field
                ))),
            },
            "telemetry" => match field {
                "enabled" => {
                    self.proxy.telemetry.enabled = parse_bool(value)?;
                    Ok(())
                }
                "endpoint" => {
                    self.proxy.telemetry.endpoint = if value.is_empty() {
                        None
                    } else {
                        validate_http_url(value, "proxy.telemetry.endpoint")?;
                        Some(value.to_string())
                    };
                    Ok(())
                }
                "token_key" => {
                    self.proxy.telemetry.token_key = if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    };
                    Ok(())
                }
                "batch_size" => {
                    self.proxy.telemetry.batch_size = parse_usize(value, field)?;
                    Ok(())
                }
                "batch_interval_secs" => {
                    self.proxy.telemetry.batch_interval_secs = parse_u64(value, field)?;
                    Ok(())
                }
                "offline_queue_max" => {
                    self.proxy.telemetry.offline_queue_max = parse_usize(value, field)?;
                    Ok(())
                }
                _ => Err(Error::Config(format!(
                    "Unknown proxy.telemetry field: {}",
                    field
                ))),
            },
            _ => Err(Error::Config(format!(
                "Unknown proxy section: {}. Allowed: routing, secrets, telemetry",
                section
            ))),
        }
    }

    /// Read a `proxy.{section}.{field}` value. Returns `Ok(None)` for fields that are
    /// unset (e.g., optional `telemetry.endpoint`).
    fn get_proxy_field(&self, section: &str, field: &str) -> Result<Option<String>> {
        match section {
            "routing" => match field {
                "strategy" => Ok(Some(routing_strategy_slug(self.proxy.routing.strategy))),
                "fallback_on_error" => Ok(Some(self.proxy.routing.fallback_on_error.to_string())),
                _ => Err(Error::Config(format!(
                    "Unknown proxy.routing field: {}",
                    field
                ))),
            },
            "secrets" => match field {
                "cache_ttl_secs" => Ok(Some(self.proxy.secrets.cache_ttl_secs.to_string())),
                _ => Err(Error::Config(format!(
                    "Unknown proxy.secrets field: {}",
                    field
                ))),
            },
            "telemetry" => match field {
                "enabled" => Ok(Some(self.proxy.telemetry.enabled.to_string())),
                "endpoint" => Ok(self.proxy.telemetry.endpoint.clone()),
                "token_key" => Ok(self.proxy.telemetry.token_key.clone()),
                "batch_size" => Ok(Some(self.proxy.telemetry.batch_size.to_string())),
                "batch_interval_secs" => {
                    Ok(Some(self.proxy.telemetry.batch_interval_secs.to_string()))
                }
                "offline_queue_max" => Ok(Some(self.proxy.telemetry.offline_queue_max.to_string())),
                _ => Err(Error::Config(format!(
                    "Unknown proxy.telemetry field: {}",
                    field
                ))),
            },
            _ => Err(Error::Config(format!(
                "Unknown proxy section: {}. Allowed: routing, secrets, telemetry",
                section
            ))),
        }
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(Error::Config(format!(
            "Invalid boolean '{}'. Allowed: true/false, 1/0, yes/no, on/off",
            value
        ))),
    }
}

fn parse_u64(value: &str, field: &str) -> Result<u64> {
    value.trim().parse::<u64>().map_err(|_| {
        Error::Config(format!(
            "Invalid value for {}: '{}'. Expected non-negative integer",
            field, value
        ))
    })
}

fn parse_usize(value: &str, field: &str) -> Result<usize> {
    value.trim().parse::<usize>().map_err(|_| {
        Error::Config(format!(
            "Invalid value for {}: '{}'. Expected non-negative integer",
            field, value
        ))
    })
}

/// Lightweight sanity check that the value looks like a valid HTTP(S) URL.
///
/// A full RFC 3986 parser would pull in the `url` crate for a single field, which no
/// other part of `devboy-core` needs. To reject the obvious garbage (`not-a-url`,
/// `ftp://…`, lone slashes) it is enough to verify that the string:
/// - starts with `http://` or `https://`
/// - has at least one non-empty character after the scheme, before any `/`, `?`, `#`
/// - contains no whitespace anywhere (host, path, query)
///
/// Stricter validation (DNS labels, port, escaping) is left to `reqwest` at upload
/// time; this helper exists purely to catch user typos at configuration time.
fn validate_http_url(value: &str, field: &str) -> Result<()> {
    // A correct URL has no whitespace anywhere (host, path, or query). Reject the
    // whole string up-front instead of letting e.g. `https://example.com/a b` slip
    // through just because the host part was clean.
    if value.contains(|c: char| c.is_whitespace()) {
        return Err(Error::Config(format!(
            "Invalid URL for {}: '{}'. Must not contain whitespace",
            field, value
        )));
    }

    let rest = if let Some(r) = value.strip_prefix("https://") {
        r
    } else if let Some(r) = value.strip_prefix("http://") {
        r
    } else {
        return Err(Error::Config(format!(
            "Invalid URL for {}: '{}'. Must start with http:// or https://",
            field, value
        )));
    };

    // Minimal host extraction — everything up to the first `/`, `?` or `#`.
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..host_end];
    if host.is_empty() {
        return Err(Error::Config(format!(
            "Invalid URL for {}: '{}'. Missing host",
            field, value
        )));
    }

    Ok(())
}

/// Stable kebab-case slug for a [`RoutingStrategy`]. Symmetric with serde and TOML
/// serialisation. Exported so CLI / observability code renders strategy values the
/// same way in every surface (JSON, plain text, `config list`).
pub fn routing_strategy_slug(s: RoutingStrategy) -> String {
    match s {
        RoutingStrategy::Remote => "remote",
        RoutingStrategy::Local => "local",
        RoutingStrategy::LocalFirst => "local-first",
        RoutingStrategy::RemoteFirst => "remote-first",
    }
    .to_string()
}

impl ContextConfig {
    /// Check whether this context config defines at least one provider.
    pub fn has_any_provider(&self) -> bool {
        self.github.is_some()
            || self.gitlab.is_some()
            || self.clickup.is_some()
            || self.jira.is_some()
            || self.linear.is_some()
            || self.yougile.is_some()
            || self.fireflies.is_some()
            || self.confluence.is_some()
            || self.slack.is_some()
            || self.telegram.is_some()
    }

    /// Return configured provider names for this context.
    pub fn configured_providers(&self) -> Vec<&'static str> {
        let mut providers = Vec::new();
        if self.github.is_some() {
            providers.push("github");
        }
        if self.gitlab.is_some() {
            providers.push("gitlab");
        }
        if self.clickup.is_some() {
            providers.push("clickup");
        }
        if self.jira.is_some() {
            providers.push("jira");
        }
        if self.linear.is_some() {
            providers.push("linear");
        }
        if self.yougile.is_some() {
            providers.push("yougile");
        }
        if self.confluence.is_some() {
            providers.push("confluence");
        }
        if self.slack.is_some() {
            providers.push("slack");
        }
        if self.telegram.is_some() {
            providers.push("telegram");
        }
        providers
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod secrets_config_tests {
    use super::*;

    /// A config with no `[secrets]` section at all must behave
    /// exactly like the `convenient` profile — this is the
    /// upgrade path for every existing user.
    #[test]
    fn absent_section_yields_convenient_defaults() {
        let config = Config::default();

        assert_eq!(config.secrets_profile(), SecretsProfile::Convenient);
        assert_eq!(config.unlock_ttl_seconds(), 8 * 60 * 60);
        assert_eq!(config.max_unlock_ttl_seconds(), 24 * 60 * 60);
        assert_eq!(config.idle_relock_seconds(), None);
        assert!(!config.is_secrets_migration_complete());
        assert!(config.secrets_keyfile_path().is_none());
    }

    /// ADR-024 §6: the OS keychain is off by default on *every*
    /// platform, including macOS. If this test ever flips, the
    /// default posture changed.
    #[test]
    fn keychain_is_disabled_by_default() {
        assert!(!Config::default().is_keychain_enabled());

        let mut config = Config::default();
        config.set("secrets.keychain.enabled", "true").unwrap();
        assert!(config.is_keychain_enabled());
        assert_eq!(
            config.get("secrets.keychain.enabled").unwrap().as_deref(),
            Some("true")
        );
    }

    /// ADR-024 §6: CI mode is never on implicitly.
    #[test]
    fn runtime_ci_is_off_by_default_and_settable() {
        assert!(!Config::default().is_ci_forced());

        let mut config = Config::default();
        config.set("runtime.ci", "1").unwrap();
        assert!(config.is_ci_forced());
        assert_eq!(config.get("runtime.ci").unwrap().as_deref(), Some("true"));
    }

    #[test]
    fn strict_profile_supplies_its_own_window() {
        let mut config = Config::default();
        config.set("secrets.profile", "strict").unwrap();

        assert_eq!(config.secrets_profile(), SecretsProfile::Strict);
        assert_eq!(config.unlock_ttl_seconds(), 15 * 60);
        assert_eq!(config.max_unlock_ttl_seconds(), 60 * 60);
        assert_eq!(config.idle_relock_seconds(), Some(5 * 60));
        assert!(SecretsProfile::Strict.forces_per_call_approval());
        assert!(SecretsProfile::Strict.requires_prompt_surface());
    }

    #[test]
    fn convenient_profile_does_not_force_approval_or_prompt_surface() {
        assert!(!SecretsProfile::Convenient.forces_per_call_approval());
        assert!(!SecretsProfile::Convenient.requires_prompt_surface());
    }

    #[test]
    fn explicit_values_override_profile_defaults() {
        let mut config = Config::default();
        config.set("secrets.profile", "strict").unwrap();
        config.set("secrets.unlock_ttl_seconds", "600").unwrap();
        config.set("secrets.idle_relock_seconds", "60").unwrap();

        assert_eq!(config.unlock_ttl_seconds(), 600);
        assert_eq!(config.idle_relock_seconds(), Some(60));
        // Untouched field still follows the profile.
        assert_eq!(config.max_unlock_ttl_seconds(), 60 * 60);
    }

    /// A window wider than the ceiling is clamped rather than
    /// rejected, and the inconsistency is reported instead of
    /// being silently absorbed.
    #[test]
    fn unlock_ttl_is_clamped_to_max_and_warned_about() {
        let mut config = Config::default();
        config
            .set("secrets.max_unlock_ttl_seconds", "3600")
            .unwrap();
        config.set("secrets.unlock_ttl_seconds", "86400").unwrap();

        assert_eq!(config.unlock_ttl_seconds(), 3600);

        let warnings = config.secrets_config_warnings();
        assert!(
            warnings.iter().any(|w| w.contains("clamped")),
            "expected a clamp warning, got: {warnings:?}"
        );
    }

    #[test]
    fn idle_relock_longer_than_window_is_reported_as_unreachable() {
        let mut config = Config::default();
        config.set("secrets.unlock_ttl_seconds", "300").unwrap();
        config.set("secrets.idle_relock_seconds", "600").unwrap();

        let warnings = config.secrets_config_warnings();
        assert!(
            warnings.iter().any(|w| w.contains("never fire")),
            "expected an unreachable-idle warning, got: {warnings:?}"
        );
    }

    #[test]
    fn zero_unlock_ttl_is_reported() {
        let mut config = Config::default();
        config.set("secrets.unlock_ttl_seconds", "0").unwrap();

        let warnings = config.secrets_config_warnings();
        assert!(
            warnings.iter().any(|w| w.contains("re-locks the vault")),
            "expected a zero-window warning, got: {warnings:?}"
        );
    }

    #[test]
    fn clean_config_produces_no_warnings() {
        let mut config = Config::default();
        config.set("secrets.profile", "convenient").unwrap();
        assert!(config.secrets_config_warnings().is_empty());
    }

    #[test]
    fn unknown_fields_and_profiles_are_rejected() {
        let mut config = Config::default();

        assert!(config.set("secrets.profile", "paranoid").is_err());
        assert!(config.set("secrets.nope", "1").is_err());
        assert!(config.set("secrets.keychain.nope", "1").is_err());
        assert!(config.set("secrets.nosuch.enabled", "1").is_err());
        assert!(config.set("runtime.nope", "1").is_err());
        assert!(config.set("secrets.unlock_ttl_seconds", "-5").is_err());
        assert!(config.set("secrets.keychain.enabled", "maybe").is_err());
    }

    #[test]
    fn keyfile_path_round_trips() {
        let mut config = Config::default();
        config
            .set("secrets.keyfile_path", "/var/lib/devboy/vault.key")
            .unwrap();

        assert_eq!(
            config
                .secrets_keyfile_path()
                .map(|p| p.display().to_string()),
            Some("/var/lib/devboy/vault.key".to_string())
        );
        assert_eq!(
            config.get("secrets.keyfile_path").unwrap().as_deref(),
            Some("/var/lib/devboy/vault.key")
        );
    }

    /// Round-trip through TOML: defaults must not be written out,
    /// so an untouched config file stays empty rather than
    /// freezing today's defaults into every user's config.
    #[test]
    fn defaults_are_not_serialized() {
        let config = Config::default();
        let toml = toml::to_string(&config).unwrap();

        assert!(!toml.contains("[secrets]"), "got: {toml}");
        assert!(!toml.contains("[runtime]"), "got: {toml}");
    }

    #[test]
    fn non_default_values_survive_a_toml_round_trip() {
        let mut config = Config::default();
        config.set("secrets.profile", "strict").unwrap();
        config.set("secrets.keychain.enabled", "true").unwrap();
        config.set("runtime.ci", "true").unwrap();
        config.set("secrets.unlock_ttl_seconds", "1800").unwrap();

        let toml = toml::to_string(&config).unwrap();
        let parsed: Config = toml::from_str(&toml).unwrap();

        assert_eq!(parsed.secrets_profile(), SecretsProfile::Strict);
        assert!(parsed.is_keychain_enabled());
        assert!(parsed.is_ci_forced());
        assert_eq!(parsed.unlock_ttl_seconds(), 1800);
    }

    /// The legacy reader is gated on `migration_complete`, not on
    /// the new keychain switch — otherwise the ADR-024 default
    /// flip would strand users whose tokens are still in the OS
    /// keychain.
    #[test]
    fn migration_flag_is_independent_of_the_keychain_switch() {
        let mut config = Config::default();
        config.set("secrets.migration_complete", "true").unwrap();

        assert!(config.is_secrets_migration_complete());
        assert!(!config.is_keychain_enabled());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.github.is_none());
        assert!(config.gitlab.is_none());
        assert!(config.telegram.is_none());
        assert!(config.contexts.is_empty());
        assert!(!config.has_any_provider());
        assert!(config.configured_providers().is_empty());
    }

    #[test]
    fn test_set_and_get() {
        let mut config = Config::default();

        // Set GitHub config
        config.set("github.owner", "test-owner").unwrap();
        config.set("github.repo", "test-repo").unwrap();

        assert_eq!(
            config.get("github.owner").unwrap(),
            Some("test-owner".to_string())
        );
        assert_eq!(
            config.get("github.repo").unwrap(),
            Some("test-repo".to_string())
        );

        // Set GitLab config
        config
            .set("gitlab.url", "https://gitlab.example.com")
            .unwrap();
        config.set("gitlab.project_id", "123").unwrap();

        assert_eq!(
            config.get("gitlab.url").unwrap(),
            Some("https://gitlab.example.com".to_string())
        );

        // Check configured providers
        assert!(config.has_any_provider());
        let providers = config.configured_providers();
        assert!(providers.contains(&"github"));
        assert!(providers.contains(&"gitlab"));
    }

    #[test]
    fn test_set_and_get_telegram() {
        let mut config = Config::default();

        config
            .set("telegram.base_url", "https://api.telegram.org")
            .unwrap();
        config.set("telegram.bot_username", "devboy_bot").unwrap();

        assert_eq!(
            config.get("telegram.base_url").unwrap(),
            Some("https://api.telegram.org".to_string())
        );
        assert_eq!(
            config.get("telegram.url").unwrap(),
            Some("https://api.telegram.org".to_string())
        );
        assert_eq!(
            config.get("telegram.bot_username").unwrap(),
            Some("devboy_bot".to_string())
        );
        assert_eq!(
            config.get("telegram.bot").unwrap(),
            Some("devboy_bot".to_string())
        );
    }

    #[test]
    fn test_default_slack_required_scopes_cover_default_conversation_types() {
        let scopes = default_slack_required_scopes();

        assert!(scopes.contains(&"channels:read".to_string()));
        assert!(scopes.contains(&"channels:history".to_string()));
        assert!(scopes.contains(&"groups:read".to_string()));
        assert!(scopes.contains(&"groups:history".to_string()));
        assert!(scopes.contains(&"im:read".to_string()));
        assert!(scopes.contains(&"im:history".to_string()));
        assert!(scopes.contains(&"mpim:read".to_string()));
        assert!(scopes.contains(&"mpim:history".to_string()));
    }

    #[test]
    fn test_invalid_key() {
        let mut config = Config::default();

        // Invalid key format
        assert!(config.set("invalid", "value").is_err());
        assert!(config.set("too.many.parts", "value").is_err());

        // Unknown provider
        assert!(config.set("unknown.field", "value").is_err());
        assert!(config.set("telegram.unknown", "value").is_err());

        // When provider config doesn't exist, get returns Ok(None)
        assert_eq!(config.get("github.owner").unwrap(), None);

        // But unknown field on configured provider should error
        config.set("github.owner", "test").unwrap();
        assert!(config.get("github.unknown_field").is_err());
    }

    #[test]
    fn is_secrets_migration_complete_defaults_to_false() {
        let config = Config::default();
        assert!(!config.is_secrets_migration_complete());
    }

    #[test]
    fn is_secrets_migration_complete_reads_explicit_flag() {
        let config = Config {
            secrets: Some(SecretsConfig {
                migration_complete: true,
                ..SecretsConfig::default()
            }),
            ..Config::default()
        };
        assert!(config.is_secrets_migration_complete());
    }

    #[test]
    fn secrets_section_round_trips_through_toml() {
        let toml = "[secrets]\nmigration_complete = true\n";
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.is_secrets_migration_complete());
        let serialized = toml::to_string(&config).unwrap();
        assert!(serialized.contains("[secrets]"));
        assert!(serialized.contains("migration_complete = true"));
    }

    #[test]
    fn secrets_section_omitted_when_unset() {
        let config = Config::default();
        let serialized = toml::to_string(&config).unwrap();
        assert!(
            !serialized.contains("[secrets]"),
            "default Config should not write a [secrets] section"
        );
    }

    #[test]
    fn test_save_and_load() {
        let config = Config {
            github: Some(GitHubConfig {
                owner: "test-owner".to_string(),
                repo: "test-repo".to_string(),
                base_url: None,
            }),
            ..Default::default()
        };

        // Save to temp file
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        config.save_to(&path).unwrap();

        // Read raw content
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("owner = \"test-owner\""));
        assert!(contents.contains("repo = \"test-repo\""));

        // Load back
        let loaded = Config::load_from(&path).unwrap();
        assert!(loaded.github.is_some());
        let gh = loaded.github.unwrap();
        assert_eq!(gh.owner, "test-owner");
        assert_eq!(gh.repo, "test-repo");
    }

    #[test]
    fn test_load_nonexistent() {
        let path = PathBuf::from("/nonexistent/path/config.toml");
        let config = Config::load_from(&path).unwrap();
        assert!(config.github.is_none());
    }

    #[test]
    fn test_set_and_get_gitlab() {
        let mut config = Config::default();

        config
            .set("gitlab.url", "https://gitlab.example.com")
            .unwrap();
        config.set("gitlab.project_id", "456").unwrap();

        assert_eq!(
            config.get("gitlab.url").unwrap(),
            Some("https://gitlab.example.com".to_string())
        );
        assert_eq!(
            config.get("gitlab.project_id").unwrap(),
            Some("456".to_string())
        );
        // Test alias
        assert_eq!(
            config.get("gitlab.project").unwrap(),
            Some("456".to_string())
        );
    }

    #[test]
    fn test_set_and_get_gitlab_alias() {
        let mut config = Config::default();

        config.set("gitlab.project", "789").unwrap();

        assert_eq!(
            config.get("gitlab.project_id").unwrap(),
            Some("789".to_string())
        );
    }

    #[test]
    fn test_set_and_get_clickup() {
        let mut config = Config::default();

        config.set("clickup.list_id", "list123").unwrap();

        assert_eq!(
            config.get("clickup.list_id").unwrap(),
            Some("list123".to_string())
        );
        // Test alias
        assert_eq!(
            config.get("clickup.list").unwrap(),
            Some("list123".to_string())
        );
    }

    #[test]
    fn test_set_and_get_clickup_alias() {
        let mut config = Config::default();

        config.set("clickup.list", "list456").unwrap();

        assert_eq!(
            config.get("clickup.list_id").unwrap(),
            Some("list456".to_string())
        );
    }

    #[test]
    fn test_set_and_get_jira() {
        let mut config = Config::default();

        config.set("jira.url", "https://jira.example.com").unwrap();
        config.set("jira.project_key", "PROJ").unwrap();
        config.set("jira.email", "user@example.com").unwrap();

        assert_eq!(
            config.get("jira.url").unwrap(),
            Some("https://jira.example.com".to_string())
        );
        assert_eq!(
            config.get("jira.project_key").unwrap(),
            Some("PROJ".to_string())
        );
        assert_eq!(
            config.get("jira.email").unwrap(),
            Some("user@example.com".to_string())
        );
        // Test alias
        assert_eq!(
            config.get("jira.project").unwrap(),
            Some("PROJ".to_string())
        );
    }

    #[test]
    fn test_set_and_get_jira_alias() {
        let mut config = Config::default();

        config.set("jira.project", "KEY").unwrap();

        assert_eq!(
            config.get("jira.project_key").unwrap(),
            Some("KEY".to_string())
        );
    }

    #[test]
    fn test_set_and_get_linear() {
        let mut config = Config::default();

        config
            .set("linear.url", "https://linear.example.com/graphql")
            .unwrap();
        config.set("linear.team_id", "team-123").unwrap();
        config.set("linear.team_key", "ENG").unwrap();

        assert_eq!(
            config.get("linear.url").unwrap(),
            Some("https://linear.example.com/graphql".to_string())
        );
        assert_eq!(
            config.get("linear.base_url").unwrap(),
            Some("https://linear.example.com/graphql".to_string())
        );
        assert_eq!(
            config.get("linear.team_id").unwrap(),
            Some("team-123".to_string())
        );
        assert_eq!(
            config.get("linear.team").unwrap(),
            Some("team-123".to_string())
        );
        assert_eq!(
            config.get("linear.team_key").unwrap(),
            Some("ENG".to_string())
        );
        assert_eq!(config.get("linear.key").unwrap(), Some("ENG".to_string()));
    }

    #[test]
    fn test_set_and_get_yougile() {
        let mut config = Config::default();

        config
            .set("yougile.url", "https://company.yougile.com/api-v2")
            .unwrap();
        config.set("yougile.board_id", "board-123").unwrap();

        assert_eq!(
            config.get("yougile.url").unwrap(),
            Some("https://company.yougile.com/api-v2".to_string())
        );
        assert_eq!(
            config.get("yougile.base_url").unwrap(),
            Some("https://company.yougile.com/api-v2".to_string())
        );
        assert_eq!(
            config.get("yougile.board_id").unwrap(),
            Some("board-123".to_string())
        );
        assert_eq!(
            config.get("yougile.board").unwrap(),
            Some("board-123".to_string())
        );
    }

    #[test]
    fn test_set_and_get_yougile_alias() {
        let mut config = Config::default();

        config.set("yougile.board", "board-456").unwrap();

        assert_eq!(
            config.get("yougile.board_id").unwrap(),
            Some("board-456".to_string())
        );
    }

    #[test]
    fn test_set_and_get_confluence() {
        let mut config = Config::default();

        config
            .set("confluence.base_url", "https://wiki.example.com")
            .unwrap();
        config.set("confluence.flavor", "cloud").unwrap();
        config.set("confluence.cloud_id", "cloud-123").unwrap();
        config.set("confluence.api_version", "v1").unwrap();
        config
            .set("confluence.username", "dev@example.com")
            .unwrap();
        config.set("confluence.client_id", "client-123").unwrap();
        config
            .set("confluence.redirect_uri", "http://localhost:8787/callback")
            .unwrap();
        config.set("confluence.space_key", "ENG").unwrap();

        assert_eq!(
            config.get("confluence.base_url").unwrap(),
            Some("https://wiki.example.com".to_string())
        );
        assert_eq!(
            config.get("confluence.url").unwrap(),
            Some("https://wiki.example.com".to_string())
        );
        assert_eq!(
            config.get("confluence.flavor").unwrap(),
            Some("cloud".to_string())
        );
        assert_eq!(
            config.get("confluence.cloud").unwrap(),
            Some("cloud-123".to_string())
        );
        assert_eq!(
            config.get("confluence.api").unwrap(),
            Some("v1".to_string())
        );
        assert_eq!(
            config.get("confluence.username").unwrap(),
            Some("dev@example.com".to_string())
        );
        assert_eq!(
            config.get("confluence.client_id").unwrap(),
            Some("client-123".to_string())
        );
        assert_eq!(
            config.get("confluence.redirect_uri").unwrap(),
            Some("http://localhost:8787/callback".to_string())
        );
        assert_eq!(
            config.get("confluence.space").unwrap(),
            Some("ENG".to_string())
        );
    }

    #[test]
    fn test_set_github_base_url() {
        let mut config = Config::default();

        config
            .set("github.base_url", "https://github.example.com/api/v3")
            .unwrap();

        assert_eq!(
            config.get("github.base_url").unwrap(),
            Some("https://github.example.com/api/v3".to_string())
        );
        // url alias should also work for get
        assert_eq!(
            config.get("github.url").unwrap(),
            Some("https://github.example.com/api/v3".to_string())
        );
    }

    #[test]
    fn test_set_github_url_alias() {
        let mut config = Config::default();

        config
            .set("github.url", "https://github.example.com/api/v3")
            .unwrap();

        assert_eq!(
            config.get("github.base_url").unwrap(),
            Some("https://github.example.com/api/v3".to_string())
        );
    }

    #[test]
    fn test_unknown_field_errors() {
        let mut config = Config::default();

        // GitHub unknown field
        assert!(config.set("github.unknown", "value").is_err());
        config.set("github.owner", "test").unwrap();
        assert!(config.get("github.unknown").is_err());

        // GitLab unknown field
        assert!(config.set("gitlab.unknown", "value").is_err());
        config.set("gitlab.url", "https://gitlab.com").unwrap();
        assert!(config.get("gitlab.unknown").is_err());

        // ClickUp unknown field
        assert!(config.set("clickup.unknown", "value").is_err());
        config.set("clickup.list_id", "123").unwrap();
        assert!(config.get("clickup.unknown").is_err());

        // Jira unknown field
        assert!(config.set("jira.unknown", "value").is_err());
        config.set("jira.url", "https://jira.com").unwrap();
        assert!(config.get("jira.unknown").is_err());

        // Linear unknown field
        assert!(config.set("linear.unknown", "value").is_err());
        config.set("linear.team_id", "team-1").unwrap();
        assert!(config.get("linear.unknown").is_err());
        // YouGile unknown field
        assert!(config.set("yougile.unknown", "value").is_err());
        config.set("yougile.board_id", "board-123").unwrap();
        assert!(config.get("yougile.unknown").is_err());
    }

    #[test]
    fn test_get_unconfigured_providers() {
        let config = Config::default();

        assert_eq!(config.get("github.owner").unwrap(), None);
        assert_eq!(config.get("gitlab.url").unwrap(), None);
        assert_eq!(config.get("clickup.list_id").unwrap(), None);
        assert_eq!(config.get("jira.url").unwrap(), None);
        assert_eq!(config.get("linear.team_id").unwrap(), None);
        assert_eq!(config.get("yougile.url").unwrap(), None);
        assert_eq!(config.get("confluence.base_url").unwrap(), None);
        assert_eq!(config.get("telegram.base_url").unwrap(), None);
    }

    #[test]
    fn test_unknown_provider_set() {
        let mut config = Config::default();
        let result = config.set("unknown.field", "value");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Unknown provider: unknown"));
    }

    #[test]
    fn test_unknown_provider_get() {
        let config = Config::default();
        let result = config.get("unknown.field");
        assert!(result.is_err());
    }

    #[test]
    fn test_malformed_toml() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        std::fs::write(&path, "invalid toml content [[[").unwrap();

        let result = Config::load_from(&path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Failed to parse config file"));
    }

    #[test]
    fn test_configured_providers_all() {
        let config = Config {
            github: Some(GitHubConfig {
                owner: "o".to_string(),
                repo: "r".to_string(),
                base_url: None,
            }),
            gitlab: Some(GitLabConfig {
                url: "u".to_string(),
                project_id: "p".to_string(),
            }),
            clickup: Some(ClickUpConfig {
                list_id: "l".to_string(),
                team_id: None,
            }),
            jira: Some(JiraConfig {
                url: "u".to_string(),
                project_key: "k".to_string(),
                email: "e".to_string(),
            }),
            linear: Some(LinearConfig {
                url: "https://api.linear.app/graphql".to_string(),
                team_id: "team-1".to_string(),
                team_key: Some("ENG".to_string()),
            }),
            yougile: Some(YouGileConfig {
                url: default_yougile_url(),
                board_id: "board-1".to_string(),
            }),
            fireflies: None,
            confluence: None,
            slack: None,
            telegram: Some(TelegramConfig {
                base_url: Some("https://api.telegram.org".to_string()),
                bot_username: Some("devboy_bot".to_string()),
            }),
            contexts: BTreeMap::new(),
            active_context: None,
            proxy_mcp_servers: Vec::new(),
            builtin_tools: BuiltinToolsConfig::default(),
            format_pipeline: None,
            proxy: ProxyConfig::default(),
            sentry: None,
            remote_config: None,
            secrets: None,
            runtime: None,
        };

        let providers = config.configured_providers();
        assert_eq!(providers.len(), 7);
        assert!(providers.contains(&"github"));
        assert!(providers.contains(&"gitlab"));
        assert!(providers.contains(&"clickup"));
        assert!(providers.contains(&"jira"));
        assert!(providers.contains(&"linear"));
        assert!(providers.contains(&"yougile"));
        assert!(providers.contains(&"telegram"));
        assert!(config.has_any_provider());
    }

    #[test]
    fn test_legacy_default_context_includes_linear() {
        let config = Config {
            linear: Some(LinearConfig {
                url: "https://api.linear.app/graphql".to_string(),
                team_id: "team-legacy".to_string(),
                team_key: Some("OPS".to_string()),
            }),
            ..Config::default()
        };

        let context = config
            .legacy_default_context()
            .expect("legacy default context should exist");
        let linear = context.linear.expect("linear should be present");
        assert_eq!(linear.team_id, "team-legacy");
        assert_eq!(linear.team_key.as_deref(), Some("OPS"));
    }

    #[test]
    fn test_config_dir() {
        // config_dir() should return a path ending with CONFIG_DIR_NAME
        let dir = Config::config_dir().unwrap();
        assert!(dir.ends_with("devboy-tools"));
    }

    #[test]
    fn test_config_path() {
        // config_path() should return config_dir/config.toml
        let path = Config::config_path().unwrap();
        assert!(path.ends_with("config.toml"));
        assert!(path.parent().unwrap().ends_with("devboy-tools"));
    }

    #[test]
    fn test_load_default_path() {
        // Use a temp path so the test is isolated from the real user/system config
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // load_from() should return a default config if the file doesn't exist
        let config = Config::load_from(&path).unwrap();
        assert!(!config.has_any_provider());
    }

    #[test]
    fn test_save_default_path() {
        // Test save() to an actual temp location by using save_to
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let config = Config {
            github: Some(GitHubConfig {
                owner: "test".to_string(),
                repo: "repo".to_string(),
                base_url: None,
            }),
            ..Default::default()
        };

        config.save_to(&path).unwrap();
        assert!(path.exists());

        // Reload and verify
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.github.unwrap().owner, "test");
    }

    #[test]
    fn test_toml_serialization() {
        let config = Config {
            github: Some(GitHubConfig {
                owner: "owner".to_string(),
                repo: "repo".to_string(),
                base_url: Some("https://github.example.com".to_string()),
            }),
            gitlab: Some(GitLabConfig {
                url: "https://gitlab.example.com".to_string(),
                project_id: "123".to_string(),
            }),
            clickup: None,
            jira: None,
            linear: None,
            yougile: None,
            fireflies: None,
            confluence: None,
            slack: None,
            telegram: Some(TelegramConfig {
                base_url: Some("https://api.telegram.org".to_string()),
                bot_username: Some("devboy_bot".to_string()),
            }),
            contexts: BTreeMap::new(),
            active_context: None,
            proxy_mcp_servers: Vec::new(),
            builtin_tools: BuiltinToolsConfig::default(),
            format_pipeline: None,
            proxy: ProxyConfig::default(),
            sentry: None,
            remote_config: None,
            secrets: None,
            runtime: None,
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(toml_str.contains("[github]"));
        assert!(toml_str.contains("[gitlab]"));
        assert!(toml_str.contains("[telegram]"));
        assert!(!toml_str.contains("[clickup]"));
        assert!(!toml_str.contains("[jira]"));
        assert!(!toml_str.contains("[yougile]"));

        // Parse back
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert!(parsed.github.is_some());
        assert!(parsed.gitlab.is_some());
    }

    #[test]
    fn test_contexts_and_active_context() {
        let mut config = Config::default();
        config.contexts.insert(
            "dashboard".to_string(),
            ContextConfig {
                github: Some(GitHubConfig {
                    owner: "meteora-pro".to_string(),
                    repo: "my-project".to_string(),
                    base_url: None,
                }),
                clickup: Some(ClickUpConfig {
                    list_id: "abc123".to_string(),
                    team_id: None,
                }),
                ..Default::default()
            },
        );

        let names = config.context_names();
        assert_eq!(names, vec!["dashboard".to_string()]);

        config.set_active_context("dashboard").unwrap();
        assert_eq!(
            config.resolve_active_context_name(),
            Some("dashboard".to_string())
        );
    }

    #[test]
    fn test_context_names_include_legacy_default() {
        let mut config = Config {
            github: Some(GitHubConfig {
                owner: "legacy-owner".to_string(),
                repo: "legacy-repo".to_string(),
                base_url: None,
            }),
            ..Default::default()
        };
        config
            .contexts
            .insert("workspace".to_string(), ContextConfig::default());

        assert_eq!(
            config.context_names(),
            vec!["default".to_string(), "workspace".to_string()]
        );
    }

    #[test]
    fn test_get_context_prefers_explicit_default_over_legacy() {
        let mut config = Config {
            github: Some(GitHubConfig {
                owner: "legacy-owner".to_string(),
                repo: "legacy-repo".to_string(),
                base_url: None,
            }),
            ..Default::default()
        };
        config.contexts.insert(
            Config::DEFAULT_CONTEXT_NAME.to_string(),
            ContextConfig {
                github: Some(GitHubConfig {
                    owner: "explicit-owner".to_string(),
                    repo: "explicit-repo".to_string(),
                    base_url: None,
                }),
                ..Default::default()
            },
        );

        let default_ctx = config.get_context(Config::DEFAULT_CONTEXT_NAME).unwrap();
        let gh = default_ctx.github.unwrap();
        assert_eq!(gh.owner, "explicit-owner");
        assert_eq!(gh.repo, "explicit-repo");
    }

    #[test]
    fn test_resolve_active_context_fallbacks() {
        let mut config = Config {
            active_context: Some("missing".to_string()),
            github: Some(GitHubConfig {
                owner: "legacy-owner".to_string(),
                repo: "legacy-repo".to_string(),
                base_url: None,
            }),
            ..Default::default()
        };
        config
            .contexts
            .insert("beta".to_string(), ContextConfig::default());
        config
            .contexts
            .insert("alpha".to_string(), ContextConfig::default());

        assert_eq!(
            config.resolve_active_context_name(),
            Some("default".to_string())
        );

        config.github = None;
        assert_eq!(
            config.resolve_active_context_name(),
            Some("alpha".to_string())
        );
    }

    #[test]
    fn test_set_active_context_unknown_context_errors() {
        let mut config = Config::default();
        let result = config.set_active_context("missing");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown context"));
    }

    #[test]
    fn test_context_config_configured_providers() {
        let context = ContextConfig {
            github: Some(GitHubConfig {
                owner: "owner".to_string(),
                repo: "repo".to_string(),
                base_url: None,
            }),
            jira: Some(JiraConfig {
                url: "https://jira.example.com".to_string(),
                project_key: "DEV".to_string(),
                email: "dev@example.com".to_string(),
            }),
            yougile: Some(YouGileConfig {
                url: default_yougile_url(),
                board_id: "board-2".to_string(),
            }),
            ..Default::default()
        };

        let providers = context.configured_providers();
        assert_eq!(providers, vec!["github", "jira", "yougile"]);
        assert!(context.has_any_provider());
    }

    // =========================================================================
    // ProxyMcpServerConfig tests
    // =========================================================================

    #[test]
    fn test_proxy_mcp_server_config_defaults() {
        let toml_str = r#"
            [[proxy_mcp_servers]]
            name = "my-server"
            url = "https://example.com/mcp"
        "#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.proxy_mcp_servers.len(), 1);

        let proxy = &config.proxy_mcp_servers[0];
        assert_eq!(proxy.name, "my-server");
        assert_eq!(proxy.url, "https://example.com/mcp");
        assert_eq!(proxy.auth_type, "none");
        assert_eq!(proxy.transport, "sse");
        assert!(proxy.token_key.is_none());
        assert!(proxy.tool_prefix.is_none());
    }

    #[test]
    fn test_proxy_mcp_server_config_full() {
        let toml_str = r#"
            [[proxy_mcp_servers]]
            name = "devboy-cloud"
            url = "https://app.devboy.pro/api/mcp"
            auth_type = "bearer"
            token_key = "devboy-cloud.token"
            tool_prefix = "cloud"
            transport = "streamable-http"
        "#;

        let config: Config = toml::from_str(toml_str).unwrap();
        let proxy = &config.proxy_mcp_servers[0];

        assert_eq!(proxy.name, "devboy-cloud");
        assert_eq!(proxy.auth_type, "bearer");
        assert_eq!(proxy.token_key.as_deref(), Some("devboy-cloud.token"));
        assert_eq!(proxy.tool_prefix.as_deref(), Some("cloud"));
        assert_eq!(proxy.transport, "streamable-http");
    }

    #[test]
    fn test_proxy_mcp_server_config_oauth2_full() {
        let toml_str = r#"
            [[proxy_mcp_servers]]
            name = "devboy-cloud"
            url = "https://app.devboy.pro/api/mcp"
            auth_type = "oauth2"
            transport = "streamable-http"

            [proxy_mcp_servers.oauth]
            client_id = "cli-abc123"
            scopes = ["mcp:read", "mcp:write"]
        "#;

        let config: Config = toml::from_str(toml_str).unwrap();
        let proxy = &config.proxy_mcp_servers[0];
        assert_eq!(proxy.auth_type, "oauth2");
        let oauth = proxy.oauth.as_ref().expect("oauth block should parse");
        assert_eq!(oauth.client_id.as_deref(), Some("cli-abc123"));
        assert_eq!(
            oauth.scopes,
            Some(vec!["mcp:read".to_string(), "mcp:write".to_string()])
        );
        assert!(oauth.authorization_server.is_none());
    }

    #[test]
    fn test_proxy_mcp_server_config_oauth2_minimal() {
        // Minimal oauth2 config: only `auth_type`, no [oauth] block — discovery
        // (RFC 9728/8414) + dynamic registration (RFC 7591) fill the rest at login.
        let toml_str = r#"
            [[proxy_mcp_servers]]
            name = "srv"
            url = "https://example.com/mcp"
            auth_type = "oauth2"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let proxy = &config.proxy_mcp_servers[0];
        assert_eq!(proxy.auth_type, "oauth2");
        assert!(proxy.oauth.is_none());
    }

    #[test]
    fn test_proxy_mcp_server_config_multiple() {
        let toml_str = r#"
            [[proxy_mcp_servers]]
            name = "server1"
            url = "https://s1.example.com/mcp"

            [[proxy_mcp_servers]]
            name = "server2"
            url = "https://s2.example.com/mcp"
            auth_type = "api_key"
            token_key = "s2.token"
        "#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.proxy_mcp_servers.len(), 2);
        assert_eq!(config.proxy_mcp_servers[0].name, "server1");
        assert_eq!(config.proxy_mcp_servers[1].name, "server2");
        assert_eq!(config.proxy_mcp_servers[1].auth_type, "api_key");
    }

    #[test]
    fn test_proxy_mcp_server_config_serialization_roundtrip() {
        let config = Config {
            proxy_mcp_servers: vec![ProxyMcpServerConfig {
                name: "test".to_string(),
                url: "https://test.com/mcp".to_string(),
                auth_type: "bearer".to_string(),
                token_key: Some("test.token".to_string()),
                tool_prefix: Some("tst".to_string()),
                transport: "streamable-http".to_string(),
                routing: None,
                oauth: None,
            }],
            ..Default::default()
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(toml_str.contains("[[proxy_mcp_servers]]"));
        assert!(toml_str.contains("name = \"test\""));

        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.proxy_mcp_servers.len(), 1);
        assert_eq!(parsed.proxy_mcp_servers[0].name, "test");
        assert_eq!(parsed.proxy_mcp_servers[0].transport, "streamable-http");
    }

    #[test]
    fn test_proxy_mcp_server_config_skips_none_fields_in_serialization() {
        let config = Config {
            proxy_mcp_servers: vec![ProxyMcpServerConfig {
                name: "minimal".to_string(),
                url: "https://test.com/mcp".to_string(),
                auth_type: "none".to_string(),
                token_key: None,
                tool_prefix: None,
                transport: "sse".to_string(),
                routing: None,
                oauth: None,
            }],
            ..Default::default()
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(!toml_str.contains("token_key"));
        assert!(!toml_str.contains("tool_prefix"));
    }

    #[test]
    fn test_empty_proxy_mcp_servers_not_serialized() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(!toml_str.contains("proxy_mcp_servers"));
    }

    // =========================================================================
    // ProxyConfig (routing, secrets, telemetry) tests
    // =========================================================================

    #[test]
    fn test_proxy_config_default_is_default() {
        let cfg = ProxyConfig::default();
        assert!(cfg.is_default());
    }

    #[test]
    fn test_default_proxy_section_not_serialized() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(!toml_str.contains("[proxy]"));
        assert!(!toml_str.contains("[proxy.routing]"));
    }

    #[test]
    fn test_routing_strategy_default_is_remote() {
        let strategy = RoutingStrategy::default();
        assert_eq!(strategy, RoutingStrategy::Remote);
    }

    #[test]
    fn test_routing_strategy_parse_tolerates_formats() {
        assert_eq!(
            RoutingStrategy::parse("remote"),
            Some(RoutingStrategy::Remote)
        );
        assert_eq!(
            RoutingStrategy::parse(" REMOTE "),
            Some(RoutingStrategy::Remote)
        );
        assert_eq!(
            RoutingStrategy::parse("local"),
            Some(RoutingStrategy::Local)
        );
        assert_eq!(
            RoutingStrategy::parse("local-first"),
            Some(RoutingStrategy::LocalFirst)
        );
        assert_eq!(
            RoutingStrategy::parse("local_first"),
            Some(RoutingStrategy::LocalFirst)
        );
        assert_eq!(
            RoutingStrategy::parse("remote-first"),
            Some(RoutingStrategy::RemoteFirst)
        );
        assert_eq!(RoutingStrategy::parse("unknown"), None);
    }

    #[test]
    fn test_routing_strategy_serde_kebab_case() {
        let toml_str = r#"
            [proxy.routing]
            strategy = "local-first"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.proxy.routing.strategy, RoutingStrategy::LocalFirst);

        // Round-trip
        let serialized = toml::to_string_pretty(&config).unwrap();
        assert!(serialized.contains("strategy = \"local-first\""));
    }

    #[test]
    fn test_proxy_routing_strategy_for_picks_first_matching_override() {
        let routing = ProxyRoutingConfig {
            strategy: RoutingStrategy::Remote,
            fallback_on_error: true,
            tool_overrides: vec![
                ProxyToolRule {
                    pattern: "create_*".to_string(),
                    strategy: RoutingStrategy::Remote,
                },
                ProxyToolRule {
                    pattern: "get_*".to_string(),
                    strategy: RoutingStrategy::LocalFirst,
                },
                ProxyToolRule {
                    pattern: "*".to_string(),
                    strategy: RoutingStrategy::Local,
                },
            ],
        };

        assert_eq!(
            routing.strategy_for("create_issue"),
            RoutingStrategy::Remote
        );
        assert_eq!(
            routing.strategy_for("get_issues"),
            RoutingStrategy::LocalFirst
        );
        assert_eq!(
            routing.strategy_for("anything_else"),
            RoutingStrategy::Local
        );
    }

    #[test]
    fn test_proxy_routing_strategy_for_falls_back_to_global() {
        let routing = ProxyRoutingConfig {
            strategy: RoutingStrategy::Remote,
            fallback_on_error: true,
            tool_overrides: vec![ProxyToolRule {
                pattern: "get_*".to_string(),
                strategy: RoutingStrategy::LocalFirst,
            }],
        };

        assert_eq!(
            routing.strategy_for("unrelated_tool"),
            RoutingStrategy::Remote
        );
    }

    #[test]
    fn test_proxy_routing_merged_with_override_wins() {
        let global = ProxyRoutingConfig {
            strategy: RoutingStrategy::Remote,
            fallback_on_error: true,
            tool_overrides: vec![ProxyToolRule {
                pattern: "get_*".to_string(),
                strategy: RoutingStrategy::LocalFirst,
            }],
        };
        let override_cfg = ProxyRoutingOverride {
            strategy: Some(RoutingStrategy::Local),
            fallback_on_error: Some(false),
            tool_overrides: Some(vec![ProxyToolRule {
                pattern: "create_*".to_string(),
                strategy: RoutingStrategy::Remote,
            }]),
        };

        let merged = global.merged_with(Some(&override_cfg));
        assert_eq!(merged.strategy, RoutingStrategy::Local);
        assert!(!merged.fallback_on_error);
        // override tool_overrides come first, global rules append
        assert_eq!(merged.tool_overrides.len(), 2);
        assert_eq!(merged.tool_overrides[0].pattern, "create_*");
        assert_eq!(merged.tool_overrides[1].pattern, "get_*");
    }

    #[test]
    fn test_proxy_routing_merged_with_partial_override_preserves_unset_fields() {
        // Reviewer concern: "a per-server block that only sets strategy must not reset
        // fallback_on_error / tool_overrides to defaults."
        let global = ProxyRoutingConfig {
            strategy: RoutingStrategy::Remote,
            fallback_on_error: false, // deliberately non-default
            tool_overrides: vec![ProxyToolRule {
                pattern: "get_*".to_string(),
                strategy: RoutingStrategy::LocalFirst,
            }],
        };
        // Override only tweaks `strategy`; everything else must inherit from global.
        let override_cfg = ProxyRoutingOverride {
            strategy: Some(RoutingStrategy::Local),
            fallback_on_error: None,
            tool_overrides: None,
        };

        let merged = global.merged_with(Some(&override_cfg));
        assert_eq!(merged.strategy, RoutingStrategy::Local);
        assert!(
            !merged.fallback_on_error,
            "fallback_on_error must inherit from global, not snap to default"
        );
        assert_eq!(
            merged.tool_overrides.len(),
            1,
            "tool_overrides must inherit from global when override omits them"
        );
        assert_eq!(merged.tool_overrides[0].pattern, "get_*");
    }

    #[test]
    fn test_proxy_routing_merged_with_none_returns_clone() {
        let global = ProxyRoutingConfig {
            strategy: RoutingStrategy::LocalFirst,
            ..Default::default()
        };
        let merged = global.merged_with(None);
        assert_eq!(merged.strategy, RoutingStrategy::LocalFirst);
    }

    #[test]
    fn test_proxy_secrets_default_cache_ttl() {
        let s = ProxySecretsConfig::default();
        assert_eq!(s.cache_ttl_secs, 300);
        assert!(s.is_default());
    }

    #[test]
    fn test_proxy_telemetry_defaults() {
        let t = ProxyTelemetryConfig::default();
        assert!(t.enabled);
        assert_eq!(t.batch_size, 100);
        assert_eq!(t.batch_interval_secs, 30);
        assert!(t.endpoint.is_none());
        assert!(t.is_default());
    }

    #[test]
    fn test_proxy_toml_parse_full() {
        let toml_str = r#"
            [proxy.routing]
            strategy = "local-first"
            fallback_on_error = false

            [[proxy.routing.tool_overrides]]
            pattern = "create_*"
            strategy = "remote"

            [proxy.secrets]
            cache_ttl_secs = 120

            [proxy.telemetry]
            enabled = true
            batch_size = 50
            batch_interval_secs = 10
            endpoint = "https://telemetry.example.com/api/events"
        "#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.proxy.routing.strategy, RoutingStrategy::LocalFirst);
        assert!(!config.proxy.routing.fallback_on_error);
        assert_eq!(config.proxy.routing.tool_overrides.len(), 1);
        assert_eq!(config.proxy.secrets.cache_ttl_secs, 120);
        assert_eq!(config.proxy.telemetry.batch_size, 50);
        assert_eq!(
            config.proxy.telemetry.endpoint.as_deref(),
            Some("https://telemetry.example.com/api/events")
        );
    }

    #[test]
    fn test_proxy_mcp_server_per_server_routing_override() {
        let toml_str = r#"
            [[proxy_mcp_servers]]
            name = "cloud"
            url = "https://api.example.com/mcp"

            [proxy_mcp_servers.routing]
            strategy = "local-first"
        "#;

        let config: Config = toml::from_str(toml_str).unwrap();
        let server = &config.proxy_mcp_servers[0];
        let override_cfg = server.routing.as_ref().expect("override present");
        // Only `strategy` was set — other fields must stay `None` so they inherit.
        assert_eq!(override_cfg.strategy, Some(RoutingStrategy::LocalFirst));
        assert!(override_cfg.fallback_on_error.is_none());
        assert!(override_cfg.tool_overrides.is_none());
    }

    // =========================================================================
    // Config::set / Config::get for `proxy.*` paths
    // =========================================================================

    #[test]
    fn test_set_get_proxy_routing_strategy_roundtrip() {
        let mut cfg = Config::default();
        cfg.set("proxy.routing.strategy", "local-first").unwrap();
        assert_eq!(cfg.proxy.routing.strategy, RoutingStrategy::LocalFirst);
        assert_eq!(
            cfg.get("proxy.routing.strategy").unwrap().as_deref(),
            Some("local-first")
        );

        cfg.set("proxy.routing.strategy", "remote").unwrap();
        assert_eq!(
            cfg.get("proxy.routing.strategy").unwrap().as_deref(),
            Some("remote")
        );
    }

    #[test]
    fn test_set_proxy_routing_strategy_rejects_garbage() {
        let mut cfg = Config::default();
        let err = cfg
            .set("proxy.routing.strategy", "teleport")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Invalid routing strategy"));
    }

    #[test]
    fn test_set_proxy_routing_booleans_accept_many_forms() {
        let mut cfg = Config::default();
        for truthy in ["true", "TRUE", "1", "yes", "on"] {
            cfg.set("proxy.routing.fallback_on_error", truthy).unwrap();
            assert!(cfg.proxy.routing.fallback_on_error);
        }
        for falsy in ["false", "0", "no", "off"] {
            cfg.set("proxy.routing.fallback_on_error", falsy).unwrap();
            assert!(!cfg.proxy.routing.fallback_on_error);
        }
    }

    #[test]
    fn test_set_proxy_secrets_cache_ttl() {
        let mut cfg = Config::default();
        cfg.set("proxy.secrets.cache_ttl_secs", "120").unwrap();
        assert_eq!(cfg.proxy.secrets.cache_ttl_secs, 120);
        assert_eq!(
            cfg.get("proxy.secrets.cache_ttl_secs").unwrap().as_deref(),
            Some("120")
        );

        assert!(cfg.set("proxy.secrets.cache_ttl_secs", "-5").is_err());
    }

    #[test]
    fn test_set_proxy_telemetry_endpoint_and_clear() {
        let mut cfg = Config::default();
        cfg.set("proxy.telemetry.endpoint", "https://example.com/t")
            .unwrap();
        assert_eq!(
            cfg.proxy.telemetry.endpoint.as_deref(),
            Some("https://example.com/t")
        );

        // Empty string clears the field — symmetric with how serde skips it.
        cfg.set("proxy.telemetry.endpoint", "").unwrap();
        assert!(cfg.proxy.telemetry.endpoint.is_none());
    }

    #[test]
    fn test_set_proxy_telemetry_endpoint_rejects_garbage() {
        let mut cfg = Config::default();
        for bad in [
            "not-a-url",
            "ftp://host.example.com",
            "//example.com",
            "https://",
            "http:// space.example.com",
            // whitespace anywhere — path, query, trailing — must be rejected too
            "https://example.com/a b",
            "https://example.com/path?key=a b",
            "https://example.com/\tpath",
            "https://example.com/ ",
        ] {
            match cfg.set("proxy.telemetry.endpoint", bad) {
                Ok(()) => panic!("expected reject for {}", bad),
                Err(e) => assert!(
                    e.to_string().contains("Invalid URL"),
                    "bad={}, err={}",
                    bad,
                    e
                ),
            }
        }
    }

    #[test]
    fn test_set_proxy_telemetry_endpoint_accepts_common_forms() {
        let mut cfg = Config::default();
        for good in [
            "https://app.example.com/api/telemetry/tool-invocations",
            "http://localhost:4335/api/telemetry/tool-invocations",
            "https://example.com",
            "http://10.0.0.1:8080/",
        ] {
            cfg.set("proxy.telemetry.endpoint", good)
                .unwrap_or_else(|e| panic!("expected accept for {}: {}", good, e));
        }
    }

    // =========================================================================
    // Config::validate() — run-time checks applied on load_from() too
    // =========================================================================

    #[test]
    fn test_validate_rejects_bad_endpoint_from_toml() {
        // A user hand-editing TOML can sneak invalid endpoints past `set()`; ensure
        // `Config::load_from` (via `validate()`) still catches them.
        let toml_str = r#"
            [proxy.telemetry]
            endpoint = "not-a-url"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let err = config
            .validate()
            .expect_err("expected validation to fail for 'not-a-url'");
        assert!(
            err.to_string().contains("Invalid URL"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_validate_accepts_empty_endpoint_as_absent() {
        // Current TOML serde path keeps `endpoint = None` when the field is skipped.
        // Validation must not fail in this common case.
        let config = Config::default();
        config.validate().expect("default config validates");
    }

    #[test]
    fn test_sanitize_normalizes_empty_endpoint_to_none() {
        // Hand-edited TOML may set `endpoint = ""`; serde keeps it as Some("").
        // `sanitize` must collapse it to None so it stops short-circuiting validation
        // and later tricking the telemetry pipeline into using an invalid URL.
        let mut config: Config = toml::from_str(
            r#"
[proxy.telemetry]
endpoint = ""
"#,
        )
        .unwrap();
        assert_eq!(config.proxy.telemetry.endpoint.as_deref(), Some(""));
        config.sanitize();
        assert!(config.proxy.telemetry.endpoint.is_none());
        config.validate().expect("sanitized config must validate");
    }

    #[test]
    fn test_load_from_sanitizes_empty_endpoint() {
        use std::fs::write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write(
            &path,
            r#"
[proxy.telemetry]
endpoint = ""
"#,
        )
        .unwrap();

        let cfg = Config::load_from(&path).expect("empty endpoint must be normalised on load");
        assert!(
            cfg.proxy.telemetry.endpoint.is_none(),
            "empty string must load as None, not Some(\"\")"
        );
    }

    #[test]
    fn test_validate_rejects_naked_empty_string_endpoint() {
        // Skip sanitize: a caller that set the value manually must see the bad-URL
        // error rather than silent acceptance.
        let mut config = Config::default();
        config.proxy.telemetry.endpoint = Some(String::new());
        let err = config
            .validate()
            .expect_err("empty string must be rejected if caller skipped sanitize");
        assert!(
            err.to_string().contains("Invalid URL"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_load_from_runs_validation() {
        use std::fs::write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write(
            &path,
            r#"
[proxy.telemetry]
endpoint = "ftp://wrong-scheme.example.com"
"#,
        )
        .unwrap();

        let err = Config::load_from(&path).expect_err("must reject bad URL from file");
        assert!(
            err.to_string().contains("Invalid URL"),
            "unexpected error: {}",
            err
        );
    }

    // =========================================================================
    // deny_unknown_fields — typos surface on load, not silently default away
    // =========================================================================

    #[test]
    fn test_unknown_field_in_proxy_routing_rejected() {
        let toml_str = r#"
            [proxy.routing]
            strategy = "local-first"
            startegy = "typo"
        "#;
        let err = toml::from_str::<Config>(toml_str)
            .expect_err("expected parse error for typo 'startegy'");
        let msg = err.to_string();
        assert!(
            msg.contains("startegy") || msg.contains("unknown field"),
            "unexpected error: {}",
            msg
        );
    }

    #[test]
    fn test_unknown_field_in_proxy_secrets_rejected() {
        let toml_str = r#"
            [proxy.secrets]
            cache_ttl_secs = 60
            chache_ttl_secs = 120
        "#;
        let err = toml::from_str::<Config>(toml_str).expect_err("typo must fail");
        assert!(
            err.to_string().contains("chache_ttl_secs")
                || err.to_string().contains("unknown field")
        );
    }

    #[test]
    fn test_unknown_field_in_proxy_telemetry_rejected() {
        let toml_str = r#"
            [proxy.telemetry]
            enabled = true
            endpooint = "https://example.com"
        "#;
        let err = toml::from_str::<Config>(toml_str).expect_err("typo must fail");
        assert!(err.to_string().contains("endpooint") || err.to_string().contains("unknown field"));
    }

    #[test]
    fn test_unknown_field_in_tool_override_rejected() {
        let toml_str = r#"
            [[proxy.routing.tool_overrides]]
            pattern = "get_*"
            strategy = "local"
            unknown = 1
        "#;
        let err = toml::from_str::<Config>(toml_str).expect_err("typo in rule must fail");
        assert!(err.to_string().contains("unknown"));
    }

    #[test]
    fn test_unknown_top_level_proxy_section_rejected() {
        // E.g. user writes [proxy.typo] — we want this to fail, not silently ignore.
        let toml_str = r#"
            [proxy.typo]
            foo = 1
        "#;
        let err = toml::from_str::<Config>(toml_str).expect_err("unknown section must fail");
        let msg = err.to_string();
        assert!(msg.contains("typo") || msg.contains("unknown field"));
    }

    #[test]
    fn test_load_from_accepts_valid_proxy_config() {
        use std::fs::write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write(
            &path,
            r#"
[proxy.routing]
strategy = "local-first"

[proxy.telemetry]
endpoint = "https://app.example.com/api/telemetry/tool-invocations"
"#,
        )
        .unwrap();

        let cfg = Config::load_from(&path).expect("valid config must load");
        assert_eq!(cfg.proxy.routing.strategy, RoutingStrategy::LocalFirst);
        assert_eq!(
            cfg.proxy.telemetry.endpoint.as_deref(),
            Some("https://app.example.com/api/telemetry/tool-invocations")
        );
    }

    #[test]
    fn test_set_proxy_telemetry_batch_fields() {
        let mut cfg = Config::default();
        cfg.set("proxy.telemetry.batch_size", "50").unwrap();
        cfg.set("proxy.telemetry.batch_interval_secs", "15")
            .unwrap();
        cfg.set("proxy.telemetry.offline_queue_max", "2000")
            .unwrap();

        assert_eq!(cfg.proxy.telemetry.batch_size, 50);
        assert_eq!(cfg.proxy.telemetry.batch_interval_secs, 15);
        assert_eq!(cfg.proxy.telemetry.offline_queue_max, 2000);
    }

    #[test]
    fn test_unknown_proxy_section_or_field_errors() {
        let mut cfg = Config::default();
        assert!(cfg.set("proxy.unknown.foo", "1").is_err());
        assert!(cfg.set("proxy.routing.unknown", "1").is_err());
        assert!(cfg.get("proxy.unknown.foo").is_err());
        assert!(cfg.get("proxy.routing.unknown").is_err());
    }

    #[test]
    fn test_four_part_key_rejected() {
        let mut cfg = Config::default();
        assert!(cfg.set("proxy.routing.strategy.extra", "local").is_err());
    }

    // =========================================================================
    // Config: backward compat
    // =========================================================================

    #[test]
    fn test_legacy_config_without_proxy_section_still_parses() {
        // A config written before this feature must keep deserializing cleanly.
        let toml_str = r#"
            [github]
            owner = "me"
            repo = "repo"

            [[proxy_mcp_servers]]
            name = "cloud"
            url = "https://api.example.com/mcp"
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.github.unwrap().owner, "me");
        assert_eq!(config.proxy_mcp_servers.len(), 1);
        assert!(config.proxy.is_default());
    }

    // =========================================================================
    // glob matcher tests
    // =========================================================================

    #[test]
    fn test_matches_glob_exact() {
        assert!(matches_glob("get_issues", "get_issues"));
        assert!(!matches_glob("get_issues", "get_issue"));
        assert!(!matches_glob("get_issues", "gets_issues"));
    }

    #[test]
    fn test_matches_glob_star_alone() {
        assert!(matches_glob("*", ""));
        assert!(matches_glob("*", "anything"));
        assert!(matches_glob("*", "create_merge_request"));
    }

    #[test]
    fn test_matches_glob_prefix() {
        assert!(matches_glob("get_*", "get_issues"));
        assert!(matches_glob("get_*", "get_"));
        assert!(!matches_glob("get_*", "create_issues"));
    }

    #[test]
    fn test_matches_glob_suffix() {
        assert!(matches_glob("*_issue", "create_issue"));
        assert!(matches_glob("*_issue", "_issue"));
        assert!(!matches_glob("*_issue", "create_issues"));
    }

    #[test]
    fn test_matches_glob_contains() {
        assert!(matches_glob("*issue*", "get_issues"));
        assert!(matches_glob("*issue*", "issue"));
        assert!(!matches_glob("*issue*", "merge_request"));
    }

    #[test]
    fn test_matches_glob_multiple_wildcards() {
        assert!(matches_glob("get_*_by_*", "get_issue_by_id"));
        assert!(matches_glob("get_*_by_*", "get_user_by_email"));
        assert!(!matches_glob("get_*_by_*", "get_issue"));
        assert!(!matches_glob("get_*_by_*", "create_issue_by_id"));
    }

    #[test]
    fn test_matches_glob_collapses_double_star() {
        assert!(matches_glob("get_**_issue", "get_new_issue"));
    }

    // =========================================================================
    // BuiltinToolsConfig tests
    // =========================================================================

    #[test]
    fn test_builtin_tools_config_default_is_empty() {
        let config = BuiltinToolsConfig::default();
        assert!(config.is_empty());
        assert!(config.validate().is_ok());
        assert!(config.is_tool_allowed("get_issues"));
    }

    #[test]
    fn test_builtin_tools_disabled_mode() {
        let config = BuiltinToolsConfig {
            disabled: vec!["get_issues".to_string(), "create_issue".to_string()],
            enabled: vec![],
        };
        assert!(!config.is_empty());
        assert!(config.validate().is_ok());
        assert!(!config.is_tool_allowed("get_issues"));
        assert!(!config.is_tool_allowed("create_issue"));
        assert!(config.is_tool_allowed("get_merge_requests"));
        assert!(config.is_tool_allowed("list_contexts"));
    }

    #[test]
    fn test_builtin_tools_enabled_mode() {
        let config = BuiltinToolsConfig {
            disabled: vec![],
            enabled: vec![
                "list_contexts".to_string(),
                "use_context".to_string(),
                "get_current_context".to_string(),
            ],
        };
        assert!(!config.is_empty());
        assert!(config.validate().is_ok());
        assert!(config.is_tool_allowed("list_contexts"));
        assert!(config.is_tool_allowed("use_context"));
        assert!(!config.is_tool_allowed("get_issues"));
        assert!(!config.is_tool_allowed("create_issue"));
    }

    #[test]
    fn test_builtin_tools_mutually_exclusive_error() {
        let config = BuiltinToolsConfig {
            disabled: vec!["get_issues".to_string()],
            enabled: vec!["list_contexts".to_string()],
        };
        assert!(config.validate().is_err());
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("mutually exclusive"));
    }

    #[test]
    fn test_builtin_tools_toml_parsing_disabled() {
        let toml_str = r#"
            [builtin_tools]
            disabled = ["get_issues", "create_issue"]
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.builtin_tools.is_empty());
        assert_eq!(config.builtin_tools.disabled.len(), 2);
        assert!(config.builtin_tools.enabled.is_empty());
    }

    #[test]
    fn test_builtin_tools_toml_parsing_enabled() {
        let toml_str = r#"
            [builtin_tools]
            enabled = ["list_contexts", "use_context", "get_current_context"]
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.builtin_tools.enabled.len(), 3);
        assert!(config.builtin_tools.disabled.is_empty());
    }

    #[test]
    fn test_builtin_tools_not_serialized_when_empty() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(!toml_str.contains("builtin_tools"));
    }

    #[test]
    fn test_builtin_tools_serialization_roundtrip() {
        let config = Config {
            builtin_tools: BuiltinToolsConfig {
                disabled: vec!["get_issues".to_string(), "create_issue".to_string()],
                enabled: vec![],
            },
            ..Default::default()
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(toml_str.contains("[builtin_tools]"));
        assert!(toml_str.contains("get_issues"));

        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.builtin_tools.disabled.len(), 2);
    }

    #[test]
    fn test_builtin_tools_warn_unknown_with_unknown_names() {
        let known = &["get_issues", "create_issue"];
        let config = BuiltinToolsConfig {
            disabled: vec!["get_issues".to_string(), "nonexistent_tool".to_string()],
            enabled: vec![],
        };
        // Should not panic, logs a warning for nonexistent_tool
        config.warn_unknown_tools(known);
    }

    #[test]
    fn test_builtin_tools_warn_unknown_all_known() {
        let known = &["get_issues", "create_issue"];
        let config = BuiltinToolsConfig {
            disabled: vec!["get_issues".to_string()],
            enabled: vec![],
        };
        // All names are known — no warnings expected
        config.warn_unknown_tools(known);
    }

    #[test]
    fn test_builtin_tools_warn_unknown_in_enabled_list() {
        let known = &["get_issues", "create_issue"];
        let config = BuiltinToolsConfig {
            disabled: vec![],
            enabled: vec!["get_issues".to_string(), "unknown_tool".to_string()],
        };
        // Verify that the enabled list is also checked
        config.warn_unknown_tools(known);
    }

    #[test]
    fn test_builtin_tools_warn_unknown_empty_config() {
        let known = &["get_issues"];
        let config = BuiltinToolsConfig::default();
        // Empty config — nothing to check
        config.warn_unknown_tools(known);
    }
}

#[cfg(test)]
mod config_dir_override_tests {
    use super::*;

    /// The override replaces the directory outright. Appending
    /// `devboy-tools` to it would surprise a caller who pointed it at
    /// a scratch directory and then looked for their file there.
    #[test]
    fn the_override_is_used_verbatim() {
        temp_env::with_var(CONFIG_DIR_ENV, Some("/tmp/devboy-scratch"), || {
            assert_eq!(
                Config::config_dir().unwrap(),
                PathBuf::from("/tmp/devboy-scratch")
            );
            assert_eq!(
                Config::config_path().unwrap(),
                PathBuf::from("/tmp/devboy-scratch").join(CONFIG_FILE_NAME)
            );
        });
    }

    /// An empty value is a variable someone meant to unset, not a
    /// request to use the current directory.
    #[test]
    fn an_empty_override_falls_through_to_the_platform_default() {
        let platform = temp_env::with_var_unset(CONFIG_DIR_ENV, || Config::config_dir().unwrap());

        for blank in ["", "   "] {
            temp_env::with_var(CONFIG_DIR_ENV, Some(blank), || {
                assert_eq!(
                    Config::config_dir().unwrap(),
                    platform,
                    "a blank override must not change where the config lives"
                );
            });
        }
    }

    /// Without the variable, nothing about the existing behaviour
    /// changes — the override is an addition, not a redirection.
    #[test]
    fn the_platform_default_still_ends_in_the_product_directory() {
        let path = temp_env::with_var_unset(CONFIG_DIR_ENV, || Config::config_dir().unwrap());
        assert!(path.ends_with(CONFIG_DIR_NAME), "{}", path.display());
    }
}
