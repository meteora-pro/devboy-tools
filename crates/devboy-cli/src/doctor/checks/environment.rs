use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;

pub struct OsSupportCheck;
pub struct ConfigDirCheck;
pub struct CredentialStoreCheck;

fn os_support_result(
    check: &dyn DiagnosticCheck,
    ctx: &DiagnosticContext,
    os: &str,
) -> CheckResult {
    let supported = matches!(os, "windows" | "macos" | "linux");

    CheckResult {
        id: check.id().to_string(),
        category: check.category().to_string(),
        name: check.name().to_string(),
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

fn config_dir_result(
    check: &dyn DiagnosticCheck,
    ctx: &DiagnosticContext,
    path: PathBuf,
) -> CheckResult {
    let exists = path.exists();
    CheckResult {
        id: check.id().to_string(),
        category: check.category().to_string(),
        name: check.name().to_string(),
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
        os_support_result(self, ctx, std::env::consts::OS)
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
            Ok(path) => config_dir_result(self, ctx, path),
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

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::{Config, Error};
    use devboy_storage::{CredentialStore, MemoryStore};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[derive(Debug)]
    struct FailingStore;

    impl CredentialStore for FailingStore {
        fn store(&self, _key: &str, _value: &str) -> devboy_core::Result<()> {
            Err(Error::Storage("store failed".to_string()))
        }

        fn get(&self, _key: &str) -> devboy_core::Result<Option<String>> {
            Err(Error::Storage("store unavailable".to_string()))
        }

        fn delete(&self, _key: &str) -> devboy_core::Result<()> {
            Err(Error::Storage("delete failed".to_string()))
        }
    }

    fn test_context(verbose: bool, store: Arc<dyn CredentialStore>) -> DiagnosticContext {
        DiagnosticContext {
            config: Some(Config::default()),
            config_path: Some(PathBuf::from("config.toml")),
            config_exists: true,
            config_source: "test",
            config_path_error: None,
            config_load_error: None,
            credential_store: store,
            verbose,
        }
    }

    #[tokio::test]
    async fn os_support_check_passes_for_current_os() {
        let ctx = test_context(true, Arc::new(MemoryStore::new()));

        let result = OsSupportCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.details.unwrap()["os"], std::env::consts::OS);
    }

    #[test]
    fn os_support_result_warns_for_unknown_os() {
        let ctx = test_context(true, Arc::new(MemoryStore::new()));

        let result = os_support_result(&OsSupportCheck, &ctx, "plan9");

        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.message.contains("may not be fully supported"));
    }

    #[test]
    fn config_dir_result_passes_when_directory_exists() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_context(true, Arc::new(MemoryStore::new()));

        let result = config_dir_result(&ConfigDirCheck, &ctx, dir.path().to_path_buf());

        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.fix_command, None);
        assert_eq!(result.details.unwrap()["exists"], true);
    }

    #[test]
    fn config_dir_result_warns_when_directory_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let ctx = test_context(true, Arc::new(MemoryStore::new()));

        let result = config_dir_result(&ConfigDirCheck, &ctx, missing.clone());

        assert_eq!(result.status, CheckStatus::Warning);
        assert_eq!(result.fix_command.as_deref(), Some("devboy init"));
        assert_eq!(
            result.details.unwrap()["path"].as_str(),
            Some(missing.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn credential_store_check_uses_probe_successfully() {
        let ctx = test_context(true, Arc::new(MemoryStore::new()));

        let result = CredentialStoreCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(result.details.unwrap()["backend"], "os-keychain");
    }

    #[tokio::test]
    async fn credential_store_check_reports_store_errors() {
        let ctx = test_context(true, Arc::new(FailingStore));

        let result = CredentialStoreCheck.run(&ctx).await;

        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("store unavailable"));
        assert_eq!(
            result.details.unwrap()["error"],
            "Storage error: store unavailable"
        );
    }
}
