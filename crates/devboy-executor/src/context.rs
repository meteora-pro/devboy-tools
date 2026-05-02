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

/// Scope for Confluence API calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConfluenceScope {
    /// Single Confluence instance, optionally scoped by a default space key.
    Space { key: Option<String> },
}

/// Scope for Slack API calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SlackScope {
    /// Single Slack workspace/team.
    Workspace { team_id: Option<String> },
}

/// Authentication configuration for Confluence self-hosted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfluenceAuthConfig {
    BearerToken { token: String },
    Basic { username: String, password: String },
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
        /// Explicit flavor override. When set, skips auto-detection from URL.
        /// Important for proxy scenarios where URL doesn't reflect actual Jira deployment.
        flavor: Option<devboy_jira::JiraFlavor>,
        #[serde(default)]
        extra: HashMap<String, serde_json::Value>,
    },
    Confluence {
        base_url: String,
        auth: ConfluenceAuthConfig,
        scope: ConfluenceScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_version: Option<String>,
        #[serde(default)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// Fireflies.ai meeting notes provider.
    Fireflies {
        api_key: String,
        #[serde(default)]
        extra: HashMap<String, serde_json::Value>,
    },
    /// Slack messenger provider.
    Slack {
        base_url: String,
        access_token: String,
        scope: SlackScope,
        #[serde(default)]
        required_scopes: Vec<String>,
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
            Self::Confluence { .. } => "confluence",
            Self::Fireflies { .. } => "fireflies",
            Self::Slack { .. } => "slack",
            Self::Custom { name, .. } => name,
        }
    }
}

/// Proxy configuration for providers behind firewalls.
///
/// When proxy is configured, `url` replaces the provider's base URL
/// and `headers` are added to every request (e.g. auth tokens, routing headers).
/// The provider's own auth headers are suppressed — proxy handles authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

/// Provider-specific metadata for dynamic schema enrichment.
///
/// Static providers (GitLab, GitHub) don't need metadata.
/// Dynamic providers (ClickUp, Jira) receive metadata from external sources
/// (e.g., DB in cloud mode, API in CLI mode) to populate enum values and custom fields.
///
/// Metadata is passed as `serde_json::Value` to avoid coupling devboy-executor
/// to provider crate types. Each provider enricher deserializes its own metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMetadata {
    /// Raw metadata value — provider enricher will deserialize this.
    pub data: serde_json::Value,
}

impl ProviderMetadata {
    pub fn new(data: serde_json::Value) -> Self {
        Self { data }
    }
}

/// Runtime context passed to the executor for each tool call.
///
/// Contains everything needed to create a provider and execute a tool:
/// - `provider` — typed connection config with scope
/// - `proxy` — optional proxy for self-hosted instances
/// - `metadata` — optional provider metadata for dynamic enrichment
/// - `extra` — cross-cutting concerns (tracing, feature flags, caller metadata)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdditionalContext {
    pub provider: ProviderConfig,
    #[serde(default)]
    pub proxy: Option<ProxyConfig>,
    #[serde(default)]
    pub metadata: Option<ProviderMetadata>,
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
    fn test_provider_config_confluence_scope() {
        let config = ProviderConfig::Confluence {
            base_url: "https://wiki.example.com".into(),
            auth: ConfluenceAuthConfig::BearerToken {
                token: "pat-token".into(),
            },
            scope: ConfluenceScope::Space {
                key: Some("ENG".into()),
            },
            api_version: Some("v1".into()),
            extra: HashMap::new(),
        };
        assert_eq!(config.provider_name(), "confluence");
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
            metadata: None,
            extra: HashMap::new(),
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: AdditionalContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.provider.provider_name(), "gitlab");
    }

    // --- ProxyConfig serde ---

    #[test]
    fn test_proxy_config_serialize_deserialize() {
        let mut headers = HashMap::new();
        headers.insert("X-Auth-Token".into(), "secret123".into());
        headers.insert("X-Routing".into(), "internal".into());

        let proxy = ProxyConfig {
            url: "https://proxy.internal/jira".into(),
            headers,
        };

        let json = serde_json::to_string(&proxy).unwrap();
        let deserialized: ProxyConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.url, "https://proxy.internal/jira");
        assert_eq!(deserialized.headers.len(), 2);
        assert_eq!(deserialized.headers["X-Auth-Token"], "secret123");
        assert_eq!(deserialized.headers["X-Routing"], "internal");
    }

    #[test]
    fn test_proxy_config_empty_headers_default() {
        let json = r#"{"url":"https://proxy.example.com"}"#;
        let proxy: ProxyConfig = serde_json::from_str(json).unwrap();
        assert_eq!(proxy.url, "https://proxy.example.com");
        assert!(proxy.headers.is_empty());
    }

    // --- JiraFlavor serde ---

    #[test]
    fn test_jira_flavor_serde_cloud() {
        let config = ProviderConfig::Jira {
            base_url: "https://test.atlassian.net".into(),
            access_token: "tok".into(),
            email: "a@b.com".into(),
            scope: JiraScope::Project { key: "TST".into() },
            flavor: Some(devboy_jira::JiraFlavor::Cloud),
            extra: HashMap::new(),
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"cloud\""));

        let deserialized: ProviderConfig = serde_json::from_str(&json).unwrap();
        match deserialized {
            ProviderConfig::Jira { flavor, .. } => {
                assert_eq!(flavor, Some(devboy_jira::JiraFlavor::Cloud));
            }
            _ => panic!("expected Jira variant"),
        }
    }

    #[test]
    fn test_jira_flavor_serde_self_hosted() {
        let config = ProviderConfig::Jira {
            base_url: "https://jira.mycompany.com".into(),
            access_token: "tok".into(),
            email: "a@b.com".into(),
            scope: JiraScope::Project { key: "INT".into() },
            flavor: Some(devboy_jira::JiraFlavor::SelfHosted),
            extra: HashMap::new(),
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"self_hosted\""));

        let deserialized: ProviderConfig = serde_json::from_str(&json).unwrap();
        match deserialized {
            ProviderConfig::Jira { flavor, .. } => {
                assert_eq!(flavor, Some(devboy_jira::JiraFlavor::SelfHosted));
            }
            _ => panic!("expected Jira variant"),
        }
    }

    #[test]
    fn test_jira_flavor_none_roundtrip() {
        let config = ProviderConfig::Jira {
            base_url: "https://jira.example.com".into(),
            access_token: "tok".into(),
            email: "a@b.com".into(),
            scope: JiraScope::Project { key: "PRJ".into() },
            flavor: None,
            extra: HashMap::new(),
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ProviderConfig = serde_json::from_str(&json).unwrap();
        match deserialized {
            ProviderConfig::Jira { flavor, .. } => assert!(flavor.is_none()),
            _ => panic!("expected Jira variant"),
        }
    }

    // --- ProviderConfig provider_name for remaining variants ---

    #[test]
    fn test_provider_name_clickup() {
        let config = ProviderConfig::ClickUp {
            access_token: "pk_test".into(),
            scope: ClickUpScope::List {
                id: "list1".into(),
                team_id: None,
            },
            extra: HashMap::new(),
        };
        assert_eq!(config.provider_name(), "clickup");
    }

    #[test]
    fn test_provider_name_jira() {
        let config = ProviderConfig::Jira {
            base_url: "https://jira.example.com".into(),
            access_token: "tok".into(),
            email: "a@b.com".into(),
            scope: JiraScope::Project { key: "X".into() },
            flavor: None,
            extra: HashMap::new(),
        };
        assert_eq!(config.provider_name(), "jira");
    }

    // --- AdditionalContext with proxy and metadata ---

    #[test]
    fn test_additional_context_with_proxy_roundtrip() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".into(), "Bearer xyz".into());

        let ctx = AdditionalContext {
            provider: ProviderConfig::Jira {
                base_url: "https://jira.example.com".into(),
                access_token: "tok".into(),
                email: "a@b.com".into(),
                scope: JiraScope::Project { key: "PRJ".into() },
                flavor: Some(devboy_jira::JiraFlavor::SelfHosted),
                extra: HashMap::new(),
            },
            proxy: Some(ProxyConfig {
                url: "https://proxy.internal".into(),
                headers,
            }),
            metadata: Some(ProviderMetadata::new(serde_json::json!({"key": "value"}))),
            extra: {
                let mut m = HashMap::new();
                m.insert("trace_id".into(), serde_json::json!("abc-123"));
                m
            },
        };

        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: AdditionalContext = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.provider.provider_name(), "jira");
        assert!(deserialized.proxy.is_some());
        let proxy = deserialized.proxy.unwrap();
        assert_eq!(proxy.url, "https://proxy.internal");
        assert_eq!(proxy.headers["Authorization"], "Bearer xyz");
        assert!(deserialized.metadata.is_some());
        assert_eq!(deserialized.extra["trace_id"], serde_json::json!("abc-123"));
    }

    #[test]
    fn test_provider_metadata_new() {
        let data = serde_json::json!({"statuses": [{"name": "Done"}]});
        let meta = ProviderMetadata::new(data.clone());
        assert_eq!(meta.data, data);
    }

    // --- Scope variants serde ---

    #[test]
    fn test_gitlab_scope_global_serde() {
        let scope = GitLabScope::Global;
        let json = serde_json::to_string(&scope).unwrap();
        let deserialized: GitLabScope = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, GitLabScope::Global));
    }

    #[test]
    fn test_gitlab_scope_group_serde() {
        let scope = GitLabScope::Group { id: "g42".into() };
        let json = serde_json::to_string(&scope).unwrap();
        let deserialized: GitLabScope = serde_json::from_str(&json).unwrap();
        match deserialized {
            GitLabScope::Group { id } => assert_eq!(id, "g42"),
            _ => panic!("expected Group"),
        }
    }

    #[test]
    fn test_github_scope_organization_serde() {
        let scope = GitHubScope::Organization {
            name: "myorg".into(),
        };
        let json = serde_json::to_string(&scope).unwrap();
        let deserialized: GitHubScope = serde_json::from_str(&json).unwrap();
        match deserialized {
            GitHubScope::Organization { name } => assert_eq!(name, "myorg"),
            _ => panic!("expected Organization"),
        }
    }

    #[test]
    fn test_github_scope_global_serde() {
        let scope = GitHubScope::Global;
        let json = serde_json::to_string(&scope).unwrap();
        let deserialized: GitHubScope = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, GitHubScope::Global));
    }

    #[test]
    fn test_clickup_scope_list_with_team_id_serde() {
        let scope = ClickUpScope::List {
            id: "lst1".into(),
            team_id: Some("team99".into()),
        };
        let json = serde_json::to_string(&scope).unwrap();
        let deserialized: ClickUpScope = serde_json::from_str(&json).unwrap();
        match deserialized {
            ClickUpScope::List { id, team_id } => {
                assert_eq!(id, "lst1");
                assert_eq!(team_id, Some("team99".into()));
            }
        }
    }

    #[test]
    fn test_clickup_scope_list_without_team_id_serde() {
        let scope = ClickUpScope::List {
            id: "lst2".into(),
            team_id: None,
        };
        let json = serde_json::to_string(&scope).unwrap();
        let deserialized: ClickUpScope = serde_json::from_str(&json).unwrap();
        match deserialized {
            ClickUpScope::List { id, team_id } => {
                assert_eq!(id, "lst2");
                assert!(team_id.is_none());
            }
        }
    }

    #[test]
    fn test_jira_scope_multi_project_serde() {
        let scope = JiraScope::MultiProject {
            keys: vec!["AAA".into(), "BBB".into()],
        };
        let json = serde_json::to_string(&scope).unwrap();
        let deserialized: JiraScope = serde_json::from_str(&json).unwrap();
        match deserialized {
            JiraScope::MultiProject { keys } => {
                assert_eq!(keys, vec!["AAA".to_string(), "BBB".to_string()]);
            }
            _ => panic!("expected MultiProject"),
        }
    }

    // --- ProviderConfig with extra fields ---

    #[test]
    fn test_provider_config_with_extra_fields() {
        let mut extra = HashMap::new();
        extra.insert("custom_key".into(), serde_json::json!("custom_value"));
        extra.insert("numeric".into(), serde_json::json!(42));

        let config = ProviderConfig::GitLab {
            base_url: "https://gitlab.com".into(),
            access_token: "tok".into(),
            scope: GitLabScope::Project { id: "1".into() },
            extra,
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ProviderConfig = serde_json::from_str(&json).unwrap();

        match deserialized {
            ProviderConfig::GitLab { extra, .. } => {
                assert_eq!(extra["custom_key"], serde_json::json!("custom_value"));
                assert_eq!(extra["numeric"], serde_json::json!(42));
            }
            _ => panic!("expected GitLab"),
        }
    }

    #[test]
    fn test_custom_provider_config_serde() {
        let mut config_map = HashMap::new();
        config_map.insert("endpoint".into(), serde_json::json!("https://custom.api"));
        config_map.insert("api_key".into(), serde_json::json!("key123"));

        let config = ProviderConfig::Custom {
            name: "linear".into(),
            config: config_map,
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ProviderConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.provider_name(), "linear");
        match deserialized {
            ProviderConfig::Custom { name, config } => {
                assert_eq!(name, "linear");
                assert_eq!(config["endpoint"], serde_json::json!("https://custom.api"));
            }
            _ => panic!("expected Custom"),
        }
    }

    #[test]
    fn test_confluence_provider_config_serde() {
        let config = ProviderConfig::Confluence {
            base_url: "https://wiki.example.com".into(),
            auth: ConfluenceAuthConfig::Basic {
                username: "dev@example.com".into(),
                password: "secret".into(),
            },
            scope: ConfluenceScope::Space {
                key: Some("ENG".into()),
            },
            api_version: Some("v1".into()),
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ProviderConfig = serde_json::from_str(&json).unwrap();

        match deserialized {
            ProviderConfig::Confluence {
                base_url,
                auth,
                scope,
                api_version,
                ..
            } => {
                assert_eq!(base_url, "https://wiki.example.com");
                assert_eq!(api_version.as_deref(), Some("v1"));
                match auth {
                    ConfluenceAuthConfig::Basic { username, password } => {
                        assert_eq!(username, "dev@example.com");
                        assert_eq!(password, "secret");
                    }
                    _ => panic!("expected Basic auth"),
                }
                match scope {
                    ConfluenceScope::Space { key } => {
                        assert_eq!(key.as_deref(), Some("ENG"));
                    }
                }
            }
            _ => panic!("expected Confluence"),
        }
    }
}
