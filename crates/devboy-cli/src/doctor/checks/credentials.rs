use crate::doctor::checks::{
    DEFAULT_CONTEXT_NAME, resolve_active_provider_context, resolve_secret,
};
use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};
use async_trait::async_trait;
use devboy_core::ContextConfig;
use serde_json::json;

pub struct GitHubTokenCheck;
pub struct GitLabTokenCheck;
pub struct ClickUpTokenCheck;
pub struct JiraTokenCheck;

struct CredentialSpec {
    provider: &'static str,
    label: &'static str,
    help_key: &'static str,
    format_hint: &'static str,
    validator: fn(&str) -> TokenFormat,
}

enum TokenFormat {
    Recognized,
    Suspicious(&'static str),
}

impl CredentialSpec {
    fn build_result(
        &self,
        check: &dyn DiagnosticCheck,
        ctx: &DiagnosticContext,
        configured: bool,
    ) -> CheckResult {
        let Some(config) = &ctx.config else {
            return skipped(check, "Skipped because config could not be loaded");
        };

        let Some(active) = resolve_active_provider_context(config) else {
            return skipped(check, "Skipped because no active context could be resolved");
        };

        if !configured {
            return skipped(
                check,
                &format!(
                    "Skipped because {} is not configured in context '{}'",
                    self.label, active.name
                ),
            );
        }

        match resolve_secret(ctx, Some(&active.name), self.provider) {
            Ok(Some(secret)) => {
                let format = (self.validator)(&secret.value);
                let status = match format {
                    TokenFormat::Recognized => CheckStatus::Pass,
                    TokenFormat::Suspicious(_) => CheckStatus::Warning,
                };
                let message = match format {
                    TokenFormat::Recognized => format!("{} token found", self.label),
                    TokenFormat::Suspicious(reason) => {
                        format!(
                            "{} token found but format looks unusual ({reason})",
                            self.label
                        )
                    }
                };

                CheckResult {
                    id: check.id().to_string(),
                    category: check.category().to_string(),
                    name: check.name().to_string(),
                    status,
                    message,
                    details: ctx.verbose.then(|| {
                        json!({
                            "provider": self.provider,
                            "context": active.name,
                            "token_key": secret.key,
                            "token_source": secret.source,
                            "format_hint": self.format_hint,
                        })
                    }),
                    fix_command: None,
                    fix_url: None,
                }
            }
            Ok(None) => {
                let secret_key = if active.name == DEFAULT_CONTEXT_NAME {
                    self.help_key.to_string()
                } else {
                    format!("contexts.{}.{}", active.name, self.help_key)
                };

                CheckResult {
                    id: check.id().to_string(),
                    category: check.category().to_string(),
                    name: check.name().to_string(),
                    status: CheckStatus::Error,
                    message: format!("{} token missing", self.label),
                    details: ctx.verbose.then(|| {
                        json!({
                            "provider": self.provider,
                            "context": active.name,
                            "expected_secret_key": secret_key,
                            "token_source_order": ["context", "global"],
                        })
                    }),
                    fix_command: Some(format!("devboy config set-secret {secret_key} <TOKEN>")),
                    fix_url: None,
                }
            }
            Err(error) => CheckResult {
                id: check.id().to_string(),
                category: check.category().to_string(),
                name: check.name().to_string(),
                status: CheckStatus::Error,
                message: format!("Could not read {} token: {error}", self.label),
                details: ctx.verbose.then(|| {
                    json!({
                        "provider": self.provider,
                        "context": active.name,
                        "error": error,
                    })
                }),
                fix_command: None,
                fix_url: None,
            },
        }
    }
}

fn skipped(check: &dyn DiagnosticCheck, message: &str) -> CheckResult {
    CheckResult {
        id: check.id().to_string(),
        category: check.category().to_string(),
        name: check.name().to_string(),
        status: CheckStatus::Skipped,
        message: message.to_string(),
        details: None,
        fix_command: None,
        fix_url: None,
    }
}

fn github_configured(context: &ContextConfig) -> bool {
    context.github.is_some()
}

fn gitlab_configured(context: &ContextConfig) -> bool {
    context.gitlab.is_some()
}

fn clickup_configured(context: &ContextConfig) -> bool {
    context.clickup.is_some()
}

fn jira_configured(context: &ContextConfig) -> bool {
    context.jira.is_some()
}

fn validate_github_token(token: &str) -> TokenFormat {
    let token = token.trim();
    if token.starts_with("ghp_")
        || token.starts_with("github_pat_")
        || token.starts_with("gho_")
        || token.starts_with("ghu_")
        || token.starts_with("ghs_")
        || token.starts_with("ghr_")
    {
        TokenFormat::Recognized
    } else if token.len() >= 20 {
        TokenFormat::Suspicious("expected a GitHub PAT prefix like ghp_ or github_pat_")
    } else {
        TokenFormat::Suspicious("token is shorter than a typical GitHub PAT")
    }
}

fn validate_gitlab_token(token: &str) -> TokenFormat {
    let token = token.trim();
    if token.starts_with("glpat-") {
        TokenFormat::Recognized
    } else if token.len() >= 20 {
        TokenFormat::Suspicious("expected a GitLab personal access token prefix like glpat-")
    } else {
        TokenFormat::Suspicious("token is shorter than a typical GitLab personal access token")
    }
}

fn validate_clickup_token(token: &str) -> TokenFormat {
    let token = token.trim();
    if token.len() >= 24 {
        TokenFormat::Recognized
    } else {
        TokenFormat::Suspicious("token is shorter than a typical ClickUp API token")
    }
}

fn validate_jira_token(token: &str) -> TokenFormat {
    let token = token.trim();
    if token.contains(':') || token.len() >= 16 {
        TokenFormat::Recognized
    } else {
        TokenFormat::Suspicious("token is shorter than a typical Jira API token or credential pair")
    }
}

#[async_trait]
impl DiagnosticCheck for GitHubTokenCheck {
    fn id(&self) -> &'static str {
        "credentials.github"
    }

    fn name(&self) -> &'static str {
        "GitHub token available"
    }

    fn category(&self) -> &'static str {
        "Credentials"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        let spec = CredentialSpec {
            provider: "github",
            label: "GitHub",
            help_key: "github.token",
            format_hint: "GitHub personal access token",
            validator: validate_github_token,
        };

        let configured = ctx
            .config
            .as_ref()
            .and_then(resolve_active_provider_context)
            .is_some_and(|active| github_configured(&active.config));

        spec.build_result(self, ctx, configured)
    }
}

#[async_trait]
impl DiagnosticCheck for GitLabTokenCheck {
    fn id(&self) -> &'static str {
        "credentials.gitlab"
    }

    fn name(&self) -> &'static str {
        "GitLab token available"
    }

    fn category(&self) -> &'static str {
        "Credentials"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        let spec = CredentialSpec {
            provider: "gitlab",
            label: "GitLab",
            help_key: "gitlab.token",
            format_hint: "GitLab personal access token",
            validator: validate_gitlab_token,
        };

        let configured = ctx
            .config
            .as_ref()
            .and_then(resolve_active_provider_context)
            .is_some_and(|active| gitlab_configured(&active.config));

        spec.build_result(self, ctx, configured)
    }
}

#[async_trait]
impl DiagnosticCheck for ClickUpTokenCheck {
    fn id(&self) -> &'static str {
        "credentials.clickup"
    }

    fn name(&self) -> &'static str {
        "ClickUp token available"
    }

    fn category(&self) -> &'static str {
        "Credentials"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        let spec = CredentialSpec {
            provider: "clickup",
            label: "ClickUp",
            help_key: "clickup.token",
            format_hint: "ClickUp API token",
            validator: validate_clickup_token,
        };

        let configured = ctx
            .config
            .as_ref()
            .and_then(resolve_active_provider_context)
            .is_some_and(|active| clickup_configured(&active.config));

        spec.build_result(self, ctx, configured)
    }
}

#[async_trait]
impl DiagnosticCheck for JiraTokenCheck {
    fn id(&self) -> &'static str {
        "credentials.jira"
    }

    fn name(&self) -> &'static str {
        "Jira token available"
    }

    fn category(&self) -> &'static str {
        "Credentials"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        let spec = CredentialSpec {
            provider: "jira",
            label: "Jira",
            help_key: "jira.token",
            format_hint: "Jira API token or self-hosted user:password pair",
            validator: validate_jira_token,
        };

        let configured = ctx
            .config
            .as_ref()
            .and_then(resolve_active_provider_context)
            .is_some_and(|active| jira_configured(&active.config));

        spec.build_result(self, ctx, configured)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::DiagnosticContext;
    use devboy_core::{
        ClickUpConfig, Config, ContextConfig, Error, GitHubConfig, GitLabConfig, JiraConfig,
    };
    use devboy_storage::{CredentialStore, MemoryStore};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[derive(Debug)]
    struct FailingStore;

    impl CredentialStore for FailingStore {
        fn store(&self, _key: &str, _value: &str) -> devboy_core::Result<()> {
            Err(Error::Storage("store failed".to_string()))
        }

        fn get(&self, _key: &str) -> devboy_core::Result<Option<String>> {
            Err(Error::Storage("credential backend unavailable".to_string()))
        }

        fn delete(&self, _key: &str) -> devboy_core::Result<()> {
            Err(Error::Storage("delete failed".to_string()))
        }
    }

    fn context_with_store(
        store: Arc<dyn CredentialStore>,
        context: ContextConfig,
    ) -> DiagnosticContext {
        let mut contexts = BTreeMap::new();
        contexts.insert("workspace".to_string(), context);

        DiagnosticContext {
            config: Some(Config {
                contexts,
                active_context: Some("workspace".to_string()),
                ..Default::default()
            }),
            config_path: Some(PathBuf::from("config.toml")),
            config_exists: true,
            config_source: "test",
            config_path_error: None,
            config_load_error: None,
            credential_store: store,
            verbose: true,
        }
    }

    fn github_context() -> ContextConfig {
        ContextConfig {
            github: Some(GitHubConfig {
                owner: "owner".to_string(),
                repo: "repo".to_string(),
                base_url: None,
            }),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn github_token_check_prefers_context_secret() {
        let store = Arc::new(MemoryStore::with_credentials([
            (
                "contexts.workspace.github.token".to_string(),
                "ghp_context_token_1234567890".to_string(),
            ),
            (
                "github.token".to_string(),
                "ghp_global_token_1234567890".to_string(),
            ),
        ]));
        let ctx = context_with_store(store, github_context());

        let result = GitHubTokenCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Pass);
        let details = result.details.unwrap();
        assert_eq!(details["token_source"], "context");
        assert_eq!(details["token_key"], "contexts.workspace.github.token");
    }

    #[tokio::test]
    async fn github_token_check_errors_when_missing() {
        let ctx = context_with_store(Arc::new(MemoryStore::new()), github_context());

        let result = GitHubTokenCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Error);
        assert_eq!(
            result.fix_command.as_deref(),
            Some("devboy config set-secret contexts.workspace.github.token <TOKEN>")
        );
    }

    #[tokio::test]
    async fn github_token_check_warns_for_suspicious_global_token() {
        let store = Arc::new(MemoryStore::with_credentials([(
            "github.token".to_string(),
            "token-without-known-prefix-but-long".to_string(),
        )]));
        let ctx = context_with_store(store, github_context());

        let result = GitHubTokenCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Warning);
        assert_eq!(result.details.unwrap()["token_source"], "global");
    }

    #[tokio::test]
    async fn gitlab_token_check_passes_for_recognized_token() {
        let store = Arc::new(MemoryStore::with_credentials([(
            "contexts.workspace.gitlab.token".to_string(),
            "glpat-12345678901234567890".to_string(),
        )]));
        let ctx = context_with_store(
            store,
            ContextConfig {
                gitlab: Some(GitLabConfig {
                    url: "https://gitlab.example.com".to_string(),
                    project_id: "group/project".to_string(),
                }),
                ..Default::default()
            },
        );

        let result = GitLabTokenCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.details.unwrap()["provider"], "gitlab");
    }

    #[tokio::test]
    async fn clickup_token_check_warns_for_short_token() {
        let store = Arc::new(MemoryStore::with_credentials([(
            "contexts.workspace.clickup.token".to_string(),
            "short-token".to_string(),
        )]));
        let ctx = context_with_store(
            store,
            ContextConfig {
                clickup: Some(ClickUpConfig {
                    list_id: "123".to_string(),
                    team_id: None,
                }),
                ..Default::default()
            },
        );

        let result = ClickUpTokenCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.message.contains("format looks unusual"));
    }

    #[tokio::test]
    async fn jira_token_check_passes_for_credential_pair() {
        let store = Arc::new(MemoryStore::with_credentials([(
            "contexts.workspace.jira.token".to_string(),
            "user:token".to_string(),
        )]));
        let ctx = context_with_store(
            store,
            ContextConfig {
                jira: Some(JiraConfig {
                    url: "https://jira.example.com".to_string(),
                    project_key: "PROJ".to_string(),
                    email: "jira@example.com".to_string(),
                }),
                ..Default::default()
            },
        );

        let result = JiraTokenCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.details.unwrap()["provider"], "jira");
    }

    #[tokio::test]
    async fn provider_token_checks_skip_when_provider_not_configured() {
        let ctx = context_with_store(Arc::new(MemoryStore::new()), ContextConfig::default());

        let result = GitLabTokenCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Skipped);
        assert!(result.message.contains("not configured"));
    }

    #[tokio::test]
    async fn provider_token_checks_skip_when_active_context_is_missing() {
        let ctx = DiagnosticContext {
            config: Some(Config::default()),
            config_path: Some(PathBuf::from("config.toml")),
            config_exists: true,
            config_source: "test",
            config_path_error: None,
            config_load_error: None,
            credential_store: Arc::new(MemoryStore::new()),
            verbose: true,
        };

        let result = GitHubTokenCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Skipped);
        assert!(result.message.contains("no active context"));
    }

    #[tokio::test]
    async fn provider_token_checks_error_when_store_fails() {
        let ctx = context_with_store(Arc::new(FailingStore), github_context());

        let result = GitHubTokenCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("Could not read GitHub token"));
        assert_eq!(
            result.details.unwrap()["error"],
            "Storage error: credential backend unavailable"
        );
    }

    #[test]
    fn token_format_validators_detect_suspicious_values() {
        assert!(matches!(
            validate_github_token("short"),
            TokenFormat::Suspicious(_)
        ));
        assert!(matches!(
            validate_gitlab_token("token-without-prefix-but-long-enough"),
            TokenFormat::Suspicious(_)
        ));
        assert!(matches!(
            validate_clickup_token("123"),
            TokenFormat::Suspicious(_)
        ));
        assert!(matches!(
            validate_jira_token("token"),
            TokenFormat::Suspicious(_)
        ));
        assert!(matches!(
            validate_github_token("ghp_12345678901234567890"),
            TokenFormat::Recognized
        ));
        assert!(matches!(
            validate_gitlab_token("glpat-12345678901234567890"),
            TokenFormat::Recognized
        ));
        assert!(matches!(
            validate_clickup_token("123456789012345678901234"),
            TokenFormat::Recognized
        ));
        assert!(matches!(
            validate_jira_token("1234567890123456"),
            TokenFormat::Recognized
        ));
    }
}
