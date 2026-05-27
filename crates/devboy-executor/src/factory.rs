use devboy_core::{
    Error, KnowledgeBaseProvider, MeetingNotesProvider, MessengerProvider, Provider, Result,
    ToolEnricher,
};

use crate::context::{
    ClickUpScope, ConfluenceAuthConfig, ConfluenceScope, GitHubScope, GitLabScope, JiraScope,
    ProviderConfig, ProviderMetadata, ProxyConfig, SlackScope, TelegramScope,
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
                    devboy_gitlab::GitLabClient::with_base_url(
                        &proxy.url,
                        id,
                        access_token.clone(),
                    )
                    .with_proxy(proxy.headers.clone())
                } else {
                    devboy_gitlab::GitLabClient::with_base_url(
                        base_url,
                        id,
                        access_token.clone(),
                    )
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
            GitHubScope::Repository { owner, repo } => {
                Ok(Box::new(devboy_github::GitHubClient::with_base_url(
                    base_url,
                    owner,
                    repo,
                    access_token.clone(),
                )))
            }
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
                let mut client = devboy_clickup::ClickUpClient::new(id, access_token.clone());
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
                    devboy_jira::JiraClient::new(
                        &proxy.url,
                        key,
                        email,
                        access_token.clone(),
                    )
                    .with_proxy(proxy.headers.clone())
                    .with_instance_url(base_url)
                } else {
                    devboy_jira::JiraClient::new(base_url, key, email, access_token.clone())
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

        ProviderConfig::Confluence { .. } => Err(Error::ProviderUnsupported {
            provider: "confluence".into(),
            operation: "Confluence is a KnowledgeBaseProvider, not a Provider. Use create_knowledge_base_provider() instead.".into(),
        }),

        ProviderConfig::Fireflies { .. } => Err(Error::ProviderUnsupported {
            provider: "fireflies".into(),
            operation: "Fireflies is a MeetingNotesProvider, not a Provider. Use create_meeting_notes_provider() instead.".into(),
        }),

        ProviderConfig::Slack { .. } => Err(Error::ProviderUnsupported {
            provider: "slack".into(),
            operation: "Slack is a MessengerProvider, not a Provider. Use create_messenger_provider() instead.".into(),
        }),

        ProviderConfig::Telegram { .. } => Err(Error::ProviderUnsupported {
            provider: "telegram".into(),
            operation: "Telegram is a MessengerProvider, not a Provider. Use create_messenger_provider() instead.".into(),
        }),

        ProviderConfig::Custom { name, .. } => Err(Error::ProviderNotFound(format!(
            "custom provider '{name}' not yet supported"
        ))),
    }
}

pub fn create_knowledge_base_provider(
    config: &ProviderConfig,
    proxy: Option<&ProxyConfig>,
) -> Result<Box<dyn KnowledgeBaseProvider>> {
    match config {
        ProviderConfig::Confluence {
            base_url,
            auth,
            api_version,
            scope: ConfluenceScope::Space { .. },
            ..
        } => {
            let client = if let Some(proxy) = proxy {
                devboy_confluence::ConfluenceClient::new(
                    &proxy.url,
                    devboy_confluence::ConfluenceAuth::None,
                )
                .with_api_version(api_version.as_deref())
                .with_proxy(proxy.headers.clone())
                // Browse links in `_links.webui` / `/pages/<id>` must
                // point at the real Confluence host, not the proxy.
                .with_instance_url(base_url)
            } else {
                devboy_confluence::ConfluenceClient::new(base_url, confluence_auth(auth))
                    .with_api_version(api_version.as_deref())
            };
            Ok(Box::new(client))
        }
        other => Err(Error::ProviderUnsupported {
            provider: other.provider_name().into(),
            operation: "not a knowledge base provider".into(),
        }),
    }
}

/// Create the matching knowledge base enricher for a provider.
///
/// Knowledge-base-capable providers use a separate path from the regular
/// issue/git repository enrichers because they implement
/// `KnowledgeBaseProvider`, not `Provider`.
pub fn create_knowledge_base_enricher(config: &ProviderConfig) -> Option<Box<dyn ToolEnricher>> {
    match config {
        ProviderConfig::Confluence { .. } => {
            Some(Box::new(devboy_confluence::ConfluenceSchemaEnricher::new()))
        }
        _ => None,
    }
}

/// Create a meeting notes provider from config.
///
/// Separate from `create_provider()` because meeting notes providers
/// implement `MeetingNotesProvider`, not `Provider`.
pub fn create_meeting_notes_provider(
    config: &ProviderConfig,
) -> Result<Box<dyn MeetingNotesProvider>> {
    match config {
        ProviderConfig::Fireflies { api_key, .. } => Ok(Box::new(
            devboy_fireflies::FirefliesClient::new(api_key.clone()),
        )),
        other => Err(Error::ProviderUnsupported {
            provider: other.provider_name().into(),
            operation: "not a meeting notes provider".into(),
        }),
    }
}

pub fn create_messenger_provider(config: &ProviderConfig) -> Result<Box<dyn MessengerProvider>> {
    match config {
        ProviderConfig::Slack {
            base_url,
            access_token,
            scope: SlackScope::Workspace { .. },
            required_scopes,
            ..
        } => Ok(Box::new(
            devboy_slack::SlackClient::new(access_token.clone())
                .with_base_url(base_url)
                .with_required_scopes(required_scopes.clone()),
        )),
        ProviderConfig::Telegram {
            base_url,
            access_token,
            scope: TelegramScope::Bot { bot_username },
            ..
        } => Ok(Box::new(
            devboy_telegram::TelegramClient::new(access_token.clone())
                .with_base_url(base_url)
                .with_bot_username(bot_username.clone()),
        )),
        other => Err(Error::ProviderUnsupported {
            provider: other.provider_name().into(),
            operation: "not a messenger provider".into(),
        }),
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
        ProviderConfig::Confluence { .. } => None,
        ProviderConfig::Fireflies { .. } => {
            Some(Box::new(devboy_fireflies::FirefliesSchemaEnricher))
        }
        ProviderConfig::Slack { .. } => None,
        ProviderConfig::Telegram { .. } => None,
        ProviderConfig::Custom { .. } => None,
    }
}

fn confluence_auth(auth: &ConfluenceAuthConfig) -> devboy_confluence::ConfluenceAuth {
    match auth {
        ConfluenceAuthConfig::BearerToken { token } => {
            devboy_confluence::ConfluenceAuth::BearerToken(token.clone())
        }
        ConfluenceAuthConfig::Basic { username, password } => {
            devboy_confluence::ConfluenceAuth::Basic {
                username: username.clone(),
                password: password.clone(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::*;
    use devboy_core::{IssueProvider, KnowledgeBaseProvider, MessengerProvider};
    use httpmock::Method::GET;
    use httpmock::MockServer;
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
    fn test_create_telegram_provider() {
        let config = ProviderConfig::Telegram {
            base_url: "https://api.telegram.org".into(),
            access_token: "bot-token".into(),
            scope: TelegramScope::Bot {
                bot_username: Some("devboy_bot".into()),
            },
            extra: HashMap::new(),
        };
        let provider = create_messenger_provider(&config);
        assert!(provider.is_ok());
        assert_eq!(
            MessengerProvider::provider_name(provider.unwrap().as_ref()),
            "telegram"
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
    fn test_create_confluence_knowledge_base_provider() {
        let config = ProviderConfig::Confluence {
            base_url: "https://wiki.example.com".into(),
            auth: ConfluenceAuthConfig::BearerToken {
                token: "test-token".into(),
            },
            scope: ConfluenceScope::Space {
                key: Some("ENG".into()),
            },
            api_version: Some("v1".into()),
            extra: HashMap::new(),
        };
        let provider = create_knowledge_base_provider(&config, None);
        assert!(provider.is_ok());
        assert_eq!(
            KnowledgeBaseProvider::provider_name(provider.unwrap().as_ref()),
            "confluence"
        );
    }

    #[test]
    fn test_create_confluence_knowledge_base_enricher() {
        let config = ProviderConfig::Confluence {
            base_url: "https://wiki.example.com".into(),
            auth: ConfluenceAuthConfig::BearerToken {
                token: "test-token".into(),
            },
            scope: ConfluenceScope::Space {
                key: Some("ENG".into()),
            },
            api_version: Some("v1".into()),
            extra: HashMap::new(),
        };
        let enricher = create_knowledge_base_enricher(&config);
        assert!(enricher.is_some());
        assert_eq!(
            enricher.unwrap().supported_categories(),
            &[devboy_core::ToolCategory::KnowledgeBase]
        );
    }

    #[test]
    fn test_confluence_is_not_regular_provider() {
        let config = ProviderConfig::Confluence {
            base_url: "https://wiki.example.com".into(),
            auth: ConfluenceAuthConfig::BearerToken {
                token: "test-token".into(),
            },
            scope: ConfluenceScope::Space { key: None },
            api_version: None,
            extra: HashMap::new(),
        };

        let result = create_provider(&config, None);
        assert!(matches!(
            result,
            Err(Error::ProviderUnsupported { provider, .. }) if provider == "confluence"
        ));
    }

    #[test]
    fn test_telegram_is_not_regular_provider() {
        let config = ProviderConfig::Telegram {
            base_url: "https://api.telegram.org".into(),
            access_token: "bot-token".into(),
            scope: TelegramScope::Bot {
                bot_username: Some("devboy_bot".into()),
            },
            extra: HashMap::new(),
        };

        let result = create_provider(&config, None);
        assert!(matches!(
            result,
            Err(Error::ProviderUnsupported { provider, .. }) if provider == "telegram"
        ));
    }

    #[tokio::test]
    async fn test_create_confluence_knowledge_base_provider_honors_api_version() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/space")
                .query_param("limit", "100")
                .query_param("type", "global,personal");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"results":[],"start":0,"limit":100,"size":0,"_links":{}}"#);
        });

        let config = ProviderConfig::Confluence {
            base_url: server.base_url(),
            auth: ConfluenceAuthConfig::BearerToken {
                token: "test-token".into(),
            },
            scope: ConfluenceScope::Space { key: None },
            api_version: Some("v2".into()),
            extra: HashMap::new(),
        };

        let provider = create_knowledge_base_provider(&config, None).unwrap();
        let _ = provider.get_spaces().await.unwrap();

        mock.assert();
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

    // --- Proxy tests ---

    #[test]
    fn test_create_gitlab_provider_with_proxy() {
        let config = ProviderConfig::GitLab {
            base_url: "https://gitlab.internal.com".into(),
            access_token: "test-token".into(),
            scope: GitLabScope::Project { id: "99".into() },
            extra: HashMap::new(),
        };
        let mut headers = HashMap::new();
        headers.insert("X-Proxy-Auth".into(), "proxy-secret".into());
        let proxy = ProxyConfig {
            url: "https://proxy.example.com/gitlab".into(),
            headers,
        };
        let provider = create_provider(&config, Some(&proxy));
        assert!(provider.is_ok());
        assert_eq!(
            IssueProvider::provider_name(provider.unwrap().as_ref()),
            "gitlab"
        );
    }

    #[test]
    fn test_create_jira_provider_with_proxy() {
        let config = ProviderConfig::Jira {
            base_url: "https://jira.mycompany.com".into(),
            access_token: "jira-token".into(),
            email: "dev@mycompany.com".into(),
            scope: JiraScope::Project { key: "DEV".into() },
            flavor: None,
            extra: HashMap::new(),
        };
        let mut headers = HashMap::new();
        headers.insert("X-Proxy-Auth".into(), "secret".into());
        headers.insert("X-Route".into(), "jira-dc".into());
        let proxy = ProxyConfig {
            url: "https://proxy.internal/jira".into(),
            headers,
        };
        let provider = create_provider(&config, Some(&proxy));
        assert!(provider.is_ok());
        assert_eq!(
            IssueProvider::provider_name(provider.unwrap().as_ref()),
            "jira"
        );
    }

    #[test]
    fn test_create_jira_provider_with_flavor_cloud() {
        let config = ProviderConfig::Jira {
            base_url: "https://myorg.atlassian.net".into(),
            access_token: "tok".into(),
            email: "a@b.com".into(),
            scope: JiraScope::Project { key: "CLD".into() },
            flavor: Some(devboy_jira::JiraFlavor::Cloud),
            extra: HashMap::new(),
        };
        let provider = create_provider(&config, None);
        assert!(provider.is_ok());
    }

    #[test]
    fn test_create_jira_provider_with_flavor_self_hosted() {
        let config = ProviderConfig::Jira {
            base_url: "https://jira.local".into(),
            access_token: "tok".into(),
            email: "a@b.com".into(),
            scope: JiraScope::Project { key: "SH".into() },
            flavor: Some(devboy_jira::JiraFlavor::SelfHosted),
            extra: HashMap::new(),
        };
        let provider = create_provider(&config, None);
        assert!(provider.is_ok());
    }

    #[test]
    fn test_create_jira_provider_with_proxy_and_flavor_override() {
        let config = ProviderConfig::Jira {
            base_url: "https://jira.dc.mycompany.com".into(),
            access_token: "tok".into(),
            email: "a@b.com".into(),
            scope: JiraScope::Project { key: "DC".into() },
            flavor: Some(devboy_jira::JiraFlavor::SelfHosted),
            extra: HashMap::new(),
        };
        let proxy = ProxyConfig {
            url: "https://proxy.internal/jira".into(),
            headers: HashMap::new(),
        };
        let provider = create_provider(&config, Some(&proxy));
        assert!(provider.is_ok());
    }

    // --- Unsupported scope tests ---

    #[test]
    fn test_gitlab_global_scope_unsupported() {
        let config = ProviderConfig::GitLab {
            base_url: "https://gitlab.com".into(),
            access_token: "tok".into(),
            scope: GitLabScope::Global,
            extra: HashMap::new(),
        };
        let result = create_provider(&config, None);
        match result {
            Err(e) => assert!(e.to_string().contains("global scope")),
            Ok(_) => panic!("expected error for global scope"),
        }
    }

    #[test]
    fn test_github_organization_scope_unsupported() {
        let config = ProviderConfig::GitHub {
            base_url: "https://api.github.com".into(),
            access_token: "tok".into(),
            scope: GitHubScope::Organization {
                name: "myorg".into(),
            },
            extra: HashMap::new(),
        };
        let result = create_provider(&config, None);
        match result {
            Err(e) => assert!(e.to_string().contains("organization scope")),
            Ok(_) => panic!("expected error for organization scope"),
        }
    }

    #[test]
    fn test_github_global_scope_unsupported() {
        let config = ProviderConfig::GitHub {
            base_url: "https://api.github.com".into(),
            access_token: "tok".into(),
            scope: GitHubScope::Global,
            extra: HashMap::new(),
        };
        let result = create_provider(&config, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_jira_multi_project_scope_unsupported() {
        let config = ProviderConfig::Jira {
            base_url: "https://test.atlassian.net".into(),
            access_token: "tok".into(),
            email: "a@b.com".into(),
            scope: JiraScope::MultiProject {
                keys: vec!["A".into(), "B".into()],
            },
            flavor: None,
            extra: HashMap::new(),
        };
        let result = create_provider(&config, None);
        match result {
            Err(e) => assert!(e.to_string().contains("multi-project")),
            Ok(_) => panic!("expected error for multi-project scope"),
        }
    }

    // --- ClickUp without team_id ---

    #[test]
    fn test_create_clickup_provider_without_team_id() {
        let config = ProviderConfig::ClickUp {
            access_token: "pk_test".into(),
            scope: ClickUpScope::List {
                id: "list999".into(),
                team_id: None,
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

    // --- Enricher for Custom is None ---

    #[test]
    fn test_create_enricher_custom_returns_none() {
        let config = ProviderConfig::Custom {
            name: "custom-plugin".into(),
            config: HashMap::new(),
        };
        assert!(create_enricher(&config, None).is_none());
    }

    #[test]
    fn test_create_enricher_telegram_returns_none() {
        let config = ProviderConfig::Telegram {
            base_url: "https://api.telegram.org".into(),
            access_token: "bot-token".into(),
            scope: TelegramScope::Bot {
                bot_username: Some("devboy_bot".into()),
            },
            extra: HashMap::new(),
        };
        assert!(create_enricher(&config, None).is_none());
    }

    #[test]
    fn test_create_knowledge_base_enricher_non_kb_returns_none() {
        let config = ProviderConfig::GitHub {
            base_url: "https://api.github.com".into(),
            access_token: "tok".into(),
            scope: GitHubScope::Repository {
                owner: "test".into(),
                repo: "test".into(),
            },
            extra: HashMap::new(),
        };
        assert!(create_knowledge_base_enricher(&config).is_none());
    }

    // --- Enricher with invalid metadata ---

    #[test]
    fn test_create_enricher_clickup_invalid_metadata_returns_none() {
        let config = ProviderConfig::ClickUp {
            access_token: "token".into(),
            scope: ClickUpScope::List {
                id: "list1".into(),
                team_id: None,
            },
            extra: HashMap::new(),
        };
        let meta = ProviderMetadata::new(serde_json::json!("invalid_data"));
        assert!(create_enricher(&config, Some(&meta)).is_none());
    }

    #[test]
    fn test_create_enricher_jira_invalid_metadata_returns_none() {
        let config = ProviderConfig::Jira {
            base_url: "https://test.atlassian.net".into(),
            access_token: "token".into(),
            email: "a@b.com".into(),
            scope: JiraScope::Project { key: "X".into() },
            flavor: None,
            extra: HashMap::new(),
        };
        let meta = ProviderMetadata::new(serde_json::json!("not_valid"));
        assert!(create_enricher(&config, Some(&meta)).is_none());
    }
}
