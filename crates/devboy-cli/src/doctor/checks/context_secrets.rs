//! Doctor "Secrets in active context" section per [ADR-021] §9.
//!
//! Walks every path declared in the active manifest (project's
//! `.devboy/secrets.toml` merged with the global index) and reports
//! per path:
//!
//! - the routed source (`Explicit` / `Prefix` / `Default` /
//!   no-route),
//! - status — `ok` / `expiring` / `expired` / `no-route`,
//! - `expires_at` from the global index (when set), with a
//!   computed days-to-expiry hint.
//!
//! Optional paths are folded into the same row breakdown but
//! contribute only `Skipped` / `Warning` to the overall status —
//! never `Error`. Required paths *do* fail the check, so a
//! missing-route / expired required secret turns
//! `devboy doctor` non-zero. That makes the command a usable CI
//! sanity gate without further wrapping (per ADR-021 §9 last
//! paragraph).
//!
//! ## What this section does **not** do
//!
//! - **Probe the upstream for actual presence.** The router
//!   orchestration that performs `source.get(path)` lives in P11+;
//!   this check only confirms the path *would* route somewhere.
//!   "Provisioned" vs "missing" requires that probe and lands in
//!   the next phase.
//! - **Format validation.** That's P9.1 (`devboy secrets validate`).
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use devboy_storage::{
    GlobalIndex, PathResolver, ProjectManifest, ResolvedSecret, RouteDecision, RouterConfig,
    merge_manifest,
};
use serde_json::{Value, json};

use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};

/// Paths whose `expires_at` is within this many days surface as
/// `expiring` (`Warning`). Past the date → `expired` (`Error`).
const EXPIRY_WARNING_DAYS: i64 = 14;

/// Doctor check that renders the per-path health card for the
/// active manifest.
pub struct ContextSecretsCheck;

#[async_trait]
impl DiagnosticCheck for ContextSecretsCheck {
    fn id(&self) -> &'static str {
        "context-secrets"
    }

    fn name(&self) -> &'static str {
        "Secrets in active context"
    }

    fn category(&self) -> &'static str {
        "Secrets"
    }

    async fn run(&self, _ctx: &DiagnosticContext) -> CheckResult {
        // 1) Manifest + global index. Either may be missing — the
        //    loaders return empty values rather than erroring.
        let global = match GlobalIndex::load() {
            Ok(g) => g,
            Err(e) => {
                return error_result(self, format!("Failed to load global index: {e}"));
            }
        };
        let manifest = match ProjectManifest::load() {
            Ok(m) => m,
            Err(e) => {
                return error_result(self, format!("Failed to load project manifest: {e}"));
            }
        };
        let merged = match merge_manifest(&global, &manifest) {
            Ok(m) => m,
            Err(e) => {
                return error_result(self, format!("Failed to merge manifest: {e}"));
            }
        };

        // 2) Router config drives the path-to-source resolution.
        //    Empty config is fine — every path will fall through to
        //    `no-route` and produce a useful error message.
        let router = match RouterConfig::load() {
            Ok(c) => c,
            Err(e) => {
                return error_result(self, format!("Failed to load router config: {e}"));
            }
        };

        if merged.secrets.is_empty() {
            return CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Skipped,
                message: "No secrets declared in the active manifest".to_string(),
                details: None,
                fix_command: None,
                fix_url: None,
            };
        }

        let resolver = PathResolver::new(&router);
        let today = Utc::now().date_naive();

        let mut cards: Vec<SecretCard> = Vec::with_capacity(merged.secrets.len());
        for resolved in merged.secrets.values() {
            cards.push(build_card(resolved, &resolver, today));
        }

        let overall = aggregate_status(&cards);
        let summary = summary_message(&cards);

        CheckResult {
            id: self.id().to_string(),
            category: self.category().to_string(),
            name: self.name().to_string(),
            status: overall,
            message: summary,
            details: Some(cards_to_json(&cards)),
            fix_command: None,
            fix_url: None,
        }
    }
}

// =============================================================================
// Per-path card
// =============================================================================

/// Per-secret breakdown surfaced via the `details` JSON.
struct SecretCard {
    path: String,
    required: bool,
    /// Routed source name, when the resolver returned a decision.
    routed_source: Option<String>,
    /// Variant of [`RouteDecision`] as a flat string for JSON.
    decision_kind: &'static str,
    /// `expires_at` from the global index, when set.
    expires_at: Option<String>,
    /// Days until expiry (negative = already expired). `None` when
    /// `expires_at` is missing or unparseable.
    expires_in_days: Option<i64>,
    /// One-word status label surfaced in summaries / JSON.
    status_label: &'static str,
    /// Severity of this path's status. Aggregated to overall.
    check_status: CheckStatus,
    /// Optional actionable hint (e.g. "rotate this secret").
    hint: Option<String>,
}

fn build_card(
    resolved: &ResolvedSecret,
    resolver: &PathResolver<'_>,
    today: NaiveDate,
) -> SecretCard {
    let (routed_source, decision_kind) = match resolver.resolve(&resolved.path) {
        Ok(decision) => {
            let source = decision.source().to_string();
            let kind = match decision {
                RouteDecision::Explicit { .. } => "explicit",
                RouteDecision::Prefix { .. } => "prefix",
                RouteDecision::Default { .. } => "default",
            };
            (Some(source), kind)
        }
        Err(_) => (None, "no-route"),
    };

    // Expiry analysis.
    let expires_at = resolved.metadata.expires_at.clone();
    let expires_in_days = expires_at
        .as_deref()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .map(|d| (d - today).num_days());
    let expiry_status = expires_in_days.map(expiry_status_for_days);

    // Combined status: no-route loses to everything, then expiry,
    // then plain "ok".
    let (status_label, severity) = if routed_source.is_none() {
        (
            "no-route",
            path_severity(resolved.required, ExpiryStatus::Missing),
        )
    } else if let Some(es) = expiry_status {
        match es {
            ExpiryStatus::Expired => ("expired", path_severity(resolved.required, es)),
            ExpiryStatus::Expiring => ("expiring", path_severity(resolved.required, es)),
            ExpiryStatus::Ok => ("ok", path_severity(resolved.required, es)),
            ExpiryStatus::Missing => ("ok", CheckStatus::Pass),
        }
    } else {
        ("ok", CheckStatus::Pass)
    };

    let hint = match status_label {
        "no-route" => Some(format!(
            "no source covers '{}' — add a `[[route]]` or `[secret.\"...\"]` entry in sources.toml",
            resolved.path
        )),
        "expired" => Some(format!("rotate '{}' — expired", resolved.path)),
        "expiring" => Some(format!(
            "rotate '{}' — expires in {} day(s)",
            resolved.path,
            expires_in_days.unwrap_or(0)
        )),
        _ => None,
    };

    SecretCard {
        path: resolved.path.to_string(),
        required: resolved.required,
        routed_source,
        decision_kind,
        expires_at,
        expires_in_days,
        status_label,
        check_status: severity,
        hint,
    }
}

#[derive(Debug, Clone, Copy)]
enum ExpiryStatus {
    /// `expires_at` not set or unparseable.
    Missing,
    /// Plenty of time left.
    Ok,
    /// Within [`EXPIRY_WARNING_DAYS`] of expiry.
    Expiring,
    /// Already past `expires_at`.
    Expired,
}

fn expiry_status_for_days(days: i64) -> ExpiryStatus {
    if days < 0 {
        ExpiryStatus::Expired
    } else if days <= EXPIRY_WARNING_DAYS {
        ExpiryStatus::Expiring
    } else {
        ExpiryStatus::Ok
    }
}

/// Map `(required, ExpiryStatus or no-route)` to a `CheckStatus`.
/// Required paths fail the check on no-route / expired; optional
/// paths cap at Warning.
fn path_severity(required: bool, status: ExpiryStatus) -> CheckStatus {
    match status {
        ExpiryStatus::Ok | ExpiryStatus::Missing => CheckStatus::Pass,
        ExpiryStatus::Expiring => CheckStatus::Warning,
        ExpiryStatus::Expired => {
            if required {
                CheckStatus::Error
            } else {
                CheckStatus::Warning
            }
        }
    }
}

// =============================================================================
// Aggregation
// =============================================================================

fn aggregate_status(cards: &[SecretCard]) -> CheckStatus {
    let mut overall = CheckStatus::Pass;
    for card in cards {
        // No-route on a required path is the strictest failure;
        // bump to Error directly. Optional no-route is informational.
        if card.routed_source.is_none() && card.required {
            return CheckStatus::Error;
        }
        overall = worst(overall, card.check_status);
    }
    overall
}

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

fn summary_message(cards: &[SecretCard]) -> String {
    let total = cards.len();
    let required_total = cards.iter().filter(|c| c.required).count();
    let no_route = cards.iter().filter(|c| c.routed_source.is_none()).count();
    let expiring = cards
        .iter()
        .filter(|c| c.status_label == "expiring")
        .count();
    let expired = cards.iter().filter(|c| c.status_label == "expired").count();

    let mut bits = vec![
        format!("{required_total} required"),
        format!("{} optional", total - required_total),
    ];
    if no_route > 0 {
        bits.push(format!("{no_route} no-route"));
    }
    if expired > 0 {
        bits.push(format!("{expired} expired"));
    }
    if expiring > 0 {
        bits.push(format!("{expiring} expiring"));
    }
    format!(
        "{} secret(s) in active manifest: {}",
        total,
        bits.join(", ")
    )
}

fn cards_to_json(cards: &[SecretCard]) -> Value {
    let required: Vec<Value> = cards
        .iter()
        .filter(|c| c.required)
        .map(card_to_json)
        .collect();
    let optional: Vec<Value> = cards
        .iter()
        .filter(|c| !c.required)
        .map(card_to_json)
        .collect();
    json!({
        "required": required,
        "optional": optional,
    })
}

fn card_to_json(card: &SecretCard) -> Value {
    json!({
        "path": card.path,
        "required": card.required,
        "routed_source": card.routed_source,
        "decision": card.decision_kind,
        "expires_at": card.expires_at,
        "expires_in_days": card.expires_in_days,
        "status": card.status_label,
        "hint": card.hint,
    })
}

// =============================================================================
// Helpers
// =============================================================================

fn error_result(check: &dyn DiagnosticCheck, message: String) -> CheckResult {
    CheckResult {
        id: check.id().to_string(),
        category: check.category().to_string(),
        name: check.name().to_string(),
        status: CheckStatus::Error,
        message,
        details: None,
        fix_command: None,
        fix_url: None,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use devboy_storage::{IndexEntry, RouterConfig, SecretOrigin, SecretPath};

    fn p(s: &str) -> SecretPath {
        SecretPath::parse(s).unwrap()
    }

    fn fixture_resolved(path: &str, required: bool, expires_at: Option<&str>) -> ResolvedSecret {
        ResolvedSecret {
            path: p(path),
            required,
            origin: SecretOrigin::ProjectLocal,
            metadata: IndexEntry {
                expires_at: expires_at.map(str::to_owned),
                ..IndexEntry::default()
            },
        }
    }

    fn keychain_only_router() -> RouterConfig {
        RouterConfig::parse(
            r#"
            [[source]]
            name = "keychain"
            type = "keychain"

            [default]
            source = "keychain"
            "#,
        )
        .unwrap()
    }

    fn empty_router() -> RouterConfig {
        RouterConfig::default()
    }

    // -- Expiry math -------------------------------------------

    #[test]
    fn expiry_status_buckets_match_thresholds() {
        assert!(matches!(expiry_status_for_days(-1), ExpiryStatus::Expired));
        assert!(matches!(expiry_status_for_days(0), ExpiryStatus::Expiring));
        assert!(matches!(
            expiry_status_for_days(EXPIRY_WARNING_DAYS),
            ExpiryStatus::Expiring
        ));
        assert!(matches!(
            expiry_status_for_days(EXPIRY_WARNING_DAYS + 1),
            ExpiryStatus::Ok
        ));
    }

    #[test]
    fn path_severity_caps_optional_at_warning() {
        // Required + expired = Error (fails the gate).
        assert_eq!(
            path_severity(true, ExpiryStatus::Expired),
            CheckStatus::Error
        );
        // Optional + expired = Warning (no fail).
        assert_eq!(
            path_severity(false, ExpiryStatus::Expired),
            CheckStatus::Warning
        );
        // Both flavours of "expiring" → Warning.
        assert_eq!(
            path_severity(true, ExpiryStatus::Expiring),
            CheckStatus::Warning
        );
        // OK / Missing → Pass.
        assert_eq!(path_severity(true, ExpiryStatus::Ok), CheckStatus::Pass);
        assert_eq!(
            path_severity(true, ExpiryStatus::Missing),
            CheckStatus::Pass
        );
    }

    // -- build_card --------------------------------------------

    #[test]
    fn build_card_marks_no_route_when_router_empty() {
        let router = empty_router();
        let resolver = PathResolver::new(&router);
        let resolved = fixture_resolved("team/foo/bar", true, None);
        let card = build_card(
            &resolved,
            &resolver,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        assert!(card.routed_source.is_none());
        assert_eq!(card.decision_kind, "no-route");
        assert_eq!(card.status_label, "no-route");
        // No-route on a required path → Pass at the per-card
        // severity, but the aggregator promotes to Error because
        // required no-route is the bright-line failure for the
        // whole gate.
    }

    #[test]
    fn build_card_routes_through_default_keychain() {
        let router = keychain_only_router();
        let resolver = PathResolver::new(&router);
        let resolved = fixture_resolved("team/foo/bar", true, None);
        let card = build_card(
            &resolved,
            &resolver,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        assert_eq!(card.routed_source.as_deref(), Some("keychain"));
        assert_eq!(card.decision_kind, "default");
        assert_eq!(card.status_label, "ok");
        assert_eq!(card.check_status, CheckStatus::Pass);
        assert!(card.hint.is_none());
    }

    #[test]
    fn build_card_flags_expiring_within_two_weeks() {
        let router = keychain_only_router();
        let resolver = PathResolver::new(&router);
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        // 5 days out → expiring.
        let resolved = fixture_resolved("team/foo/bar", true, Some("2026-01-06"));
        let card = build_card(&resolved, &resolver, today);
        assert_eq!(card.expires_in_days, Some(5));
        assert_eq!(card.status_label, "expiring");
        assert_eq!(card.check_status, CheckStatus::Warning);
        assert!(card.hint.as_ref().unwrap().contains("5 day"));
    }

    #[test]
    fn build_card_flags_expired_required_as_error() {
        let router = keychain_only_router();
        let resolver = PathResolver::new(&router);
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let resolved = fixture_resolved("team/foo/bar", true, Some("2025-12-25"));
        let card = build_card(&resolved, &resolver, today);
        assert!(card.expires_in_days.unwrap() < 0);
        assert_eq!(card.status_label, "expired");
        assert_eq!(card.check_status, CheckStatus::Error);
    }

    #[test]
    fn build_card_optional_expired_caps_at_warning() {
        let router = keychain_only_router();
        let resolver = PathResolver::new(&router);
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let resolved = fixture_resolved("team/foo/bar", false, Some("2025-12-25"));
        let card = build_card(&resolved, &resolver, today);
        assert_eq!(card.status_label, "expired");
        assert_eq!(card.check_status, CheckStatus::Warning);
    }

    #[test]
    fn unparseable_expires_at_does_not_crash_card() {
        let router = keychain_only_router();
        let resolver = PathResolver::new(&router);
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let resolved = fixture_resolved("team/foo/bar", true, Some("not-a-date"));
        let card = build_card(&resolved, &resolver, today);
        assert!(card.expires_in_days.is_none());
        assert_eq!(card.status_label, "ok");
        assert_eq!(card.check_status, CheckStatus::Pass);
    }

    // -- Aggregation -------------------------------------------

    #[test]
    fn aggregate_required_no_route_is_error_even_if_others_ok() {
        let router = empty_router();
        let resolver = PathResolver::new(&router);
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let cards = vec![
            build_card(
                &fixture_resolved("team/foo/bar", true, None),
                &resolver,
                today,
            ),
            build_card(
                &fixture_resolved("team/baz/qux", false, None),
                &resolver,
                today,
            ),
        ];
        assert_eq!(aggregate_status(&cards), CheckStatus::Error);
    }

    #[test]
    fn aggregate_optional_no_route_does_not_fail_overall() {
        let router = keychain_only_router();
        let resolver = PathResolver::new(&router);
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        // Required paths route fine; optional one is fine too
        // (router has a default).
        let cards = vec![
            build_card(
                &fixture_resolved("team/foo/bar", true, None),
                &resolver,
                today,
            ),
            build_card(
                &fixture_resolved("team/baz/qux", false, None),
                &resolver,
                today,
            ),
        ];
        assert_eq!(aggregate_status(&cards), CheckStatus::Pass);
    }

    // -- summary_message ---------------------------------------

    #[test]
    fn summary_counts_required_optional_and_problem_categories() {
        let router = keychain_only_router();
        let resolver = PathResolver::new(&router);
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let cards = vec![
            build_card(
                &fixture_resolved("team/foo/bar", true, Some("2026-01-05")),
                &resolver,
                today,
            ),
            build_card(
                &fixture_resolved("team/baz/qux", false, Some("2025-12-25")),
                &resolver,
                today,
            ),
        ];
        let s = summary_message(&cards);
        assert!(s.contains("2 secret(s)"));
        assert!(s.contains("1 required"));
        assert!(s.contains("1 optional"));
        assert!(s.contains("expiring"));
        assert!(s.contains("expired"));
    }

    // -- worst -------------------------------------------------

    #[test]
    fn worst_picks_more_severe() {
        assert_eq!(
            worst(CheckStatus::Pass, CheckStatus::Warning),
            CheckStatus::Warning
        );
        assert_eq!(
            worst(CheckStatus::Warning, CheckStatus::Error),
            CheckStatus::Error
        );
        assert_eq!(
            worst(CheckStatus::Error, CheckStatus::Skipped),
            CheckStatus::Error
        );
    }

    // -- JSON projection ---------------------------------------

    #[test]
    fn cards_to_json_separates_required_and_optional() {
        let router = keychain_only_router();
        let resolver = PathResolver::new(&router);
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let cards = vec![
            build_card(
                &fixture_resolved("team/foo/bar", true, None),
                &resolver,
                today,
            ),
            build_card(
                &fixture_resolved("team/baz/qux", false, None),
                &resolver,
                today,
            ),
        ];
        let v = cards_to_json(&cards);
        let req = v["required"].as_array().unwrap();
        let opt = v["optional"].as_array().unwrap();
        assert_eq!(req.len(), 1);
        assert_eq!(opt.len(), 1);
        assert_eq!(req[0]["path"], "team/foo/bar");
        assert_eq!(opt[0]["path"], "team/baz/qux");
    }
}
