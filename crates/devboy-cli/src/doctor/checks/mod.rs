pub mod config;
pub mod credentials;
pub mod environment;
pub mod providers;

use crate::doctor::DiagnosticContext;
use devboy_core::{Config, ContextConfig};

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
    pub value: String,
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
    if let Some(context_name) = context_name {
        let scoped_key = format!("contexts.{context_name}.{provider}.token");
        match ctx.credential_store.get(&scoped_key) {
            Ok(Some(value)) => {
                return Ok(Some(ResolvedSecret {
                    key: scoped_key,
                    source: "context",
                    value,
                }))
            }
            Ok(None) => {}
            Err(error) => return Err(error.to_string()),
        }
    }

    let global_key = format!("{provider}.token");
    match ctx.credential_store.get(&global_key) {
        Ok(Some(value)) => Ok(Some(ResolvedSecret {
            key: global_key,
            source: "global",
            value,
        })),
        Ok(None) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}
