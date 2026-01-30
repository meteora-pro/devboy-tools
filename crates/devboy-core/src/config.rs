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

fn default_gitlab_url() -> String {
    "https://gitlab.com".to_string()
}

// =============================================================================
// Config implementation
// =============================================================================

impl Config {
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
                        )))
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
                        )))
                    }
                }
            }
            "clickup" => {
                let config = self.clickup.get_or_insert_with(|| ClickUpConfig {
                    list_id: String::new(),
                });
                match field {
                    "list_id" | "list" => config.list_id = value.to_string(),
                    _ => {
                        return Err(Error::Config(format!(
                            "Unknown ClickUp config field: {}",
                            field
                        )))
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
                        )))
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
        let mut config = Config::default();
        config.github = Some(GitHubConfig {
            owner: "test-owner".to_string(),
            repo: "test-repo".to_string(),
            base_url: None,
        });

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
}
