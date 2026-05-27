use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Scope for GitLab API calls — determines the endpoint prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GitLabScope {
    /// Single project: `/api/v4/projects/{id}/...`
    Project {
        /// Project id (numeric or `group/project` slug).
        id: String,
    },
    /// Group-level: `/api/v4/groups/{id}/...`
    Group {
        /// Group id (numeric or path).
        id: String,
    },
    /// Global: `/api/v4/...`
    Global,
}

/// Scope for GitHub API calls — determines the endpoint prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GitHubScope {
    /// Single repository: `/repos/{owner}/{repo}/...`
    Repository {
        /// Owner (user or org).
        owner: String,
        repo: String,
    },
    /// Organization-level: search with `org:` qualifier
    Organization {
        /// Organization login.
        name: String,
    },
    /// Global: search across all accessible resources
    Global,
}

/// Scope for ClickUp API calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClickUpScope {
    /// Single list (with optional team_id for custom task ID resolution)
    List {
        id: String,
        /// Optional team id (workspace) for custom task ID resolution.
        team_id: Option<String>,
    },
}

/// Scope for Jira API calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JiraScope {
    /// Single Jira project
    Project {
        /// Project key (e.g. `DEV`).
        key: String,
    },
    /// Multiple Jira projects (union of results)
    MultiProject { keys: Vec<String> },
}

/// Scope for Confluence API calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConfluenceScope {
    /// Single Confluence instance, optionally scoped by a default space key.
    Space {
        /// Default space key (optional).
        key: Option<String>,
    },
}

/// Scope for Slack API calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SlackScope {
    /// Single Slack workspace/team.
    Workspace { team_id: Option<String> },
}

/// Scope for Telegram API calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TelegramScope {
    /// Single Telegram bot identity.
    Bot { bot_username: Option<String> },
}

/// Authentication configuration for Confluence self-hosted.
///
/// **Note:** intentionally not `Serialize`/`Deserialize` — Confluence credentials
/// are constructed in-process from the keychain. Cross-process transport would
/// expose `password` / `token` via the wire format.
#[derive(Debug, Clone)]
pub enum ConfluenceAuthConfig {
    BearerToken {
        token: SecretString,
    },
    Basic {
        username: String,
        password: SecretString,
    },
}

/// Provider connection configuration with typed scope.
///
/// Each variant carries only the fields relevant to that provider.
/// Scope is provider-specific — compiler prevents invalid combinations
/// (e.g., GitLab Group scope on a GitHub provider).
///
/// **Note:** intentionally not `Serialize`/`Deserialize`. Provider configs
/// carry plaintext access tokens; serializing them to JSON would defeat the
/// `SecretString` discipline. Construct provider configs in-process from
/// `Config` + `CredentialStore` instead of round-tripping through transport.
#[derive(Debug, Clone)]
pub enum ProviderConfig {
    GitLab {
        base_url: String,
        access_token: SecretString,
        scope: GitLabScope,
        extra: HashMap<String, serde_json::Value>,
    },
    GitHub {
        base_url: String,
        access_token: SecretString,
        scope: GitHubScope,
        extra: HashMap<String, serde_json::Value>,
    },
    ClickUp {
        access_token: SecretString,
        scope: ClickUpScope,
        extra: HashMap<String, serde_json::Value>,
    },
    Jira {
        base_url: String,
        access_token: SecretString,
        email: String,
        scope: JiraScope,
        /// Explicit flavor override. When set, skips auto-detection from URL.
        /// Important for proxy scenarios where URL doesn't reflect actual Jira deployment.
        flavor: Option<devboy_jira::JiraFlavor>,
        extra: HashMap<String, serde_json::Value>,
    },
    Confluence {
        base_url: String,
        auth: ConfluenceAuthConfig,
        scope: ConfluenceScope,
        api_version: Option<String>,
        extra: HashMap<String, serde_json::Value>,
    },
    /// Fireflies.ai meeting notes provider.
    Fireflies {
        api_key: SecretString,
        extra: HashMap<String, serde_json::Value>,
    },
    /// Slack messenger provider.
    Slack {
        base_url: String,
        access_token: SecretString,
        scope: SlackScope,
        required_scopes: Vec<String>,
        extra: HashMap<String, serde_json::Value>,
    },
    /// Telegram messenger provider.
    Telegram {
        base_url: String,
        access_token: SecretString,
        scope: TelegramScope,
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
            Self::Telegram { .. } => "telegram",
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
///
/// **Note:** intentionally not `Serialize`/`Deserialize` — `provider` carries
/// `SecretString` access tokens that must not leak through wire formats.
#[derive(Debug, Clone)]
pub struct AdditionalContext {
    pub provider: ProviderConfig,
    pub proxy: Option<ProxyConfig>,
    pub metadata: Option<ProviderMetadata>,
    pub extra: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    fn token(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    #[test]
    fn test_provider_config_gitlab_project_scope() {
        let config = ProviderConfig::GitLab {
            base_url: "https://gitlab.com".into(),
            access_token: token("glpat-xxx"),
            scope: GitLabScope::Project { id: "12345".into() },
            extra: HashMap::new(),
        };
        assert_eq!(config.provider_name(), "gitlab");
    }

    #[test]
    fn test_provider_config_github_repo_scope() {
        let config = ProviderConfig::GitHub {
            base_url: "https://api.github.com".into(),
            access_token: token("ghp_xxx"),
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
                token: token("pat-token"),
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
    fn test_provider_name_clickup() {
        let config = ProviderConfig::ClickUp {
            access_token: token("pk_test"),
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
            access_token: token("tok"),
            email: "a@b.com".into(),
            scope: JiraScope::Project { key: "X".into() },
            flavor: None,
            extra: HashMap::new(),
        };
        assert_eq!(config.provider_name(), "jira");
    }

    #[test]
    fn test_provider_name_telegram() {
        let config = ProviderConfig::Telegram {
            base_url: "https://api.telegram.org".into(),
            access_token: token("bot-token"),
            scope: TelegramScope::Bot { bot_username: None },
            extra: HashMap::new(),
        };
        assert_eq!(config.provider_name(), "telegram");
    }

    #[test]
    fn test_provider_metadata_new() {
        let data = serde_json::json!({"statuses": [{"name": "Done"}]});
        let meta = ProviderMetadata::new(data.clone());
        assert_eq!(meta.data, data);
    }

    #[test]
    fn test_proxy_config_serialize_deserialize() {
        // ProxyConfig still keeps Serialize/Deserialize because it carries no
        // secrets; provider auth lives on `ProviderConfig::access_token` instead.
        let mut headers = HashMap::new();
        headers.insert("X-Routing".into(), "internal".into());
        let proxy = ProxyConfig {
            url: "https://proxy.internal/jira".into(),
            headers,
        };
        let json = serde_json::to_string(&proxy).unwrap();
        let deserialized: ProxyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.url, "https://proxy.internal/jira");
        assert_eq!(deserialized.headers["X-Routing"], "internal");
    }

    #[test]
    fn test_provider_config_debug_redacts_access_token() {
        let config = ProviderConfig::GitLab {
            base_url: "https://gitlab.com".into(),
            access_token: token("super-secret-glpat"),
            scope: GitLabScope::Project { id: "12345".into() },
            extra: HashMap::new(),
        };
        let dbg = format!("{:?}", config);
        assert!(
            !dbg.contains("super-secret-glpat"),
            "Debug must redact access_token, got: {dbg}"
        );
    }

    #[test]
    fn test_confluence_auth_basic_password_redacted() {
        let auth = ConfluenceAuthConfig::Basic {
            username: "dev@example.com".into(),
            password: token("super-secret-password"),
        };
        let dbg = format!("{:?}", auth);
        assert!(
            !dbg.contains("super-secret-password"),
            "Basic password must not appear in Debug: {dbg}"
        );

        // Sanity check: SecretString itself round-trips via expose_secret().
        if let ConfluenceAuthConfig::Basic { password, .. } = &auth {
            assert_eq!(password.expose_secret(), "super-secret-password");
        }
    }
}
