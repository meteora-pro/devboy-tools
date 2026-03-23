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
