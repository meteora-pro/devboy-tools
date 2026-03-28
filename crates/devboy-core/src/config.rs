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

fn default_gitlab_url() -> String {
    "https://gitlab.com".to_string()
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

        let config: Config = toml::from_str(&contents)
            .map_err(|e| Error::Config(format!("Failed to parse config file: {}", e)))?;

        info!(path = ?path, "Config loaded successfully");
        Ok(config)
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
        };

        if ctx.has_any_provider() {
            Some(ctx)
        } else {
            None
        }
    }

    /// Set a configuration value by key path.
    ///
    /// Key format: `provider.field` (e.g., `github.owner`, `gitlab.url`)
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let parts: Vec<&str> = key.split('.').collect();
        if parts.len() != 2 {
            return Err(Error::Config(format!(
                "Invalid config key '{}'. Expected format: provider.field",
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
            _ => {
                return Err(Error::Config(format!("Unknown provider: {}", provider)));
            }
        }

        Ok(())
    }

    /// Get a configuration value by key path.
    ///
    /// Key format: `provider.field` (e.g., `github.owner`, `gitlab.url`)
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        let parts: Vec<&str> = key.split('.').collect();
        if parts.len() != 2 {
            return Err(Error::Config(format!(
                "Invalid config key '{}'. Expected format: provider.field",
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
            _ => Err(Error::Config(format!("Unknown provider: {}", provider))),
        }
    }
}

impl ContextConfig {
    /// Check whether this context config defines at least one provider.
    pub fn has_any_provider(&self) -> bool {
        self.github.is_some()
            || self.gitlab.is_some()
            || self.clickup.is_some()
            || self.jira.is_some()
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
            contexts: BTreeMap::new(),
            active_context: None,
            proxy_mcp_servers: Vec::new(),
            builtin_tools: BuiltinToolsConfig::default(),
            format_pipeline: None,
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
            contexts: BTreeMap::new(),
            active_context: None,
            proxy_mcp_servers: Vec::new(),
            builtin_tools: BuiltinToolsConfig::default(),
            format_pipeline: None,
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
                    repo: "dev-boy-monorepo".to_string(),
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
