use devboy_core::{Error, Provider, Result};

use crate::context::{ClickUpScope, GitHubScope, GitLabScope, JiraScope, ProviderConfig};

/// Create a provider instance from a typed `ProviderConfig`.
///
/// Provider is created on the stack — cheap and stateless.
/// The scope determines which project/repo/list is targeted.
///
/// # Unsupported scopes
///
/// Group, Organization, and Global scopes are not yet implemented.
/// They will be added when cross-project queries are needed.
pub fn create_provider(config: &ProviderConfig) -> Result<Box<dyn Provider>> {
    match config {
        ProviderConfig::GitLab {
            base_url,
            access_token,
            scope,
            ..
        } => match scope {
            GitLabScope::Project { id } => Ok(Box::new(
                devboy_gitlab::GitLabClient::with_base_url(base_url, id, access_token),
            )),
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
            ..
        } => match scope {
            JiraScope::Project { key } => Ok(Box::new(devboy_jira::JiraClient::new(
                base_url,
                key,
                email,
                access_token,
            ))),
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
        let provider = create_provider(&config);
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
        let provider = create_provider(&config);
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
        let provider = create_provider(&config);
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
            extra: HashMap::new(),
        };
        let provider = create_provider(&config);
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
        let result = create_provider(&config);
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
        let result = create_provider(&config);
        assert!(result.is_err());
    }
}
