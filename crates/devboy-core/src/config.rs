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

/// Config file name.
const CONFIG_FILE_NAME: &str = "config.toml";

/// Config directory name.
const CONFIG_DIR_NAME: &str = "devboy-tools";

// =============================================================================
// Configuration structures
// =============================================================================

/// Main configuration structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// GitHub configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubConfig>,

    /// GitLab configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gitlab: Option<GitLabConfig>,

    /// ClickUp configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clickup: Option<ClickUpConfig>,

    /// Jira configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jira: Option<JiraConfig>,

    /// Fireflies.ai configuration (meeting notes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fireflies: Option<FirefliesConfig>,

    /// Confluence self-hosted configuration (knowledge base)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confluence: Option<ConfluenceConfig>,

    /// Slack configuration (messenger)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<SlackConfig>,

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
}

/// Configuration for an upstream MCP server to proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyMcpServerConfig {
    /// Server name (used as tool prefix if tool_prefix not set)
    pub name: String,
    /// Server URL (SSE or Streamable HTTP endpoint)
    pub url: String,
    /// Auth type: "bearer", "api_key", "none"
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
}

fn default_transport_sse() -> String {
    "sse".to_string()
}

fn default_auth_none() -> String {
    "none".to_string()
}

/// Per-context provider configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextConfig {
    /// GitHub configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<GitHubConfig>,

    /// GitLab configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gitlab: Option<GitLabConfig>,

    /// ClickUp configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clickup: Option<ClickUpConfig>,

    /// Jira configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jira: Option<JiraConfig>,

    /// Fireflies.ai configuration (meeting notes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fireflies: Option<FirefliesConfig>,

    /// Confluence self-hosted configuration (knowledge base)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confluence: Option<ConfluenceConfig>,

    /// Slack configuration (messenger)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<SlackConfig>,
}

/// GitHub provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubConfig {
    /// Repository owner (user or organization)
    pub owner: String,
    /// Repository name
    pub repo: String,
    /// GitHub API base URL (for GitHub Enterprise)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// GitLab provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabConfig {
    /// GitLab instance URL
    #[serde(default = "default_gitlab_url")]
    pub url: String,
    /// Project ID (numeric or path)
    pub project_id: String,
}

/// ClickUp provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpConfig {
    /// ClickUp list ID
    pub list_id: String,
    /// ClickUp team (workspace) ID — required for custom task ID resolution
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
}

/// Jira provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraConfig {
    /// Jira instance URL
    pub url: String,
    /// Project key (e.g., "PROJ")
    pub project_key: String,
    /// User email (required for Jira auth)
    pub email: String,
}

/// Fireflies.ai provider configuration (meeting notes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirefliesConfig {
    // API key is stored in OS keychain (key: "fireflies.token")
    // No fields needed — config just enables the provider
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Confluence Config.
pub struct ConfluenceConfig {
    /// Confluence base URL, e.g. `https://wiki.example.com`.
    pub base_url: String,
    /// Preferred REST API generation when the instance supports multiple.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
    /// Username/email for basic auth when that auth mode is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Optional default space hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_key: Option<String>,
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
    /// OAuth redirect URI.
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

/// default slack required scopes.
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

    /// Proxy tool matching configuration.
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

/// Proxy tool matching configuration.
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
    /// Strategy.
    pub strategy: Option<RoutingStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Fallback on error.
    pub fallback_on_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Tool overrides.
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
    /// is default.
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
    /// is default.
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
    /// Routing.
    pub routing: ProxyRoutingConfig,

    #[serde(default, skip_serializing_if = "ProxySecretsConfig::is_default")]
    /// Secrets.
    pub secrets: ProxySecretsConfig,

    #[serde(default, skip_serializing_if = "ProxyTelemetryConfig::is_default")]
    /// Telemetry.
    pub telemetry: ProxyTelemetryConfig,
}

impl ProxyConfig {
    /// is default.
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
    pub fn config_dir() -> Result<PathBuf> {
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
            || self.fireflies.is_some()
            || self.confluence.is_some()
            || self.slack.is_some()
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
        if self.confluence.is_some() {
            providers.push("confluence");
        }
        if self.slack.is_some() {
            providers.push("slack");
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
            fireflies: self.fireflies.clone(),
            confluence: self.confluence.clone(),
            slack: self.slack.clone(),
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

        // Three-part paths are reserved for `proxy.*` sections.
        if parts.len() == 3 && parts[0] == "proxy" {
            return self.set_proxy_field(parts[1], parts[2], value);
        }

        if parts.len() != 2 {
            return Err(Error::Config(format!(
                "Invalid config key '{}'. Expected formats: provider.field or proxy.section.field",
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
            "confluence" => {
                let config = self.confluence.get_or_insert_with(|| ConfluenceConfig {
                    base_url: String::new(),
                    api_version: None,
                    username: None,
                    space_key: None,
                });
                match field {
                    "base_url" | "url" => config.base_url = value.to_string(),
                    "api_version" | "api" | "version" => {
                        config.api_version = Some(value.to_string())
                    }
                    "username" | "email" | "user" => config.username = Some(value.to_string()),
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
            _ => {
                return Err(Error::Config(format!("Unknown provider: {}", provider)));
            }
        }

        Ok(())
    }

    /// Get a configuration value by key path.
    ///
    /// Supported key formats:
    /// - `provider.field` — e.g., `github.owner`, `gitlab.url`
    /// - `proxy.{routing|secrets|telemetry}.{field}` — e.g., `proxy.routing.strategy`
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        let parts: Vec<&str> = key.split('.').collect();

        if parts.len() == 3 && parts[0] == "proxy" {
            return self.get_proxy_field(parts[1], parts[2]);
        }

        if parts.len() != 2 {
            return Err(Error::Config(format!(
                "Invalid config key '{}'. Expected formats: provider.field or proxy.section.field",
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
            "confluence" => {
                let Some(config) = &self.confluence else {
                    return Ok(None);
                };
                match field {
                    "base_url" | "url" => Ok(Some(config.base_url.clone())),
                    "api_version" | "api" | "version" => Ok(config.api_version.clone()),
                    "username" | "email" | "user" => Ok(config.username.clone()),
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
            _ => Err(Error::Config(format!("Unknown provider: {}", provider))),
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
            || self.fireflies.is_some()
            || self.confluence.is_some()
            || self.slack.is_some()
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
        if self.confluence.is_some() {
            providers.push("confluence");
        }
        if self.slack.is_some() {
            providers.push("slack");
        }
        providers
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.github.is_none());
        assert!(config.gitlab.is_none());
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

        // When provider config doesn't exist, get returns Ok(None)
        assert_eq!(config.get("github.owner").unwrap(), None);

        // But unknown field on configured provider should error
        config.set("github.owner", "test").unwrap();
        assert!(config.get("github.unknown_field").is_err());
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
    fn test_set_and_get_confluence() {
        let mut config = Config::default();

        config
            .set("confluence.base_url", "https://wiki.example.com")
            .unwrap();
        config.set("confluence.api_version", "v1").unwrap();
        config
            .set("confluence.username", "dev@example.com")
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
            config.get("confluence.api").unwrap(),
            Some("v1".to_string())
        );
        assert_eq!(
            config.get("confluence.username").unwrap(),
            Some("dev@example.com".to_string())
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
    }

    #[test]
    fn test_get_unconfigured_providers() {
        let config = Config::default();

        assert_eq!(config.get("github.owner").unwrap(), None);
        assert_eq!(config.get("gitlab.url").unwrap(), None);
        assert_eq!(config.get("clickup.list_id").unwrap(), None);
        assert_eq!(config.get("jira.url").unwrap(), None);
        assert_eq!(config.get("confluence.base_url").unwrap(), None);
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
            fireflies: None,
            confluence: None,
            slack: None,
            contexts: BTreeMap::new(),
            active_context: None,
            proxy_mcp_servers: Vec::new(),
            builtin_tools: BuiltinToolsConfig::default(),
            format_pipeline: None,
            proxy: ProxyConfig::default(),
            sentry: None,
            remote_config: None,
        };

        let providers = config.configured_providers();
        assert_eq!(providers.len(), 4);
        assert!(providers.contains(&"github"));
        assert!(providers.contains(&"gitlab"));
        assert!(providers.contains(&"clickup"));
        assert!(providers.contains(&"jira"));
        assert!(config.has_any_provider());
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
            fireflies: None,
            confluence: None,
            slack: None,
            contexts: BTreeMap::new(),
            active_context: None,
            proxy_mcp_servers: Vec::new(),
            builtin_tools: BuiltinToolsConfig::default(),
            format_pipeline: None,
            proxy: ProxyConfig::default(),
            sentry: None,
            remote_config: None,
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(toml_str.contains("[github]"));
        assert!(toml_str.contains("[gitlab]"));
        assert!(!toml_str.contains("[clickup]"));
        assert!(!toml_str.contains("[jira]"));

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
            ..Default::default()
        };

        let providers = context.configured_providers();
        assert_eq!(providers, vec!["github", "jira"]);
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
