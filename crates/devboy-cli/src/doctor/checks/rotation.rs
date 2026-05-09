//! Doctor "Rotation reminders" section per [ADR-021] §9 +
//! ADR-020 §3 (`expires_at` / `last_rotated_at` /
//! `rotate_every_days`).
//!
//! Walks the global index and surfaces every secret whose hard
//! expiry or rotation cadence is within 7 days of today (or
//! already past). The math lives in
//! [`devboy_storage::check_rotation_reminders`]; this module is
//! just the doctor wrapper that turns the warnings into a single
//! `CheckResult`.
//!
//! ## Severity
//!
//! Status fold:
//!
//! - any [`Expired`](devboy_storage::ExpiryWarningKind::Expired)
//!   or
//!   [`RotationOverdue`](devboy_storage::ExpiryWarningKind::RotationOverdue)
//!   → `Warning` overall (the existing P7.3
//!   `ContextSecretsCheck` is the place that fails the gate; this
//!   reminder check stays advisory so the same condition isn't
//!   double-counted).
//! - only [`ExpiringSoon`](devboy_storage::ExpiryWarningKind::ExpiringSoon)
//!   /
//!   [`RotationDueSoon`](devboy_storage::ExpiryWarningKind::RotationDueSoon)
//!   → `Warning`.
//! - empty result → `Pass`.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use async_trait::async_trait;
use chrono::Utc;
use devboy_storage::{ExpiryWarning, ExpiryWarningKind, GlobalIndex, check_rotation_reminders};
use serde_json::{Value, json};

use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};

pub struct RotationRemindersCheck;

#[async_trait]
impl DiagnosticCheck for RotationRemindersCheck {
    fn id(&self) -> &'static str {
        "rotation-reminders"
    }

    fn name(&self) -> &'static str {
        "Rotation reminders"
    }

    fn category(&self) -> &'static str {
        "Secrets"
    }

    async fn run(&self, _ctx: &DiagnosticContext) -> CheckResult {
        let index = match GlobalIndex::load() {
            Ok(i) => i,
            Err(e) => {
                return CheckResult {
                    id: self.id().to_string(),
                    category: self.category().to_string(),
                    name: self.name().to_string(),
                    status: CheckStatus::Error,
                    message: format!("Failed to load global index: {e}"),
                    details: None,
                    fix_command: None,
                    fix_url: None,
                };
            }
        };

        if index.is_empty() {
            return CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Skipped,
                message: "No entries declared in the global index".to_string(),
                details: None,
                fix_command: None,
                fix_url: None,
            };
        }

        let today = Utc::now().date_naive();
        let warnings = check_rotation_reminders(&index, today);

        if warnings.is_empty() {
            return CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Pass,
                message: format!(
                    "{} entr{} — none expiring or due for rotation within {} day(s)",
                    index.len(),
                    if index.len() == 1 { "y" } else { "ies" },
                    devboy_storage::WARNING_WINDOW_DAYS
                ),
                details: None,
                fix_command: None,
                fix_url: None,
            };
        }

        let summary = summary_message(&warnings);
        CheckResult {
            id: self.id().to_string(),
            category: self.category().to_string(),
            name: self.name().to_string(),
            status: CheckStatus::Warning,
            message: summary,
            details: Some(warnings_to_json(&warnings)),
            fix_command: Some("devboy secrets rotate <path>".to_string()),
            fix_url: None,
        }
    }
}

// =============================================================================
// Rendering helpers
// =============================================================================

fn summary_message(warnings: &[ExpiryWarning]) -> String {
    let expiring = warnings
        .iter()
        .filter(|w| matches!(w.kind, ExpiryWarningKind::ExpiringSoon { .. }))
        .count();
    let expired = warnings
        .iter()
        .filter(|w| matches!(w.kind, ExpiryWarningKind::Expired { .. }))
        .count();
    let rotation_due = warnings
        .iter()
        .filter(|w| matches!(w.kind, ExpiryWarningKind::RotationDueSoon { .. }))
        .count();
    let rotation_overdue = warnings
        .iter()
        .filter(|w| matches!(w.kind, ExpiryWarningKind::RotationOverdue { .. }))
        .count();
    let mut bits = Vec::new();
    if expired > 0 {
        bits.push(format!("{expired} expired"));
    }
    if expiring > 0 {
        bits.push(format!("{expiring} expiring"));
    }
    if rotation_overdue > 0 {
        bits.push(format!("{rotation_overdue} overdue rotations"));
    }
    if rotation_due > 0 {
        bits.push(format!("{rotation_due} rotations due soon"));
    }
    format!(
        "{} reminder(s) within {} day(s): {}",
        warnings.len(),
        devboy_storage::WARNING_WINDOW_DAYS,
        bits.join(", ")
    )
}

fn warnings_to_json(warnings: &[ExpiryWarning]) -> Value {
    let entries: Vec<Value> = warnings.iter().map(warning_to_json).collect();
    json!({ "warnings": entries })
}

fn warning_to_json(w: &ExpiryWarning) -> Value {
    match &w.kind {
        ExpiryWarningKind::ExpiringSoon {
            expires_at,
            days_remaining,
        } => json!({
            "path": w.path.to_string(),
            "kind": "expiring_soon",
            "expires_at": expires_at,
            "days_remaining": days_remaining,
        }),
        ExpiryWarningKind::Expired {
            expires_at,
            days_overdue,
        } => json!({
            "path": w.path.to_string(),
            "kind": "expired",
            "expires_at": expires_at,
            "days_overdue": days_overdue,
        }),
        ExpiryWarningKind::RotationDueSoon {
            last_rotated_at,
            rotate_every_days,
            days_remaining,
        } => json!({
            "path": w.path.to_string(),
            "kind": "rotation_due_soon",
            "last_rotated_at": last_rotated_at,
            "rotate_every_days": rotate_every_days,
            "days_remaining": days_remaining,
        }),
        ExpiryWarningKind::RotationOverdue {
            last_rotated_at,
            rotate_every_days,
            days_overdue,
        } => json!({
            "path": w.path.to_string(),
            "kind": "rotation_overdue",
            "last_rotated_at": last_rotated_at,
            "rotate_every_days": rotate_every_days,
            "days_overdue": days_overdue,
        }),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_storage::SecretPath;

    fn p(s: &str) -> SecretPath {
        SecretPath::parse(s).unwrap()
    }

    #[test]
    fn summary_message_lists_each_category_present() {
        let warnings = vec![
            ExpiryWarning {
                path: p("a/b/c"),
                kind: ExpiryWarningKind::Expired {
                    expires_at: "2025-12-01".into(),
                    days_overdue: 30,
                },
            },
            ExpiryWarning {
                path: p("d/e/f"),
                kind: ExpiryWarningKind::ExpiringSoon {
                    expires_at: "2026-05-08".into(),
                    days_remaining: 5,
                },
            },
            ExpiryWarning {
                path: p("g/h/i"),
                kind: ExpiryWarningKind::RotationOverdue {
                    last_rotated_at: "2026-01-01".into(),
                    rotate_every_days: 30,
                    days_overdue: 120,
                },
            },
        ];
        let s = summary_message(&warnings);
        assert!(s.contains("3 reminder"));
        assert!(s.contains("1 expired"));
        assert!(s.contains("1 expiring"));
        assert!(s.contains("1 overdue rotation"));
        assert!(s.contains("7 day"));
    }

    #[test]
    fn warning_to_json_preserves_kind_label_and_payload() {
        let w = ExpiryWarning {
            path: p("a/b/c"),
            kind: ExpiryWarningKind::ExpiringSoon {
                expires_at: "2026-05-08".into(),
                days_remaining: 5,
            },
        };
        let v = warning_to_json(&w);
        assert_eq!(v["path"], "a/b/c");
        assert_eq!(v["kind"], "expiring_soon");
        assert_eq!(v["expires_at"], "2026-05-08");
        assert_eq!(v["days_remaining"], 5);
    }

    #[test]
    fn warnings_to_json_wraps_in_warnings_array() {
        let warnings = vec![ExpiryWarning {
            path: p("a/b/c"),
            kind: ExpiryWarningKind::RotationDueSoon {
                last_rotated_at: "2026-04-01".into(),
                rotate_every_days: 30,
                days_remaining: 3,
            },
        }];
        let v = warnings_to_json(&warnings);
        let arr = v["warnings"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["kind"], "rotation_due_soon");
        assert_eq!(arr[0]["rotate_every_days"], 30);
    }
}
