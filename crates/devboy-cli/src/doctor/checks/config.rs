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
            // Issue #229: a `[remote_config]`-only or
            // `[[proxy_mcp_servers]]` install is the supported "thin
            // client" mode — the actual tool surface is exposed by the
            // remote MCP server we proxy to. `DEVBOY_REMOTE_CONFIG_URL`
            // env var is also respected even when `[remote_config]` is
            // absent (see `devboy_core::remote_config::resolve_url`).
            // There are no local contexts by design; the generic "no
            // context" warning would otherwise loop the user into
            // re-running `devboy init`.
            None if devboy_core::remote_config::resolve_url(config).is_some()
                || !config.proxy_mcp_servers.is_empty() =>
            {
                let proxy_names: Vec<String> = config
                    .proxy_mcp_servers
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                let resolved_url = devboy_core::remote_config::resolve_url(config);
                let redacted = resolved_url
                    .as_deref()
                    .map(devboy_core::remote_config::redact_url_for_display);
                let message = match (&redacted, proxy_names.is_empty()) {
                    (Some(url), _) => format!(
                        "Remote-config install detected; local contexts are not required (url: {url})"
                    ),
                    (None, false) => format!(
                        "Proxy MCP install detected; local contexts are not required (servers: {})",
                        proxy_names.join(", ")
                    ),
                    (None, true) => unreachable!("guarded by `if` arm"),
                };
                CheckResult {
                    id: self.id().to_string(),
                    category: self.category().to_string(),
                    name: self.name().to_string(),
                    status: CheckStatus::Pass,
                    message,
                    details: ctx.verbose.then(|| {
                        json!({
                            // Note: redacted (userinfo + query stripped) for
                            // safe display in `--verbose` output too.
                            "remote_config_url": redacted,
                            "proxy_mcp_servers": proxy_names,
                        })
                    }),
                    fix_command: None,
                    fix_url: None,
                }
            }
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
        assert!(
            result
                .message
                .contains("Skipped because no config file was found")
        );
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

    /// Issue #229: a `[remote_config]`-only install (the supported
    /// thin-client / proxy mode) must not warn about a missing context
    /// nor suggest re-running `devboy init` (which would loop).
    #[tokio::test]
    async fn active_context_check_passes_for_remote_config_install() {
        use devboy_core::config::RemoteConfigSettings;

        let config = Config {
            remote_config: Some(RemoteConfigSettings {
                url: Some("https://example.com/api/config/mcp".to_string()),
                token_key: Some("remote_config.token".to_string()),
            }),
            ..Default::default()
        };
        let ctx = test_context(
            Some(config),
            Some(PathBuf::from("config.toml")),
            true,
            None,
            None,
            true,
        );

        let result = ActiveContextCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("Remote-config"));
        assert!(
            result
                .message
                .contains("https://example.com/api/config/mcp")
        );
        assert!(result.fix_command.is_none());
    }

    /// Issue #229: same goes for an install where the runtime fetch
    /// has already populated `[[proxy_mcp_servers]]` (no
    /// `[remote_config]` block, but the proxy is configured).
    #[tokio::test]
    async fn active_context_check_passes_when_proxy_servers_configured() {
        use devboy_core::config::ProxyMcpServerConfig;

        let config = Config {
            proxy_mcp_servers: vec![ProxyMcpServerConfig {
                name: "remote-1".to_string(),
                url: "https://example.com/api/mcp".to_string(),
                auth_type: "none".to_string(),
                token_key: None,
                tool_prefix: None,
                transport: "streamable-http".to_string(),
                routing: None,
            }],
            ..Default::default()
        };
        let ctx = test_context(
            Some(config),
            Some(PathBuf::from("config.toml")),
            true,
            None,
            None,
            true,
        );

        let result = ActiveContextCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.fix_command.is_none());
    }
}
