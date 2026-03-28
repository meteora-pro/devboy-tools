use devboy_core::{Error, Provider, Result, ToolEnricher};

use crate::context::{
    ClickUpScope, GitHubScope, GitLabScope, JiraScope, ProviderConfig, ProviderMetadata,
    ProxyConfig,
};

/// Create a provider instance from a typed `ProviderConfig`.
///
/// Provider is created on the stack — cheap and stateless.
/// The scope determines which project/repo/list is targeted.
///
/// # Unsupported scopes
///
/// Group, Organization, and Global scopes are not yet implemented.
/// They will be added when cross-project queries are needed.
pub fn create_provider(
    config: &ProviderConfig,
    proxy: Option<&ProxyConfig>,
) -> Result<Box<dyn Provider>> {
    match config {
        ProviderConfig::GitLab {
            base_url,
            access_token,
            scope,
            ..
        } => match scope {
            GitLabScope::Project { id } => {
                let client = if let Some(proxy) = proxy {
                    devboy_gitlab::GitLabClient::with_base_url(&proxy.url, id, access_token)
                        .with_proxy(proxy.headers.clone())
                } else {
                    devboy_gitlab::GitLabClient::with_base_url(base_url, id, access_token)
                };
                Ok(Box::new(client))
            }
            GitLabScope::Group { id } => Err(Error::ProviderUnsupported {
                provider: "gitlab".into(),
                operation: format!("group scope (group_id: {id}) not yet implemented"),
            }),
            GitLabScope::Global => Err(Error::ProviderUnsupported {
                provider: "gitlab".into(),
                operation: "global scope not yet implemented".into(),
            }),
        },

        ProviderConfig::GitHub {
            base_url,
            access_token,
            scope,
            ..
        } => match scope {
            GitHubScope::Repository { owner, repo } => Ok(Box::new(
                devboy_github::GitHubClient::with_base_url(base_url, owner, repo, access_token),
            )),
            GitHubScope::Organization { name } => Err(Error::ProviderUnsupported {
                provider: "github".into(),
                operation: format!("organization scope (org: {name}) not yet implemented"),
            }),
            GitHubScope::Global => Err(Error::ProviderUnsupported {
                provider: "github".into(),
                operation: "global scope not yet implemented".into(),
            }),
        },

        ProviderConfig::ClickUp {
            access_token,
            scope,
            ..
        } => match scope {
            ClickUpScope::List { id, team_id } => {
                let mut client = devboy_clickup::ClickUpClient::new(id, access_token);
                if let Some(tid) = team_id {
                    client = client.with_team_id(tid);
                }
                Ok(Box::new(client))
            }
        },

        ProviderConfig::Jira {
            base_url,
            access_token,
            email,
            scope,
            flavor,
            ..
        } => match scope {
            JiraScope::Project { key } => {
                let mut client = if let Some(proxy) = proxy {
                    devboy_jira::JiraClient::new(&proxy.url, key, email, access_token)
                        .with_proxy(proxy.headers.clone())
                        .with_instance_url(base_url)
                } else {
                    devboy_jira::JiraClient::new(base_url, key, email, access_token)
                };
                if let Some(f) = flavor {
                    client = client.with_flavor(*f);
                }
                Ok(Box::new(client))
            }
            JiraScope::MultiProject { keys } => Err(Error::ProviderUnsupported {
                provider: "jira".into(),
                operation: format!(
                    "multi-project scope ({}) not yet implemented",
                    keys.join(", ")
                ),
            }),
        },

        ProviderConfig::Custom { name, .. } => Err(Error::ProviderNotFound(format!(
            "custom provider '{name}' not yet supported"
        ))),
    }
}

/// Create the matching enricher for a provider.
///
/// Static providers (GitLab, GitHub) always return an enricher.
/// Dynamic providers (ClickUp, Jira) require metadata — returns None if missing.
///
/// The metadata `data` field is deserialized to provider-specific types.
pub fn create_enricher(
    config: &ProviderConfig,
    metadata: Option<&ProviderMetadata>,
) -> Option<Box<dyn ToolEnricher>> {
    match config {
        ProviderConfig::GitLab { .. } => Some(Box::new(devboy_gitlab::GitLabSchemaEnricher)),
        ProviderConfig::GitHub { .. } => Some(Box::new(devboy_github::GitHubSchemaEnricher)),
        ProviderConfig::ClickUp { .. } => {
            let meta = metadata?;
            let clickup_meta: devboy_clickup::ClickUpMetadata =
                serde_json::from_value(meta.data.clone()).ok()?;
            Some(Box::new(devboy_clickup::ClickUpSchemaEnricher::new(
                clickup_meta,
            )))
        }
        ProviderConfig::Jira { .. } => {
            let meta = metadata?;
            let jira_meta: devboy_jira::JiraMetadata =
                serde_json::from_value(meta.data.clone()).ok()?;
            Some(Box::new(devboy_jira::JiraSchemaEnricher::new(jira_meta)))
        }
        ProviderConfig::Custom { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::*;
    use devboy_core::IssueProvider;
    use std::collections::HashMap;

    #[test]
    fn test_create_gitlab_project_provider() {
        let config = ProviderConfig::GitLab {
            base_url: "https://gitlab.com".into(),
            access_token: "test-token".into(),
            scope: GitLabScope::Project { id: "12345".into() },
            extra: HashMap::new(),
        };
        let provider = create_provider(&config, None);
        assert!(provider.is_ok());
        assert_eq!(
            IssueProvider::provider_name(provider.unwrap().as_ref()),
            "gitlab"
        );
    }

    #[test]
    fn test_create_github_repo_provider() {
        let config = ProviderConfig::GitHub {
            base_url: "https://api.github.com".into(),
            access_token: "ghp_test".into(),
            scope: GitHubScope::Repository {
                owner: "meteora-pro".into(),
                repo: "devboy-tools".into(),
            },
            extra: HashMap::new(),
        };
        let provider = create_provider(&config, None);
        assert!(provider.is_ok());
        assert_eq!(
            IssueProvider::provider_name(provider.unwrap().as_ref()),
            "github"
        );
    }

    #[test]
    fn test_create_clickup_provider() {
        let config = ProviderConfig::ClickUp {
            access_token: "pk_test".into(),
            scope: ClickUpScope::List {
                id: "list123".into(),
                team_id: Some("team456".into()),
            },
            extra: HashMap::new(),
        };
        let provider = create_provider(&config, None);
        assert!(provider.is_ok());
        assert_eq!(
            IssueProvider::provider_name(provider.unwrap().as_ref()),
            "clickup"
        );
    }

    #[test]
    fn test_create_jira_provider() {
        let config = ProviderConfig::Jira {
            base_url: "https://myorg.atlassian.net".into(),
            access_token: "jira-token".into(),
            email: "user@example.com".into(),
            scope: JiraScope::Project { key: "PROJ".into() },
            flavor: None,
            extra: HashMap::new(),
        };
        let provider = create_provider(&config, None);
        assert!(provider.is_ok());
        assert_eq!(
            IssueProvider::provider_name(provider.unwrap().as_ref()),
            "jira"
        );
    }

    #[test]
    fn test_create_custom_provider_unsupported() {
        let config = ProviderConfig::Custom {
            name: "my-plugin".into(),
            config: HashMap::new(),
        };
        let result = create_provider(&config, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_gitlab_group_scope_unsupported() {
        let config = ProviderConfig::GitLab {
            base_url: "https://gitlab.com".into(),
            access_token: "token".into(),
            scope: GitLabScope::Group {
                id: "group1".into(),
            },
            extra: HashMap::new(),
        };
        let result = create_provider(&config, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_enricher_gitlab_static() {
        let config = ProviderConfig::GitLab {
            base_url: "https://gitlab.com".into(),
            access_token: "token".into(),
            scope: GitLabScope::Project { id: "123".into() },
            extra: HashMap::new(),
        };
        let enricher = create_enricher(&config, None);
        assert!(enricher.is_some());
    }

    #[test]
    fn test_create_enricher_github_static() {
        let config = ProviderConfig::GitHub {
            base_url: "https://api.github.com".into(),
            access_token: "token".into(),
            scope: GitHubScope::Repository {
                owner: "test".into(),
                repo: "test".into(),
            },
            extra: HashMap::new(),
        };
        let enricher = create_enricher(&config, None);
        assert!(enricher.is_some());
    }

    #[test]
    fn test_create_enricher_clickup_needs_metadata() {
        let config = ProviderConfig::ClickUp {
            access_token: "token".into(),
            scope: ClickUpScope::List {
                id: "list1".into(),
                team_id: None,
            },
            extra: HashMap::new(),
        };
        // No metadata → None
        assert!(create_enricher(&config, None).is_none());

        // With metadata → Some
        let meta = ProviderMetadata::new(serde_json::json!({
            "statuses": [{ "name": "To Do" }],
            "custom_fields": []
        }));
        assert!(create_enricher(&config, Some(&meta)).is_some());
    }

    #[test]
    fn test_create_enricher_jira_needs_metadata() {
        let config = ProviderConfig::Jira {
            base_url: "https://test.atlassian.net".into(),
            access_token: "token".into(),
            email: "test@test.com".into(),
            scope: JiraScope::Project { key: "PROJ".into() },
            flavor: None,
            extra: HashMap::new(),
        };
        // No metadata → None
        assert!(create_enricher(&config, None).is_none());

        // With metadata → Some
        let meta = ProviderMetadata::new(serde_json::json!({
            "flavor": "cloud",
            "projects": {
                "PROJ": {
                    "issue_types": [],
                    "priorities": [],
                    "components": [],
                    "link_types": [],
                    "custom_fields": []
                }
            }
        }));
        assert!(create_enricher(&config, Some(&meta)).is_some());
    }
}
