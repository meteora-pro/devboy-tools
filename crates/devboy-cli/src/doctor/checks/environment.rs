use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};
use async_trait::async_trait;
use serde_json::json;

pub struct OsSupportCheck;
pub struct ConfigDirCheck;
pub struct CredentialStoreCheck;

#[async_trait]
impl DiagnosticCheck for OsSupportCheck {
    fn id(&self) -> &'static str {
        "environment.os_support"
    }

    fn name(&self) -> &'static str {
        "Operating system supported"
    }

    fn category(&self) -> &'static str {
        "Environment"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        let os = std::env::consts::OS;
        let supported = matches!(os, "windows" | "macos" | "linux");

        CheckResult {
            id: self.id().to_string(),
            category: self.category().to_string(),
            name: self.name().to_string(),
            status: if supported {
                CheckStatus::Pass
            } else {
                CheckStatus::Warning
            },
            message: if supported {
                format!("Operating system supported ({os})")
            } else {
                format!("Operating system may not be fully supported ({os})")
            },
            details: ctx.verbose.then(|| json!({ "os": os })),
            fix_command: None,
            fix_url: None,
        }
    }
}

#[async_trait]
impl DiagnosticCheck for ConfigDirCheck {
    fn id(&self) -> &'static str {
        "environment.config_dir"
    }

    fn name(&self) -> &'static str {
        "Config directory exists"
    }

    fn category(&self) -> &'static str {
        "Environment"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        match devboy_core::Config::config_dir() {
            Ok(path) => {
                let exists = path.exists();
                CheckResult {
                    id: self.id().to_string(),
                    category: self.category().to_string(),
                    name: self.name().to_string(),
                    status: if exists {
                        CheckStatus::Pass
                    } else {
                        CheckStatus::Warning
                    },
                    message: if exists {
                        format!("Config directory exists ({})", path.display())
                    } else {
                        format!("Config directory missing ({})", path.display())
                    },
                    details: ctx
                        .verbose
                        .then(|| json!({ "path": path, "exists": exists })),
                    fix_command: (!exists).then(|| "devboy init".to_string()),
                    fix_url: None,
                }
            }
            Err(error) => CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Error,
                message: format!("Could not determine config directory: {error}"),
                details: ctx.verbose.then(|| json!({ "error": error.to_string() })),
                fix_command: None,
                fix_url: None,
            },
        }
    }
}

#[async_trait]
impl DiagnosticCheck for CredentialStoreCheck {
    fn id(&self) -> &'static str {
        "environment.credential_store"
    }

    fn name(&self) -> &'static str {
        "Credential store available"
    }

    fn category(&self) -> &'static str {
        "Environment"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        match ctx.credential_store.get("__devboy_doctor_probe__") {
            Ok(_) => CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Pass,
                message: "Credential store available".to_string(),
                details: ctx.verbose.then(|| json!({ "backend": "os-keychain" })),
                fix_command: None,
                fix_url: None,
            },
            Err(error) => CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Error,
                message: format!("Credential store unavailable: {error}"),
                details: ctx.verbose.then(|| json!({ "error": error.to_string() })),
                fix_command: None,
                fix_url: None,
            },
        }
    }
}
