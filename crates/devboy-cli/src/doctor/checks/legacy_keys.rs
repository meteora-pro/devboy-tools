//! Doctor "Legacy keychain entries" check per [ADR-020] §7
//! (migration from the pre-manifest credential store).
//!
//! Pre-ADR-020 devboy-tools wrote credentials under flat
//! `<provider>/token` keys (and `contexts.<name>.<provider>.token`
//! for context-scoped overrides). The new path convention requires
//! `<scope>/<provider>/<purpose>` (≥3 segments, ADR-020 §2). This
//! check probes the keychain for the legacy shapes, reports any
//! that are present, and suggests:
//!
//! 1. A canonical ADR-020 path the entry should move to.
//! 2. A one-liner `devboy secrets migrate <legacy>` to do it.
//!
//! ## Why no enumeration?
//!
//! The `keyring` crate doesn't expose
//! `SecKeychainSearchCreateFromAttributes` / `SearchItems` /
//! `CredEnumerate`. Probing a known-legacy list keeps the check
//! useful today; a follow-up that wires the platform search APIs
//! directly can drop in here without changing the doctor surface.
//!
//! ## What the check does **not** do
//!
//! - Migrate. That's `devboy secrets migrate <legacy>` (P10.2).
//! - Probe `contexts.<name>.<provider>.token`. The context names
//!   are unbounded; we'd need a separate config-walk pass to know
//!   which ones to probe. Migration UX is left to P10.2.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};

/// Providers that the pre-ADR-020 devboy-tools knew about.
/// Updated when a new provider lands so the doctor probe covers
/// it. The list is intentionally narrow — false positives (probing
/// a provider that isn't installed) just return `None` from the
/// keychain and cost one cheap lookup per entry.
const KNOWN_PROVIDERS: &[&str] = &[
    "github",
    "gitlab",
    "clickup",
    "jira",
    "confluence",
    "slack",
    "fireflies",
    "devboy-cloud",
];

/// Suffixes the legacy store appended to the provider segment.
const LEGACY_SUFFIXES: &[&str] = &["token", "email"];

pub struct LegacyKeysCheck;

#[async_trait]
impl DiagnosticCheck for LegacyKeysCheck {
    fn id(&self) -> &'static str {
        "legacy-keys"
    }

    fn name(&self) -> &'static str {
        "Legacy keychain entries"
    }

    fn category(&self) -> &'static str {
        "Secrets"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        let store = ctx.credential_store.clone();
        let mut findings = Vec::new();
        for key in known_legacy_keys() {
            match store.get(&key) {
                Ok(Some(_)) => findings.push(classify(&key)),
                Ok(None) => {}
                // A keychain that errors out on `get` is the
                // CredentialStoreCheck's problem; this check
                // should not double-fail on the same condition.
                // Stop probing the rest to avoid spamming the
                // Output.
                Err(_) => break,
            }
        }

        let migration_complete = ctx
            .config
            .as_ref()
            .map(|c| c.is_secrets_migration_complete())
            .unwrap_or(false);

        if findings.is_empty() {
            // Two clean states: "we never had legacy entries"
            // (default) and "the user has confirmed migration is
            // done" (explicit). Both are `Pass`; the message
            // tells them apart.
            let message = if migration_complete {
                "Migration confirmed complete; no pre-ADR-020 entries remain".to_string()
            } else {
                "No pre-ADR-020 keychain entries detected".to_string()
            };
            return CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Pass,
                message,
                details: None,
                fix_command: None,
                fix_url: None,
            };
        }

        // Findings + migration_complete=true is a strict failure
        // — the user told us they were done, but legacy entries
        // remain. Treat as Error so doctor's exit code flips and
        // a CI gate catches it. Findings + migration_complete=false
        // is the migration-not-finished-yet path: Warning, with a
        // helpful fix_command.
        let (status, summary) = if migration_complete {
            (
                CheckStatus::Error,
                format!(
                    "[secrets] migration_complete=true but {} legacy entr{} remain — finish migrating or clear the flag",
                    findings.len(),
                    if findings.len() == 1 { "y" } else { "ies" }
                ),
            )
        } else {
            (
                CheckStatus::Warning,
                format!(
                    "{n} secret{s} resolve only through the read-only legacy keychain fallback, \
                     which is removed in {removed} — migrate {them} to the ADR-020 path \
                     convention",
                    n = findings.len(),
                    s = if findings.len() == 1 { "" } else { "s" },
                    them = if findings.len() == 1 { "it" } else { "them" },
                    removed = devboy_storage::legacy_keychain::FALLBACK_REMOVED_IN,
                ),
            )
        };
        // Surface the FIRST migration command in `fix_command` so
        // the renderer's "run this:" footer points users at a
        // concrete one-liner. The full list is in the JSON
        // details.
        let fix_command = findings.first().map(|f| f.migrate_command.clone());
        CheckResult {
            id: self.id().to_string(),
            category: self.category().to_string(),
            name: self.name().to_string(),
            status,
            message: summary,
            details: Some(findings_to_json(&findings)),
            fix_command,
            fix_url: None,
        }
    }
}

// =============================================================================
// Pure helpers
// =============================================================================

/// One legacy entry the doctor probe found in the keychain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyFinding {
    /// The legacy key as it lives in the keychain.
    pub legacy_key: String,
    /// Suggested ADR-020 canonical path. `None` when no
    /// heuristic produces one — the user has to decide.
    pub suggested_path: Option<String>,
    /// Ready-to-run `devboy secrets migrate` invocation.
    pub migrate_command: String,
}

/// Yield every key the probe inspects. Public for testability —
/// the test suite uses it to seed a `MemoryStore` with the same
/// shape the production probe walks.
pub fn known_legacy_keys() -> Vec<String> {
    let mut out = Vec::with_capacity(KNOWN_PROVIDERS.len() * LEGACY_SUFFIXES.len());
    for p in KNOWN_PROVIDERS {
        for s in LEGACY_SUFFIXES {
            out.push(format!("{p}/{s}"));
        }
    }
    out
}

fn classify(legacy_key: &str) -> LegacyFinding {
    LegacyFinding {
        legacy_key: legacy_key.to_owned(),
        suggested_path: suggest_canonical_path(legacy_key),
        migrate_command: format!("devboy secrets migrate {legacy_key}"),
    }
}

/// Best-effort mapping from a legacy key to a canonical
/// ADR-020 path. The user is the source of truth — the
/// migration flow (P10.2) will confirm the suggestion before
/// it writes anything.
///
/// Heuristics:
///
/// - `<provider>/token` → `personal/<provider>/token-legacy`
/// - `<provider>/email` → `personal/<provider>/email-legacy`
/// - `contexts.<context>.<provider>.<field>` →
///   `<context>/<provider>/<field>-legacy`
/// - anything else → `None`
pub fn suggest_canonical_path(legacy_key: &str) -> Option<String> {
    if let Some(rest) = legacy_key.strip_prefix("contexts.") {
        let parts: Vec<&str> = rest.split('.').collect();
        if parts.len() == 3 {
            let (ctx, provider, field) = (parts[0], parts[1], parts[2]);
            return Some(format!("{ctx}/{provider}/{field}-legacy"));
        }
    }
    let parts: Vec<&str> = legacy_key.split('/').collect();
    if parts.len() == 2 {
        let (provider, field) = (parts[0], parts[1]);
        return Some(format!("personal/{provider}/{field}-legacy"));
    }
    None
}

fn findings_to_json(findings: &[LegacyFinding]) -> Value {
    let entries: Vec<Value> = findings
        .iter()
        .map(|f| {
            json!({
                "legacy_key": f.legacy_key,
                "suggested_path": f.suggested_path,
                "migrate_command": f.migrate_command,
            })
        })
        .collect();
    json!({ "findings": entries })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::DiagnosticContext;
    use devboy_core::Config;
    use devboy_core::config::SecretsConfig;
    use devboy_storage::{CredentialStore, MemoryStore};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn ctx_with_store(store: Arc<dyn CredentialStore>) -> DiagnosticContext {
        ctx_with_config_and_store(Config::default(), store)
    }

    fn ctx_with_config_and_store(
        config: Config,
        store: Arc<dyn CredentialStore>,
    ) -> DiagnosticContext {
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

    fn config_with_migration_complete(complete: bool) -> Config {
        Config {
            secrets: Some(SecretsConfig {
                migration_complete: complete,
                ..SecretsConfig::default()
            }),
            ..Config::default()
        }
    }

    // -- known_legacy_keys -----------------------------------------

    #[test]
    fn known_legacy_keys_covers_provider_x_suffix_matrix() {
        let keys = known_legacy_keys();
        assert!(keys.contains(&"github/token".to_owned()));
        assert!(keys.contains(&"jira/email".to_owned()));
        assert_eq!(keys.len(), KNOWN_PROVIDERS.len() * LEGACY_SUFFIXES.len());
    }

    // -- suggest_canonical_path ------------------------------------

    #[test]
    fn legacy_provider_token_maps_to_personal_provider_token_legacy() {
        assert_eq!(
            suggest_canonical_path("github/token").as_deref(),
            Some("personal/github/token-legacy")
        );
    }

    #[test]
    fn legacy_provider_email_maps_to_personal_provider_email_legacy() {
        assert_eq!(
            suggest_canonical_path("jira/email").as_deref(),
            Some("personal/jira/email-legacy")
        );
    }

    #[test]
    fn legacy_contexts_form_maps_to_context_provider_field_legacy() {
        assert_eq!(
            suggest_canonical_path("contexts.workspace.github.token").as_deref(),
            Some("workspace/github/token-legacy")
        );
    }

    #[test]
    fn unmappable_legacy_key_returns_none() {
        // No leading `contexts.` and not exactly 2 segments.
        assert_eq!(suggest_canonical_path("just-one-segment"), None);
        assert_eq!(suggest_canonical_path("a/b/c/d"), None);
    }

    // -- classify --------------------------------------------------

    #[test]
    fn classify_emits_migrate_command_with_legacy_key() {
        let f = classify("github/token");
        assert_eq!(f.legacy_key, "github/token");
        assert_eq!(f.migrate_command, "devboy secrets migrate github/token");
        assert_eq!(
            f.suggested_path.as_deref(),
            Some("personal/github/token-legacy")
        );
    }

    // -- Doctor check via MemoryStore ------------------------------

    #[tokio::test]
    async fn empty_keychain_passes_with_no_findings() {
        let ctx = ctx_with_store(Arc::new(MemoryStore::new()));
        let r = LegacyKeysCheck.run(&ctx).await;
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.details.is_none());
    }

    #[tokio::test]
    async fn detects_present_legacy_entries_and_emits_warning() {
        let store = MemoryStore::with_credentials([
            ("github/token".into(), "ghp_fixture".into()),
            ("gitlab/token".into(), "glpat-fixture".into()),
        ]);
        let ctx = ctx_with_store(Arc::new(store));
        let r = LegacyKeysCheck.run(&ctx).await;
        assert_eq!(r.status, CheckStatus::Warning);
        assert!(r.message.contains("2 secrets"), "{}", r.message);
        // The warning has to say what the user is standing on and
        // when it goes away. Without the version this reads as a
        // tidiness nag and gets deferred indefinitely.
        assert!(
            r.message.contains("legacy keychain fallback"),
            "{}",
            r.message
        );
        assert!(
            r.message
                .contains(devboy_storage::legacy_keychain::FALLBACK_REMOVED_IN),
            "{}",
            r.message
        );
        let details = r.details.unwrap();
        let entries = details["findings"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        // First entry surfaces in fix_command.
        assert!(r.fix_command.unwrap().starts_with("devboy secrets migrate"));
    }

    #[tokio::test]
    async fn pass_when_only_non_legacy_entries_present() {
        // Conformant ADR-020 paths in the keychain (3 segments)
        // are not legacy and must not trip the check. We seed
        // them under arbitrary keys; the check probes only the
        // known-legacy shapes.
        let store = MemoryStore::with_credentials([(
            "personal/github/token-deploy".into(),
            "ghp_new".into(),
        )]);
        let ctx = ctx_with_store(Arc::new(store));
        let r = LegacyKeysCheck.run(&ctx).await;
        assert_eq!(r.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn migration_complete_with_no_findings_passes_with_confirmation_message() {
        let ctx = ctx_with_config_and_store(
            config_with_migration_complete(true),
            Arc::new(MemoryStore::new()),
        );
        let r = LegacyKeysCheck.run(&ctx).await;
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(
            r.message.contains("Migration confirmed complete"),
            "expected explicit confirmation, got: {}",
            r.message
        );
    }

    #[tokio::test]
    async fn migration_complete_with_findings_escalates_to_error() {
        // The user told us they were done, but legacy entries
        // remain — this is a strong inconsistency the doctor
        // surfaces as Error so a CI gate catches it.
        let store = MemoryStore::with_credentials([("github/token".into(), "ghp".into())]);
        let ctx = ctx_with_config_and_store(config_with_migration_complete(true), Arc::new(store));
        let r = LegacyKeysCheck.run(&ctx).await;
        assert_eq!(r.status, CheckStatus::Error);
        assert!(
            r.message.contains("migration_complete=true"),
            "error message must mention the offending flag, got: {}",
            r.message
        );
        assert!(r.message.contains("1 legacy entry remain"));
    }

    #[tokio::test]
    async fn detects_legacy_email_entries_too() {
        let store =
            MemoryStore::with_credentials([("jira/email".into(), "user@example.com".into())]);
        let ctx = ctx_with_store(Arc::new(store));
        let r = LegacyKeysCheck.run(&ctx).await;
        assert_eq!(r.status, CheckStatus::Warning);
        let details = r.details.unwrap();
        let entries = details["findings"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["legacy_key"], "jira/email");
        assert_eq!(entries[0]["suggested_path"], "personal/jira/email-legacy");
    }
}
