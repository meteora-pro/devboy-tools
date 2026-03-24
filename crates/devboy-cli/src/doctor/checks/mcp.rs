use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};
use async_trait::async_trait;
use devboy_mcp::KNOWN_BUILTIN_TOOLS;
use serde_json::json;

pub struct McpToolsCheck;

#[async_trait]
impl DiagnosticCheck for McpToolsCheck {
    fn id(&self) -> &'static str {
        "mcp.tools"
    }

    fn name(&self) -> &'static str {
        "Built-in MCP tools configuration"
    }

    fn category(&self) -> &'static str {
        "MCP Server"
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

        let builtin = &config.builtin_tools;
        if let Err(error) = builtin.validate() {
            return CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Error,
                message: format!("Built-in MCP tools configuration is invalid: {error}"),
                details: ctx.verbose.then(|| {
                    json!({
                        "disabled": builtin.disabled,
                        "enabled": builtin.enabled,
                    })
                }),
                fix_command: None,
                fix_url: None,
            };
        }

        let unknown_tools: Vec<String> = builtin
            .disabled
            .iter()
            .chain(builtin.enabled.iter())
            .filter(|name| {
                !KNOWN_BUILTIN_TOOLS
                    .iter()
                    .any(|known| known == &name.as_str())
            })
            .cloned()
            .collect();

        let mode = if !builtin.enabled.is_empty() {
            "whitelist"
        } else if !builtin.disabled.is_empty() {
            "blacklist"
        } else {
            "default"
        };

        let status = if unknown_tools.is_empty() {
            CheckStatus::Pass
        } else {
            CheckStatus::Warning
        };

        let message = if !unknown_tools.is_empty() {
            format!(
                "Built-in MCP tools configuration contains {} unknown tool name(s)",
                unknown_tools.len()
            )
        } else if mode == "default" {
            "Built-in MCP tools configuration uses the default tool set".to_string()
        } else if mode == "whitelist" {
            format!(
                "Built-in MCP tools whitelist is valid ({} enabled)",
                builtin.enabled.len()
            )
        } else {
            format!(
                "Built-in MCP tools blacklist is valid ({} disabled)",
                builtin.disabled.len()
            )
        };

        CheckResult {
            id: self.id().to_string(),
            category: self.category().to_string(),
            name: self.name().to_string(),
            status,
            message,
            details: ctx.verbose.then(|| {
                json!({
                    "mode": mode,
                    "known_tools_count": KNOWN_BUILTIN_TOOLS.len(),
                    "disabled": builtin.disabled,
                    "enabled": builtin.enabled,
                    "unknown_tools": unknown_tools,
                })
            }),
            fix_command: None,
            fix_url: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::DiagnosticContext;
    use devboy_core::{BuiltinToolsConfig, Config};
    use devboy_storage::MemoryStore;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_context(config: Config, verbose: bool) -> DiagnosticContext {
        DiagnosticContext {
            config: Some(config),
            config_path: Some(PathBuf::from("config.toml")),
            config_exists: true,
            config_source: "test",
            config_path_error: None,
            config_load_error: None,
            credential_store: Arc::new(MemoryStore::new()),
            verbose,
        }
    }

    #[tokio::test]
    async fn mcp_tools_check_passes_for_default_configuration() {
        let ctx = test_context(Config::default(), true);
        let result = McpToolsCheck.run(&ctx).await;
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(result.message.contains("default tool set"));
    }

    #[tokio::test]
    async fn mcp_tools_check_warns_for_unknown_tools() {
        let ctx = test_context(
            Config {
                builtin_tools: BuiltinToolsConfig {
                    disabled: vec!["get_issues".to_string(), "missing_tool".to_string()],
                    enabled: vec![],
                },
                ..Default::default()
            },
            true,
        );

        let result = McpToolsCheck.run(&ctx).await;
        assert_eq!(result.status, CheckStatus::Warning);
        let details = result.details.unwrap();
        assert_eq!(details["unknown_tools"][0], "missing_tool");
    }
}
