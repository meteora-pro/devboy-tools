//! Which secret-resolution mode this process is actually in
//! (ADR-024 §6).
//!
//! The framework has three postures and they behave very
//! differently:
//!
//! - **env-only / CI** — the environment is the sole source; no
//!   vault, no daemon, no keychain, no prompt.
//! - **default** — environment variables, then the local vault,
//!   which replaced the OS keychain as the durable store.
//! - **keychain opted in** — environment variables, then the local
//!   vault, then the OS keychain.
//!
//! Before this check there was no way to see which one applied
//! without reading the source. That matters because the failure
//! it prevents is quiet: a user who expects the keychain to be
//! consulted, and whose token therefore "disappears", gets no
//! signal at all — the lookup simply returns nothing.
//!
//! The check also surfaces `CiPolicy`, which until ADR-024 was
//! constructed and never read.

use async_trait::async_trait;
use devboy_storage::{CiPolicy, detect_ci_mode};
use serde_json::json;

use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};

/// The resolution posture in force for this process.
///
/// The shared `Env` prefix is not accidental naming: after
/// ADR-024 §6 every mode starts from environment variables, and
/// they differ only in what comes after. Renaming them to drop
/// the prefix would hide that.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretsMode {
    /// CI / env-only: the environment is the only source.
    EnvOnly,
    /// Interactive, keychain not enabled: environment, then the
    /// local vault.
    EnvDefault,
    /// Interactive with `[secrets.keychain] enabled = true`: the
    /// keychain is added after the vault, not in place of it.
    EnvThenKeychain,
}

impl SecretsMode {
    /// Short machine-readable form for `--json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EnvOnly => "env-only",
            Self::EnvDefault => "env-default",
            Self::EnvThenKeychain => "env-then-keychain",
        }
    }

    /// One-line description of where secrets come from.
    ///
    /// This is read as a factual account of the chain, so it has to
    /// track [`ChainStore`](devboy_storage::ChainStore) exactly —
    /// a description that omits a member is worse than none, since
    /// it makes a secret resolved from that member look impossible.
    pub fn chain_description(self) -> &'static str {
        match self {
            Self::EnvOnly => "environment variables",
            Self::EnvDefault => "environment variables, then the local vault",
            Self::EnvThenKeychain => {
                "environment variables, then the local vault, then the OS keychain"
            }
        }
    }
}

pub struct SecretsModeCheck;

#[async_trait]
impl DiagnosticCheck for SecretsModeCheck {
    fn id(&self) -> &'static str {
        "secrets-mode"
    }

    fn name(&self) -> &'static str {
        "Secret resolution mode"
    }

    fn category(&self) -> &'static str {
        "Secrets"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        let config = ctx.config.clone().unwrap_or_default();

        // `--ci` belongs to the parsed CLI, which this check does
        // not own; `DEVBOY_CI` and `[runtime] ci` are both visible
        // here, and they are what a misconfiguration usually turns
        // on by accident.
        let detection = detect_ci_mode(false, Some(config.is_ci_forced()));
        let keychain_enabled = config.is_keychain_enabled();

        let mode = if detection.active {
            SecretsMode::EnvOnly
        } else if keychain_enabled {
            SecretsMode::EnvThenKeychain
        } else {
            SecretsMode::EnvDefault
        };

        let profile = config.secrets_profile();
        let config_warnings = config.secrets_config_warnings();

        let mut status = CheckStatus::Pass;
        let mut notes: Vec<String> = Vec::new();
        let mut fix_command = None;

        if mode == SecretsMode::EnvOnly {
            let policy = CiPolicy::active();
            notes.push(
                "CI / env-only mode: no vault, daemon, keychain or prompt is used; writes fail \
                 rather than being absorbed."
                    .to_string(),
            );
            if policy.refuse_local_vault_unlock {
                notes.push("Vault unlock is refused in this mode by policy.".to_string());
            }
        }

        if mode == SecretsMode::EnvDefault {
            notes.push(
                "The OS keychain is not in the chain (ADR-024 §6 default). Enable it if you keep \
                 tokens there."
                    .to_string(),
            );
            fix_command = Some("devboy config set secrets.keychain.enabled true".to_string());
        }

        // Something looks like CI but nothing switched the mode.
        // Say so rather than letting the user guess which posture
        // is in force.
        if let Some(notice) = detection.doctor_notice() {
            notes.push(notice);
            status = CheckStatus::Warning;
        }

        // A config that contradicts itself warrants a warning even
        // when the mode itself is fine.
        if !config_warnings.is_empty() {
            status = CheckStatus::Warning;
            notes.extend(config_warnings.iter().cloned());
        }

        // `strict` promises per-call approval, which needs someone
        // to ask. Env-only mode has nobody.
        if profile.requires_prompt_surface() && mode == SecretsMode::EnvOnly {
            status = CheckStatus::Warning;
            notes.push(
                "The `strict` profile needs a surface on which the daemon can ask for approval, \
                 which env-only mode does not have. Use `convenient`, or run outside CI."
                    .to_string(),
            );
        }

        CheckResult {
            id: self.id().to_string(),
            category: self.category().to_string(),
            name: self.name().to_string(),
            status,
            message: format!("{} — {}", mode.as_str(), mode.chain_description()),
            details: Some(json!({
                "mode": mode.as_str(),
                "chain": mode.chain_description(),
                "keychain_enabled": keychain_enabled,
                "ci_active": detection.active,
                "ci_heuristic_without_explicit": detection.heuristic_without_explicit(),
                "profile": {
                    "unlock_ttl_seconds": config.unlock_ttl_seconds(),
                    "max_unlock_ttl_seconds": config.max_unlock_ttl_seconds(),
                    "idle_relock_seconds": config.idle_relock_seconds(),
                    // Named for what it is: an intent this build does
                    // not enforce. Reporting it as `forces_…` told
                    // users the strict profile was gating every
                    // access when nothing consulted it.
                    "intends_per_call_approval_not_enforced": profile.intends_per_call_approval(),
                },
                "notes": notes,
                "config_warnings": config_warnings,
            })),
            fix_command,
            fix_url: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_strings_are_stable() {
        // These land in `doctor --json`, so they are a contract.
        assert_eq!(SecretsMode::EnvOnly.as_str(), "env-only");
        assert_eq!(SecretsMode::EnvDefault.as_str(), "env-default");
        assert_eq!(SecretsMode::EnvThenKeychain.as_str(), "env-then-keychain");
    }

    #[test]
    fn chain_description_mentions_keychain_only_when_it_participates() {
        assert!(
            !SecretsMode::EnvDefault
                .chain_description()
                .contains("keychain")
        );
        assert!(
            !SecretsMode::EnvOnly
                .chain_description()
                .contains("keychain")
        );
        assert!(
            SecretsMode::EnvThenKeychain
                .chain_description()
                .contains("keychain")
        );
    }
}
