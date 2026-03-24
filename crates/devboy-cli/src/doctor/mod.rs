mod checks;
mod output;

use self::checks::config::{ActiveContextCheck, ConfigExistsCheck, ConfigValidTomlCheck};
use self::checks::credentials::{
    ClickUpTokenCheck, GitHubTokenCheck, GitLabTokenCheck, JiraTokenCheck,
};
use self::checks::environment::{ConfigDirCheck, CredentialStoreCheck, OsSupportCheck};
use self::checks::mcp::McpToolsCheck;
use self::checks::providers::{ClickUpApiCheck, GitHubApiCheck, GitLabApiCheck, JiraApiCheck};
use self::checks::proxy::ProxyServersCheck;
use self::output::console::{print_report, summarize};
use crate::get_credential_store;
use anyhow::Result;
use async_trait::async_trait;
use devboy_core::Config;
use devboy_storage::CredentialStore;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub id: String,
    pub category: String,
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    pub details: Option<Value>,
    pub fix_command: Option<String>,
    pub fix_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Warning,
    Error,
    Skipped,
}

#[async_trait]
pub trait DiagnosticCheck: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn category(&self) -> &'static str;
    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult;
}

pub struct DiagnosticContext {
    pub config: Option<Config>,
    pub config_path: Option<PathBuf>,
    pub config_exists: bool,
    pub config_source: &'static str,
    pub config_path_error: Option<String>,
    pub config_load_error: Option<String>,
    pub credential_store: Arc<dyn CredentialStore>,
    pub verbose: bool,
}

impl DiagnosticContext {
    pub fn load(verbose: bool) -> Self {
        let local_path = PathBuf::from(".devboy.toml");
        let mut config_source = "global";
        let (config_path, config_path_error) = if local_path.exists() {
            config_source = "local";
            (Some(local_path), None)
        } else {
            match Config::config_path() {
                Ok(path) => (Some(path), None),
                Err(error) => (None, Some(error.to_string())),
            }
        };

        let config_exists = config_path.as_ref().is_some_and(|path| path.exists());
        let (config, config_load_error) = if config_exists {
            match std::fs::read_to_string(config_path.as_ref().unwrap()) {
                Ok(contents) => match toml::from_str::<Config>(&contents) {
                    Ok(config) => (Some(config), None),
                    Err(error) => (None, Some(format!("Failed to parse config file: {error}"))),
                },
                Err(error) => (None, Some(format!("Failed to read config file: {error}"))),
            }
        } else {
            (None, None)
        };

        Self {
            config,
            config_path,
            config_exists,
            config_source,
            config_path_error,
            config_load_error,
            credential_store: Arc::from(get_credential_store()),
            verbose,
        }
    }
}

pub struct CheckRegistry {
    checks: Vec<Box<dyn DiagnosticCheck>>,
}

impl CheckRegistry {
    pub fn new() -> Self {
        let mut registry = Self { checks: Vec::new() };
        registry.register(Box::new(OsSupportCheck));
        registry.register(Box::new(ConfigDirCheck));
        registry.register(Box::new(CredentialStoreCheck));
        registry.register(Box::new(ConfigExistsCheck));
        registry.register(Box::new(ConfigValidTomlCheck));
        registry.register(Box::new(ActiveContextCheck));
        registry.register(Box::new(GitHubTokenCheck));
        registry.register(Box::new(GitLabTokenCheck));
        registry.register(Box::new(ClickUpTokenCheck));
        registry.register(Box::new(JiraTokenCheck));
        registry.register(Box::new(GitHubApiCheck));
        registry.register(Box::new(GitLabApiCheck));
        registry.register(Box::new(ClickUpApiCheck));
        registry.register(Box::new(JiraApiCheck));
        registry.register(Box::new(McpToolsCheck));
        registry.register(Box::new(ProxyServersCheck));
        registry
    }

    fn register(&mut self, check: Box<dyn DiagnosticCheck>) {
        self.checks.push(check);
    }

    pub async fn run_all(&self, ctx: &DiagnosticContext) -> Vec<CheckResult> {
        let mut results = Vec::with_capacity(self.checks.len());
        for check in &self.checks {
            results.push(check.run(ctx).await);
        }
        results
    }
}

pub async fn handle_doctor_command(verbose: bool) -> Result<i32> {
    let ctx = DiagnosticContext::load(verbose);
    let registry = CheckRegistry::new();
    let results = registry.run_all(&ctx).await;

    print_report(&results, verbose);

    let summary = summarize(&results);
    let exit_code = if summary.errors > 0 {
        2
    } else if summary.warnings > 0 {
        1
    } else {
        0
    };

    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::checks::config::{
        ActiveContextCheck, ConfigExistsCheck, ConfigValidTomlCheck,
    };
    use crate::doctor::checks::environment::CredentialStoreCheck;
    use devboy_core::{ContextConfig, GitHubConfig};
    use devboy_storage::MemoryStore;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    fn test_context(
        config: Option<Config>,
        config_path: Option<PathBuf>,
        config_exists: bool,
        config_load_error: Option<&str>,
    ) -> DiagnosticContext {
        DiagnosticContext {
            config,
            config_path,
            config_exists,
            config_source: "test",
            config_path_error: None,
            config_load_error: config_load_error.map(ToString::to_string),
            credential_store: Arc::new(MemoryStore::new()),
            verbose: false,
        }
    }

    #[tokio::test]
    async fn config_exists_check_warns_when_missing() {
        let ctx = test_context(None, Some(PathBuf::from("missing.toml")), false, None);
        let result = ConfigExistsCheck.run(&ctx).await;
        assert_eq!(result.status, CheckStatus::Warning);
        assert_eq!(result.fix_command.as_deref(), Some("devboy init"));
    }

    #[tokio::test]
    async fn config_valid_toml_check_errors_on_parse_failure() {
        let ctx = test_context(
            None,
            Some(PathBuf::from("bad.toml")),
            true,
            Some("Failed to parse config file"),
        );
        let result = ConfigValidTomlCheck.run(&ctx).await;
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("invalid"));
    }

    #[tokio::test]
    async fn active_context_check_passes_when_context_resolves() {
        let mut contexts = BTreeMap::new();
        contexts.insert(
            "workspace".to_string(),
            ContextConfig {
                github: Some(GitHubConfig {
                    owner: "owner".to_string(),
                    repo: "repo".to_string(),
                    base_url: None,
                }),
                ..Default::default()
            },
        );

        let config = Config {
            contexts,
            active_context: Some("workspace".to_string()),
            ..Default::default()
        };

        let ctx = test_context(Some(config), Some(PathBuf::from("config.toml")), true, None);
        let result = ActiveContextCheck.run(&ctx).await;
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("workspace"));
    }

    #[tokio::test]
    async fn credential_store_check_uses_store_probe() {
        let ctx = test_context(None, None, false, None);
        let result = CredentialStoreCheck.run(&ctx).await;
        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[test]
    fn diagnostic_context_prefers_local_config() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".devboy.toml"),
            "[github]\nowner='o'\nrepo='r'\n",
        )
        .unwrap();

        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let ctx = DiagnosticContext::load(false);

        std::env::set_current_dir(previous).unwrap();

        assert_eq!(ctx.config_source, "local");
        assert!(ctx.config_exists);
        assert!(ctx.config.is_some());
    }
}
