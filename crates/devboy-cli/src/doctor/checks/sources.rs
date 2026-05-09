//! Doctor "Sources" section per [ADR-021] §9.
//!
//! Enumerates every `[[source]]` entry in the router config
//! (`<config_dir>/devboy-tools/secrets/sources.toml`), constructs
//! the matching `SecretSource` impl for built-in types, probes
//! each via `is_available()`, and reports a single
//! [`CheckResult`] summarising the per-source health card.
//!
//! The card surfaces:
//!
//! - `name` and `type` from the config,
//! - declared capabilities (with [`BIOMETRIC_PROMPT`] /
//!   [`AUDIT_LOGGED`] flagged so the user knows the cost of each
//!   read),
//! - availability — `Available` / `Locked` / `NotInstalled` /
//!   `Error`,
//! - an actionable hint when the status is recoverable
//!   (`op signin`, unlock the keychain, sign in to Vault, …).
//!
//! Last-successful-contact tracking lives one layer up — the
//! router (P11+) keeps that state. This check reports the
//! *current* probe outcome only; that's already enough to drive
//! the bulk of the §9 promise.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md
//! [`BIOMETRIC_PROMPT`]: devboy_storage::Capabilities::BIOMETRIC_PROMPT
//! [`AUDIT_LOGGED`]: devboy_storage::Capabilities::AUDIT_LOGGED

use async_trait::async_trait;
use devboy_secret_1password::OnePasswordSource;
use devboy_secret_env_store::EnvStoreSource;
use devboy_secret_keychain::KeychainSource;
use devboy_secret_local_vault::LocalVaultSource;
use devboy_storage::{Capabilities, RouterConfig, SecretSource, SourceDefinition, SourceStatus};
use serde_json::{Value, json};

use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};

/// Doctor check that renders the per-source health card.
///
/// Single-row check (one [`CheckResult`]) — the per-source
/// breakdown is in the `details` JSON. That keeps the
/// console-formatter simple and the JSON output easy to grep.
pub struct SourcesCheck;

#[async_trait]
impl DiagnosticCheck for SourcesCheck {
    fn id(&self) -> &'static str {
        "sources"
    }

    fn name(&self) -> &'static str {
        "Sources"
    }

    fn category(&self) -> &'static str {
        "Secrets"
    }

    async fn run(&self, _ctx: &DiagnosticContext) -> CheckResult {
        let config = match RouterConfig::load() {
            Ok(c) => c,
            Err(e) => {
                return CheckResult {
                    id: self.id().to_string(),
                    category: self.category().to_string(),
                    name: self.name().to_string(),
                    status: CheckStatus::Error,
                    message: format!("Failed to load router config: {e}"),
                    details: None,
                    fix_command: None,
                    fix_url: None,
                };
            }
        };

        if config.sources.is_empty() {
            return CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Skipped,
                message: "No `[[source]]` entries declared in sources.toml".to_string(),
                details: None,
                fix_command: None,
                fix_url: Some(
                    "https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md"
                        .to_string(),
                ),
            };
        }

        let mut cards = Vec::with_capacity(config.sources.len());
        let mut overall = CheckStatus::Pass;
        let mut summary_pieces = Vec::with_capacity(config.sources.len());

        for def in &config.sources {
            let card = probe_source(def).await;
            // Track worst status across the whole list.
            overall = worst(overall, card.check_status);
            summary_pieces.push(format!("{}={}", def.name, card.status_label));
            cards.push(card_to_json(&card));
        }

        CheckResult {
            id: self.id().to_string(),
            category: self.category().to_string(),
            name: self.name().to_string(),
            status: overall,
            message: format!(
                "{} source(s) configured: {}",
                config.sources.len(),
                summary_pieces.join(", ")
            ),
            details: Some(json!({ "sources": cards })),
            fix_command: None,
            fix_url: None,
        }
    }
}

// =============================================================================
// Per-source probe
// =============================================================================

struct SourceCard {
    name: String,
    source_type: String,
    capabilities: Capabilities,
    status_label: &'static str,
    status_message: Option<String>,
    hint: Option<String>,
    check_status: CheckStatus,
}

async fn probe_source(def: &SourceDefinition) -> SourceCard {
    match def.source_type.as_str() {
        "keychain" => probe_keychain(def).await,
        "local-vault" => probe_local_vault(def).await,
        "1password" => probe_1password(def).await,
        // The vault source needs auth credentials we don't yet
        // wire end to end (router orchestration is P11+). Report
        // it as configured-but-not-probed so the user sees it in
        // the inventory; full liveness comes later.
        "vault" => unprobed_card(
            def,
            Capabilities::READ
                | Capabilities::LIST
                | Capabilities::VALIDATE
                | Capabilities::WRITE
                | Capabilities::ROTATE
                | Capabilities::AUDIT_LOGGED,
            "credentials wiring lands with the router orchestration",
        ),
        "env-store" => probe_env_store(def).await,
        unknown => unknown_type_card(def, unknown),
    }
}

async fn probe_keychain(def: &SourceDefinition) -> SourceCard {
    let src = KeychainSource::new(def.name.clone());
    let status = src.is_available().await;
    SourceCard {
        name: def.name.clone(),
        source_type: def.source_type.clone(),
        capabilities: src.capabilities(),
        status_label: status_label(&status),
        status_message: status_message(&status),
        hint: keychain_hint(&status),
        check_status: status_to_check_status(&status, /*required=*/ false),
    }
}

async fn probe_local_vault(def: &SourceDefinition) -> SourceCard {
    let src = match LocalVaultSource::new(def.name.clone()) {
        Ok(s) => s,
        Err(e) => {
            return SourceCard {
                name: def.name.clone(),
                source_type: def.source_type.clone(),
                capabilities: Capabilities::empty(),
                status_label: "Error",
                status_message: Some(format!("could not resolve agent socket: {e}")),
                hint: Some("set DEVBOY_AGENT_SOCKET or fix `dirs::config_dir()`".to_string()),
                check_status: CheckStatus::Error,
            };
        }
    };
    let status = src.is_available().await;
    SourceCard {
        name: def.name.clone(),
        source_type: def.source_type.clone(),
        capabilities: src.capabilities(),
        status_label: status_label(&status),
        status_message: status_message(&status),
        hint: local_vault_hint(&status),
        check_status: status_to_check_status(&status, /*required=*/ false),
    }
}

async fn probe_1password(def: &SourceDefinition) -> SourceCard {
    let src = match OnePasswordSource::new(def.name.clone()) {
        Ok(s) => s,
        Err(_) => {
            // Binary missing — straight to NotInstalled.
            return SourceCard {
                name: def.name.clone(),
                source_type: def.source_type.clone(),
                // Even with the binary missing we know the cap
                // declaration from ADR-021 §8.
                capabilities: Capabilities::READ
                    | Capabilities::LIST
                    | Capabilities::VALIDATE
                    | Capabilities::BIOMETRIC_PROMPT
                    | Capabilities::AUDIT_LOGGED,
                status_label: "NotInstalled",
                status_message: Some("`op` CLI not found in PATH".into()),
                hint: Some(
                    "install the 1Password CLI from https://developer.1password.com/docs/cli/get-started/"
                        .into(),
                ),
                check_status: CheckStatus::Warning,
            };
        }
    };
    let mut src = src;
    if let Some(account) = def.settings.get("account").and_then(toml::Value::as_str) {
        src = src.with_account(account);
    }
    let status = src.is_available().await;
    SourceCard {
        name: def.name.clone(),
        source_type: def.source_type.clone(),
        capabilities: src.capabilities(),
        status_label: status_label(&status),
        status_message: status_message(&status),
        hint: one_password_hint(&status),
        check_status: status_to_check_status(&status, /*required=*/ false),
    }
}

async fn probe_env_store(def: &SourceDefinition) -> SourceCard {
    let src = EnvStoreSource::new(def.name.clone());
    let status = src.is_available().await;
    SourceCard {
        name: def.name.clone(),
        source_type: def.source_type.clone(),
        capabilities: src.capabilities(),
        status_label: status_label(&status),
        status_message: status_message(&status),
        hint: None,
        check_status: status_to_check_status(&status, /*required=*/ false),
    }
}

fn unprobed_card(def: &SourceDefinition, capabilities: Capabilities, reason: &str) -> SourceCard {
    SourceCard {
        name: def.name.clone(),
        source_type: def.source_type.clone(),
        capabilities,
        status_label: "Skipped",
        status_message: Some(format!("not probed yet — {reason}")),
        hint: None,
        check_status: CheckStatus::Skipped,
    }
}

fn unknown_type_card(def: &SourceDefinition, unknown: &str) -> SourceCard {
    SourceCard {
        name: def.name.clone(),
        source_type: def.source_type.clone(),
        capabilities: Capabilities::empty(),
        status_label: "Error",
        status_message: Some(format!("unknown source type '{unknown}'")),
        hint: Some(
            "fix [[source]].type in sources.toml or install the corresponding plugin".to_string(),
        ),
        check_status: CheckStatus::Error,
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn status_label(s: &SourceStatus) -> &'static str {
    match s {
        SourceStatus::Available => "Available",
        SourceStatus::Locked => "Locked",
        SourceStatus::NotInstalled => "NotInstalled",
        SourceStatus::Error(_) => "Error",
    }
}

fn status_message(s: &SourceStatus) -> Option<String> {
    match s {
        SourceStatus::Available | SourceStatus::Locked | SourceStatus::NotInstalled => None,
        SourceStatus::Error(msg) => Some(msg.clone()),
    }
}

fn status_to_check_status(s: &SourceStatus, required: bool) -> CheckStatus {
    match s {
        SourceStatus::Available => CheckStatus::Pass,
        SourceStatus::Locked => CheckStatus::Warning,
        SourceStatus::NotInstalled => {
            if required {
                CheckStatus::Error
            } else {
                CheckStatus::Warning
            }
        }
        SourceStatus::Error(_) => CheckStatus::Error,
    }
}

/// Worst (most severe) of two statuses. Pass < Warning < Error;
/// Skipped does not raise the overall, but doesn't lower it
/// either.
fn worst(a: CheckStatus, b: CheckStatus) -> CheckStatus {
    fn rank(s: CheckStatus) -> u8 {
        match s {
            CheckStatus::Skipped => 0,
            CheckStatus::Pass => 1,
            CheckStatus::Warning => 2,
            CheckStatus::Error => 3,
        }
    }
    if rank(b) > rank(a) { b } else { a }
}

fn card_to_json(card: &SourceCard) -> Value {
    json!({
        "name": card.name,
        "type": card.source_type,
        "capabilities": capabilities_to_json(card.capabilities),
        "status": card.status_label,
        "status_message": card.status_message,
        "hint": card.hint,
    })
}

fn capabilities_to_json(caps: Capabilities) -> Value {
    let pairs = [
        ("READ", Capabilities::READ),
        ("LIST", Capabilities::LIST),
        ("VALIDATE", Capabilities::VALIDATE),
        ("WRITE", Capabilities::WRITE),
        ("ROTATE", Capabilities::ROTATE),
        ("BIOMETRIC_PROMPT", Capabilities::BIOMETRIC_PROMPT),
        ("AUDIT_LOGGED", Capabilities::AUDIT_LOGGED),
    ];
    let names: Vec<&str> = pairs
        .iter()
        .filter(|(_, bit)| caps.contains(*bit))
        .map(|(name, _)| *name)
        .collect();
    let flagged: Vec<&str> = ["BIOMETRIC_PROMPT", "AUDIT_LOGGED"]
        .into_iter()
        .filter(|n| names.contains(n))
        .collect();
    json!({
        "all": names,
        "ux_flags": flagged,
    })
}

// =============================================================================
// Per-type hint copy
// =============================================================================

fn keychain_hint(status: &SourceStatus) -> Option<String> {
    match status {
        SourceStatus::Locked => Some(
            "unlock the OS keychain (macOS Keychain Access app, GNOME Keyring login)".to_string(),
        ),
        SourceStatus::NotInstalled => Some(
            "no Secret Service backend on this host; route default to local-vault per ADR-023"
                .to_string(),
        ),
        SourceStatus::Error(_) | SourceStatus::Available => None,
    }
}

fn local_vault_hint(status: &SourceStatus) -> Option<String> {
    match status {
        SourceStatus::Locked => Some(
            "unlock the daemon — run `devboy secrets agent start` and the next read will prompt for the passphrase"
                .to_string(),
        ),
        SourceStatus::NotInstalled => Some(
            "start the agent with `devboy secrets agent start` (or install it via `devboy secrets agent install`)"
                .to_string(),
        ),
        SourceStatus::Error(_) | SourceStatus::Available => None,
    }
}

fn one_password_hint(status: &SourceStatus) -> Option<String> {
    match status {
        SourceStatus::Locked => Some("run `op signin` to unlock the 1Password session".to_string()),
        SourceStatus::NotInstalled => Some(
            "install the 1Password CLI from https://developer.1password.com/docs/cli/get-started/"
                .to_string(),
        ),
        SourceStatus::Error(_) | SourceStatus::Available => None,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_storage::SourceStatus;

    #[test]
    fn worst_picks_more_severe_status() {
        assert_eq!(
            worst(CheckStatus::Pass, CheckStatus::Warning),
            CheckStatus::Warning
        );
        assert_eq!(
            worst(CheckStatus::Warning, CheckStatus::Error),
            CheckStatus::Error
        );
        assert_eq!(
            worst(CheckStatus::Error, CheckStatus::Pass),
            CheckStatus::Error
        );
        assert_eq!(
            worst(CheckStatus::Skipped, CheckStatus::Pass),
            CheckStatus::Pass
        );
    }

    #[test]
    fn status_label_covers_all_variants() {
        assert_eq!(status_label(&SourceStatus::Available), "Available");
        assert_eq!(status_label(&SourceStatus::Locked), "Locked");
        assert_eq!(status_label(&SourceStatus::NotInstalled), "NotInstalled");
        assert_eq!(status_label(&SourceStatus::Error("x".into())), "Error");
    }

    #[test]
    fn status_to_check_status_required_promotes_not_installed_to_error() {
        assert_eq!(
            status_to_check_status(&SourceStatus::NotInstalled, false),
            CheckStatus::Warning
        );
        assert_eq!(
            status_to_check_status(&SourceStatus::NotInstalled, true),
            CheckStatus::Error
        );
        assert_eq!(
            status_to_check_status(&SourceStatus::Available, false),
            CheckStatus::Pass
        );
        assert_eq!(
            status_to_check_status(&SourceStatus::Locked, false),
            CheckStatus::Warning
        );
        assert_eq!(
            status_to_check_status(&SourceStatus::Error("oops".into()), false),
            CheckStatus::Error
        );
    }

    #[test]
    fn capabilities_json_lists_all_set_bits_and_flags_ux_bits() {
        let caps = Capabilities::READ
            | Capabilities::LIST
            | Capabilities::BIOMETRIC_PROMPT
            | Capabilities::AUDIT_LOGGED;
        let v = capabilities_to_json(caps);
        let all = v["all"].as_array().unwrap();
        let names: Vec<&str> = all.iter().map(|n| n.as_str().unwrap()).collect();
        assert!(names.contains(&"READ"));
        assert!(names.contains(&"LIST"));
        assert!(names.contains(&"BIOMETRIC_PROMPT"));
        assert!(names.contains(&"AUDIT_LOGGED"));
        let flags = v["ux_flags"].as_array().unwrap();
        let flag_names: Vec<&str> = flags.iter().map(|n| n.as_str().unwrap()).collect();
        assert_eq!(flag_names, vec!["BIOMETRIC_PROMPT", "AUDIT_LOGGED"]);
    }

    #[test]
    fn capabilities_json_hides_unset_ux_flags() {
        let caps = Capabilities::READ | Capabilities::WRITE;
        let v = capabilities_to_json(caps);
        let flags = v["ux_flags"].as_array().unwrap();
        assert!(
            flags.is_empty(),
            "no UX flags should be reported when neither bit is set"
        );
    }

    #[test]
    fn keychain_hint_only_fires_on_recoverable_states() {
        assert!(keychain_hint(&SourceStatus::Available).is_none());
        assert!(keychain_hint(&SourceStatus::Locked).is_some());
        assert!(keychain_hint(&SourceStatus::NotInstalled).is_some());
        assert!(keychain_hint(&SourceStatus::Error("x".into())).is_none());
    }

    #[test]
    fn local_vault_hint_mentions_agent_subcommand() {
        let h = local_vault_hint(&SourceStatus::NotInstalled).unwrap();
        assert!(h.contains("devboy secrets agent"));
    }

    #[test]
    fn one_password_hint_mentions_op_signin_when_locked() {
        let h = one_password_hint(&SourceStatus::Locked).unwrap();
        assert!(h.contains("op signin"));
    }
}
