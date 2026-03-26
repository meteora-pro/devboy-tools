use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Scope for GitLab API calls — determines the endpoint prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GitLabScope {
    /// Single project: `/api/v4/projects/{id}/...`
    Project { id: String },
    /// Group-level: `/api/v4/groups/{id}/...`
    Group { id: String },
    /// Global: `/api/v4/...`
    Global,
}

/// Scope for GitHub API calls — determines the endpoint prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GitHubScope {
    /// Single repository: `/repos/{owner}/{repo}/...`
    Repository { owner: String, repo: String },
    /// Organization-level: search with `org:` qualifier
    Organization { name: String },
    /// Global: search across all accessible resources
    Global,
}

/// Scope for ClickUp API calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClickUpScope {
    /// Single list (with optional team_id for custom task ID resolution)
    List { id: String, team_id: Option<String> },
}

/// Scope for Jira API calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JiraScope {
    /// Single Jira project
    Project { key: String },
    /// Multiple Jira projects (union of results)
    MultiProject { keys: Vec<String> },
}

/// Provider connection configuration with typed scope.
///
/// Each variant carries only the fields relevant to that provider.
/// Scope is provider-specific — compiler prevents invalid combinations
/// (e.g., GitLab Group scope on a GitHub provider).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProviderConfig {
    GitLab {
        base_url: String,
        access_token: String,
        scope: GitLabScope,
        #[serde(default)]
        extra: HashMap<String, serde_json::Value>,
    },
    GitHub {
        base_url: String,
        access_token: String,
        scope: GitHubScope,
        #[serde(default)]
        extra: HashMap<String, serde_json::Value>,
    },
    ClickUp {
        access_token: String,
        scope: ClickUpScope,
        #[serde(default)]
        extra: HashMap<String, serde_json::Value>,
    },
    Jira {
        base_url: String,
        access_token: String,
        email: String,
        scope: JiraScope,
        #[serde(default)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// Fully dynamic variant for community/custom provider plugins.
    Custom {
        name: String,
        config: HashMap<String, serde_json::Value>,
    },
}

impl ProviderConfig {
    /// Returns the provider name as a static string.
    pub fn provider_name(&self) -> &str {
        match self {
            Self::GitLab { .. } => "gitlab",
            Self::GitHub { .. } => "github",
            Self::ClickUp { .. } => "clickup",
            Self::Jira { .. } => "jira",
            Self::Custom { name, .. } => name,
        }
    }
}

/// Proxy configuration for providers behind firewalls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub url: String,
    pub token: Option<String>,
}

/// Runtime context passed to the executor for each tool call.
///
/// Contains everything needed to create a provider and execute a tool:
/// - `provider` — typed connection config with scope
/// - `proxy` — optional proxy for self-hosted instances
/// - `extra` — cross-cutting concerns (tracing, feature flags, caller metadata)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdditionalContext {
    pub provider: ProviderConfig,
    #[serde(default)]
    pub proxy: Option<ProxyConfig>,
    #[serde(default)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_config_gitlab_project_scope() {
        let config = ProviderConfig::GitLab {
            base_url: "https://gitlab.com".into(),
            access_token: "glpat-xxx".into(),
            scope: GitLabScope::Project { id: "12345".into() },
            extra: HashMap::new(),
        };
        assert_eq!(config.provider_name(), "gitlab");
    }

    #[test]
    fn test_provider_config_github_repo_scope() {
        let config = ProviderConfig::GitHub {
            base_url: "https://api.github.com".into(),
            access_token: "ghp_xxx".into(),
            scope: GitHubScope::Repository {
                owner: "meteora-pro".into(),
                repo: "devboy-tools".into(),
            },
            extra: HashMap::new(),
        };
        assert_eq!(config.provider_name(), "github");
    }

    #[test]
    fn test_provider_config_custom() {
        let config = ProviderConfig::Custom {
            name: "my-provider".into(),
            config: HashMap::new(),
        };
        assert_eq!(config.provider_name(), "my-provider");
    }

    #[test]
    fn test_additional_context_serialization() {
        let ctx = AdditionalContext {
            provider: ProviderConfig::GitLab {
                base_url: "https://gitlab.com".into(),
                access_token: "token".into(),
                scope: GitLabScope::Project { id: "123".into() },
                extra: HashMap::new(),
            },
            proxy: None,
            extra: HashMap::new(),
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: AdditionalContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.provider.provider_name(), "gitlab");
    }
}
