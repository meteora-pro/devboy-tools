pub mod config;
pub mod credentials;
pub mod environment;
pub mod mcp;
pub mod providers;
pub mod proxy;
pub mod sources;

use crate::doctor::DiagnosticContext;
use devboy_core::{Config, ContextConfig};
use secrecy::SecretString;

pub(super) const DEFAULT_CONTEXT_NAME: &str = Config::DEFAULT_CONTEXT_NAME;

#[derive(Debug, Clone)]
pub(super) struct ActiveProviderContext {
    pub name: String,
    pub config: ContextConfig,
}

#[derive(Debug, Clone)]
pub(super) struct ResolvedSecret {
    pub key: String,
    pub source: &'static str,
    pub value: SecretString,
}

pub(super) fn resolve_active_provider_context(config: &Config) -> Option<ActiveProviderContext> {
    let name = config.resolve_active_context_name()?;
    let config = config.get_context(&name)?;
    Some(ActiveProviderContext { name, config })
}

pub(super) fn resolve_secret(
    ctx: &DiagnosticContext,
    context_name: Option<&str>,
    provider: &str,
) -> Result<Option<ResolvedSecret>, String> {
    let mut last_error: Option<String> = None;

    if let Some(context_name) = context_name {
        let scoped_key = format!("contexts.{context_name}.{provider}.token");
        match ctx.credential_store.get(&scoped_key) {
            Ok(Some(value)) => {
                return Ok(Some(ResolvedSecret {
                    key: scoped_key,
                    source: "context",
                    value,
                }));
            }
            Ok(None) => {}
            // Scoped lookup failed (no keychain available, ephemeral
            // HOME, etc.). Remember the error but keep trying the
            // global key — the env-var backend may still expose the
            // token via `DEVBOY_<PROVIDER>_TOKEN`. This is the same
            // chain `tools call` uses, and #188/#5 had `doctor`
            // diverging from it (reporting "missing" even though the
            // env var was set and the rest of the CLI could see it).
            Err(error) => last_error = Some(error.to_string()),
        }
    }

    let global_key = format!("{provider}.token");
    match ctx.credential_store.get(&global_key) {
        Ok(Some(value)) => Ok(Some(ResolvedSecret {
            key: global_key,
            source: "global",
            value,
        })),
        // If the global lookup doesn't find anything *and* the scoped
        // lookup erred earlier, surface that error — it is strictly
        // more informative than "token missing". If both probes
        // returned `None`, that's the real "missing" answer.
        Ok(None) => match last_error {
            Some(e) => Err(e),
            None => Ok(None),
        },
        Err(error) => Err(last_error.unwrap_or_else(|| error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::{Config, ContextConfig, Error, GitHubConfig};
    use devboy_storage::{CredentialStore, MemoryStore};
    use secrecy::ExposeSecret;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[derive(Debug)]
    struct FailingStore;

    impl CredentialStore for FailingStore {
        fn store(&self, _key: &str, _value: &SecretString) -> devboy_core::Result<()> {
            Err(Error::Storage("store failed".to_string()))
        }

        fn get(&self, _key: &str) -> devboy_core::Result<Option<SecretString>> {
            Err(Error::Storage("secret backend unavailable".to_string()))
        }

        fn delete(&self, _key: &str) -> devboy_core::Result<()> {
            Err(Error::Storage("delete failed".to_string()))
        }
    }

    fn context_with_store(store: Arc<dyn CredentialStore>, config: Config) -> DiagnosticContext {
        DiagnosticContext {
            config: Some(config),
            config_path: Some(PathBuf::from("config.toml")),
            config_exists: true,
            config_source: "test",
            config_path_error: None,
            config_load_error: None,
            credential_store: store,
            verbose: false,
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

    #[test]
    fn resolve_active_provider_context_returns_active_context() {
        let active = resolve_active_provider_context(&config_with_active_context()).unwrap();

        assert_eq!(active.name, "workspace");
        assert!(active.config.github.is_some());
    }

    #[test]
    fn resolve_active_provider_context_returns_none_when_missing() {
        assert!(resolve_active_provider_context(&Config::default()).is_none());
    }

    #[test]
    fn resolve_secret_prefers_context_then_global_then_none() {
        let ctx = context_with_store(
            Arc::new(MemoryStore::with_credentials([
                (
                    "contexts.workspace.github.token".to_string(),
                    "context-secret".to_string(),
                ),
                ("github.token".to_string(), "global-secret".to_string()),
            ])),
            config_with_active_context(),
        );

        let context_secret = resolve_secret(&ctx, Some("workspace"), "github")
            .unwrap()
            .unwrap();
        assert_eq!(context_secret.source, "context");
        assert_eq!(context_secret.value.expose_secret(), "context-secret");

        let global_ctx = context_with_store(
            Arc::new(MemoryStore::with_credentials([(
                "github.token".to_string(),
                "global-secret".to_string(),
            )])),
            config_with_active_context(),
        );
        let global_secret = resolve_secret(&global_ctx, Some("workspace"), "github")
            .unwrap()
            .unwrap();
        assert_eq!(global_secret.source, "global");
        assert_eq!(global_secret.key, "github.token");

        let missing_ctx =
            context_with_store(Arc::new(MemoryStore::new()), config_with_active_context());
        assert!(
            resolve_secret(&missing_ctx, Some("workspace"), "github")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn resolve_secret_propagates_store_errors() {
        let ctx = context_with_store(Arc::new(FailingStore), config_with_active_context());

        let error = resolve_secret(&ctx, Some("workspace"), "github").unwrap_err();

        assert_eq!(error, "Storage error: secret backend unavailable");
    }

    #[test]
    fn resolve_secret_falls_back_to_global_when_scoped_errs() {
        // Regression for #188/#5: scoped lookup erroring (e.g. no
        // keychain available) must not abort the resolution — an env
        // var like `DEVBOY_GITHUB_TOKEN` that fills the global key
        // should still surface as a successful match.
        #[derive(Debug)]
        struct ScopedFailingStore {
            inner: Arc<MemoryStore>,
        }

        impl CredentialStore for ScopedFailingStore {
            fn store(&self, key: &str, value: &SecretString) -> devboy_core::Result<()> {
                self.inner.store(key, value)
            }

            fn get(&self, key: &str) -> devboy_core::Result<Option<SecretString>> {
                if key.starts_with("contexts.") {
                    Err(Error::Storage("scoped key backend unavailable".to_string()))
                } else {
                    self.inner.get(key)
                }
            }

            fn delete(&self, key: &str) -> devboy_core::Result<()> {
                self.inner.delete(key)
            }
        }

        let ctx = context_with_store(
            Arc::new(ScopedFailingStore {
                inner: Arc::new(MemoryStore::with_credentials([(
                    "github.token".to_string(),
                    "ghp_from_env_var_fallback".to_string(),
                )])),
            }),
            config_with_active_context(),
        );

        let resolved = resolve_secret(&ctx, Some("workspace"), "github")
            .unwrap()
            .unwrap();
        assert_eq!(resolved.source, "global");
        assert_eq!(resolved.key, "github.token");
        assert_eq!(resolved.value.expose_secret(), "ghp_from_env_var_fallback");
    }

    #[test]
    fn resolve_secret_surfaces_scoped_error_when_global_empty() {
        // Companion to the fallback test above: if the scoped lookup
        // errored AND the global probe returned `None`, the scoped
        // error is the strictly more informative answer — better than
        // reporting "token missing".
        #[derive(Debug)]
        struct ScopedOnlyFails;

        impl CredentialStore for ScopedOnlyFails {
            fn store(&self, _key: &str, _value: &SecretString) -> devboy_core::Result<()> {
                Err(Error::Storage("store failed".into()))
            }

            fn get(&self, key: &str) -> devboy_core::Result<Option<SecretString>> {
                if key.starts_with("contexts.") {
                    Err(Error::Storage("keychain unavailable".into()))
                } else {
                    Ok(None)
                }
            }

            fn delete(&self, _key: &str) -> devboy_core::Result<()> {
                Err(Error::Storage("delete failed".into()))
            }
        }

        let ctx = context_with_store(Arc::new(ScopedOnlyFails), config_with_active_context());
        let err = resolve_secret(&ctx, Some("workspace"), "github").unwrap_err();
        assert!(
            err.contains("keychain unavailable"),
            "expected to surface the scoped error, got: {err}"
        );
    }
}
