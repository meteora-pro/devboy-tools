use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};
use async_trait::async_trait;
use serde_json::json;

pub struct ConfigExistsCheck;
pub struct ConfigValidTomlCheck;
pub struct ActiveContextCheck;

#[async_trait]
impl DiagnosticCheck for ConfigExistsCheck {
    fn id(&self) -> &'static str {
        "config.exists"
    }

    fn name(&self) -> &'static str {
        "Config file exists"
    }

    fn category(&self) -> &'static str {
        "Configuration"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        match &ctx.config_path {
            Some(path) if ctx.config_exists => CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Pass,
                message: format!("Config file found ({})", path.display()),
                details: ctx
                    .verbose
                    .then(|| json!({ "path": path, "source": ctx.config_source })),
                fix_command: None,
                fix_url: None,
            },
            Some(path) => CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Warning,
                message: format!("Config file missing ({})", path.display()),
                details: ctx
                    .verbose
                    .then(|| json!({ "path": path, "source": ctx.config_source })),
                fix_command: Some("devboy init".to_string()),
                fix_url: None,
            },
            None => CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Error,
                message: "Could not determine config file path".to_string(),
                details: ctx
                    .verbose
                    .then(|| json!({ "error": ctx.config_path_error })),
                fix_command: None,
                fix_url: None,
            },
        }
    }
}

#[async_trait]
impl DiagnosticCheck for ConfigValidTomlCheck {
    fn id(&self) -> &'static str {
        "config.valid_toml"
    }

    fn name(&self) -> &'static str {
        "Config file valid TOML"
    }

    fn category(&self) -> &'static str {
        "Configuration"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        if !ctx.config_exists {
            return CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Skipped,
                message: "Skipped because no config file was found".to_string(),
                details: None,
                fix_command: None,
                fix_url: None,
            };
        }

        match (&ctx.config, &ctx.config_load_error) {
            (Some(_), _) => CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Pass,
                message: "Config file parsed successfully".to_string(),
                details: ctx.verbose.then(|| {
                    json!({
                        "path": ctx.config_path,
                        "source": ctx.config_source,
                    })
                }),
                fix_command: None,
                fix_url: None,
            },
            (_, Some(error)) => CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Error,
                message: format!("Config file is invalid: {error}"),
                details: ctx.verbose.then(|| json!({ "error": error })),
                fix_command: None,
                fix_url: None,
            },
            _ => CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Error,
                message: "Config file could not be loaded".to_string(),
                details: None,
                fix_command: None,
                fix_url: None,
            },
        }
    }
}

#[async_trait]
impl DiagnosticCheck for ActiveContextCheck {
    fn id(&self) -> &'static str {
        "config.active_context"
    }

    fn name(&self) -> &'static str {
        "Active context valid"
    }

    fn category(&self) -> &'static str {
        "Configuration"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        let Some(config) = &ctx.config else {
            return CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Skipped,
                message: "Skipped because config could not be loaded".to_string(),
                details: None,
                fix_command: None,
                fix_url: None,
            };
        };

        match config.resolve_active_context_name() {
            Some(active) => CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Pass,
                message: format!("Active context: {active}"),
                details: ctx.verbose.then(|| {
                    json!({
                        "active_context": active,
                        "contexts": config.context_names(),
                    })
                }),
                fix_command: None,
                fix_url: None,
            },
            None => CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Warning,
                message: "No active context could be resolved".to_string(),
                details: ctx.verbose.then(|| {
                    json!({
                        "active_context": config.active_context,
                        "contexts": config.context_names(),
                    })
                }),
                fix_command: Some("devboy init".to_string()),
                fix_url: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::{Config, ContextConfig, GitHubConfig};
    use devboy_storage::MemoryStore;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_context(
        config: Option<Config>,
        config_path: Option<PathBuf>,
        config_exists: bool,
        config_path_error: Option<&str>,
        config_load_error: Option<&str>,
        verbose: bool,
    ) -> DiagnosticContext {
        DiagnosticContext {
            config,
            config_path,
            config_exists,
            config_source: "test",
            config_path_error: config_path_error.map(ToString::to_string),
            config_load_error: config_load_error.map(ToString::to_string),
            credential_store: Arc::new(MemoryStore::new()),
            verbose,
        }
    }

    fn config_with_active_context() -> Config {
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

        Config {
            contexts,
            active_context: Some("workspace".to_string()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn config_exists_check_passes_when_file_exists() {
        let ctx = test_context(
            Some(Config::default()),
            Some(PathBuf::from("config.toml")),
            true,
            None,
            None,
            true,
        );

        let result = ConfigExistsCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("Config file found"));
        assert_eq!(result.details.unwrap()["source"], "test");
    }

    #[tokio::test]
    async fn config_exists_check_errors_when_path_is_unknown() {
        let ctx = test_context(None, None, false, Some("no path"), None, true);

        let result = ConfigExistsCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Error);
        assert_eq!(result.details.unwrap()["error"], "no path");
    }

    #[tokio::test]
    async fn config_valid_toml_check_skips_when_config_missing() {
        let ctx = test_context(
            None,
            Some(PathBuf::from("missing.toml")),
            false,
            None,
            None,
            false,
        );

        let result = ConfigValidTomlCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Skipped);
        assert!(result
            .message
            .contains("Skipped because no config file was found"));
    }

    #[tokio::test]
    async fn config_valid_toml_check_passes_when_config_loaded() {
        let ctx = test_context(
            Some(Config::default()),
            Some(PathBuf::from("config.toml")),
            true,
            None,
            None,
            true,
        );

        let result = ConfigValidTomlCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.details.unwrap()["source"], "test");
    }

    #[tokio::test]
    async fn config_valid_toml_check_errors_when_load_fails_without_parse_error() {
        let ctx = test_context(
            None,
            Some(PathBuf::from("config.toml")),
            true,
            None,
            None,
            false,
        );

        let result = ConfigValidTomlCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Error);
        assert_eq!(result.message, "Config file could not be loaded");
    }

    #[tokio::test]
    async fn active_context_check_skips_when_config_missing() {
        let ctx = test_context(
            None,
            Some(PathBuf::from("config.toml")),
            true,
            None,
            None,
            false,
        );

        let result = ActiveContextCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Skipped);
    }

    #[tokio::test]
    async fn active_context_check_warns_when_no_context_resolves() {
        let ctx = test_context(
            Some(Config::default()),
            Some(PathBuf::from("config.toml")),
            true,
            None,
            None,
            true,
        );

        let result = ActiveContextCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Warning);
        assert_eq!(result.fix_command.as_deref(), Some("devboy init"));
        assert_eq!(
            result.details.unwrap()["contexts"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn active_context_check_passes_when_context_resolves() {
        let ctx = test_context(
            Some(config_with_active_context()),
            Some(PathBuf::from("config.toml")),
            true,
            None,
            None,
            true,
        );

        let result = ActiveContextCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("workspace"));
        assert_eq!(result.details.unwrap()["active_context"], "workspace");
    }
}
