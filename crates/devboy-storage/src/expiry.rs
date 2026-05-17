//! Expiry + rotation reminders per [ADR-020] §3.
//!
//! Two timer concepts attach to every global-index entry:
//!
//! - **Hard expiry.** `entry.expires_at` is the upstream-reported
//!   date the credential stops working. P9.2's liveness probes
//!   write it back into the index when the upstream surfaces one
//!   (GitLab `expires_at`, GitHub
//!   `github-authentication-token-expiration` header).
//! - **Rotation cadence.** `entry.last_rotated_at` +
//!   `entry.rotate_every_days` is an *advisory* schedule the
//!   user attaches per secret. The framework doesn't enforce
//!   it; doctor warns when a secret is overdue.
//!
//! [`check_rotation_reminders`] is the pure function that walks
//! the index against `today` and returns a list of warnings.
//! Doctor (P10.1 / this commit's doctor wiring) renders them as a
//! single `Warning` row.
//!
//! ## Warning windows
//!
//! Per the task spec for P9.3:
//!
//! - `now > expires_at - 7d`  → `ExpiringSoon` /
//!   `Expired` (negative `days_remaining`).
//! - `now > last_rotated_at + rotate_every_days - 7d` →
//!   `RotationDueSoon` / `RotationOverdue`.
//!
//! Seven days is the warning window for *both* timers. P7.3
//! already uses 14 days for the *informational* `expires_at`
//! check on individual paths in `doctor`; this module is the
//! *rotation reminders* surface and intentionally fires
//! tighter.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md

use chrono::NaiveDate;

use crate::index::{GlobalIndex, IndexEntry};
use crate::secret_path::SecretPath;

/// Days-out-from-event when the warning starts to fire.
pub const WARNING_WINDOW_DAYS: i64 = 7;

// =============================================================================
// Public types
// =============================================================================

/// One reminder produced by [`check_rotation_reminders`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiryWarning {
    /// Which path the warning is about.
    pub path: SecretPath,
    /// Why we're warning.
    pub kind: ExpiryWarningKind,
}

/// Reason for an [`ExpiryWarning`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpiryWarningKind {
    /// `entry.expires_at` is within `WARNING_WINDOW_DAYS` of
    /// today.
    ExpiringSoon {
        /// `entry.expires_at` (verbatim ISO 8601 from the index).
        expires_at: String,
        /// Days from `today` until expiry. Positive.
        days_remaining: i64,
    },
    /// `entry.expires_at` is in the past — the credential is
    /// already expired.
    Expired {
        /// `entry.expires_at` (verbatim).
        expires_at: String,
        /// Days `today` is past expiry. Positive.
        days_overdue: i64,
    },
    /// `last_rotated_at + rotate_every_days` is within
    /// `WARNING_WINDOW_DAYS` of today — rotate soon.
    RotationDueSoon {
        /// `entry.last_rotated_at` (verbatim).
        last_rotated_at: String,
        /// `entry.rotate_every_days`.
        rotate_every_days: u32,
        /// Days until the schedule says rotate. Positive.
        days_remaining: i64,
    },
    /// Rotation schedule already overdue.
    RotationOverdue {
        /// `entry.last_rotated_at` (verbatim).
        last_rotated_at: String,
        /// `entry.rotate_every_days`.
        rotate_every_days: u32,
        /// Days `today` is past the rotation schedule. Positive.
        days_overdue: i64,
    },
}

// =============================================================================
// Pure check
// =============================================================================

/// Walk every entry in `index` and produce zero or more
/// [`ExpiryWarning`]s relative to `today`.
///
/// Order of warnings:
///
/// 1. Sorted by path (the index is a `BTreeMap`, so iteration
///    is already sorted — no extra sort needed).
/// 2. Within a path, the function emits *both* an expiry
///    warning and a rotation warning when both fire. The two
///    timers are independent; doctor renders them as separate
///    bullet points.
pub fn check_rotation_reminders(index: &GlobalIndex, today: NaiveDate) -> Vec<ExpiryWarning> {
    let mut out = Vec::new();
    for (path, entry) in index.iter() {
        if let Some(kind) = check_expiry(entry, today) {
            out.push(ExpiryWarning {
                path: path.clone(),
                kind,
            });
        }
        if let Some(kind) = check_rotation(entry, today) {
            out.push(ExpiryWarning {
                path: path.clone(),
                kind,
            });
        }
    }
    out
}

fn check_expiry(entry: &IndexEntry, today: NaiveDate) -> Option<ExpiryWarningKind> {
    let raw = entry.expires_at.as_deref()?;
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?;
    let delta = (date - today).num_days();
    if delta < 0 {
        Some(ExpiryWarningKind::Expired {
            expires_at: raw.to_owned(),
            days_overdue: -delta,
        })
    } else if delta <= WARNING_WINDOW_DAYS {
        Some(ExpiryWarningKind::ExpiringSoon {
            expires_at: raw.to_owned(),
            days_remaining: delta,
        })
    } else {
        None
    }
}

fn check_rotation(entry: &IndexEntry, today: NaiveDate) -> Option<ExpiryWarningKind> {
    let raw = entry.last_rotated_at.as_deref()?;
    let cadence = entry.rotate_every_days?;
    let last = NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?;
    let due = last + chrono::Duration::days(cadence as i64);
    let delta = (due - today).num_days();
    if delta < 0 {
        Some(ExpiryWarningKind::RotationOverdue {
            last_rotated_at: raw.to_owned(),
            rotate_every_days: cadence,
            days_overdue: -delta,
        })
    } else if delta <= WARNING_WINDOW_DAYS {
        Some(ExpiryWarningKind::RotationDueSoon {
            last_rotated_at: raw.to_owned(),
            rotate_every_days: cadence,
            days_remaining: delta,
        })
    } else {
        None
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{GlobalIndex, IndexEntry};

    fn p(s: &str) -> SecretPath {
        SecretPath::parse(s).unwrap()
    }

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn entry_with(
        expires_at: Option<&str>,
        last_rotated: Option<&str>,
        cadence: Option<u32>,
    ) -> IndexEntry {
        IndexEntry {
            expires_at: expires_at.map(str::to_owned),
            last_rotated_at: last_rotated.map(str::to_owned),
            rotate_every_days: cadence,
            ..IndexEntry::default()
        }
    }

    fn index_with_entry(path: &str, entry: IndexEntry) -> GlobalIndex {
        let mut idx = GlobalIndex::new();
        idx.insert(p(path), entry);
        idx
    }

    // -- Expiry checks ----------------------------------------

    #[test]
    fn no_expires_at_no_warning() {
        let idx = index_with_entry("a/b/c", entry_with(None, None, None));
        let w = check_rotation_reminders(&idx, date("2026-05-01"));
        assert!(w.is_empty());
    }

    #[test]
    fn expires_at_far_future_no_warning() {
        let idx = index_with_entry("a/b/c", entry_with(Some("2026-12-31"), None, None));
        let w = check_rotation_reminders(&idx, date("2026-05-01"));
        assert!(w.is_empty());
    }

    #[test]
    fn expires_at_within_warning_window_emits_expiring_soon() {
        let idx = index_with_entry("a/b/c", entry_with(Some("2026-05-05"), None, None));
        let w = check_rotation_reminders(&idx, date("2026-05-01"));
        assert_eq!(w.len(), 1);
        match &w[0].kind {
            ExpiryWarningKind::ExpiringSoon {
                expires_at,
                days_remaining,
            } => {
                assert_eq!(expires_at, "2026-05-05");
                assert_eq!(*days_remaining, 4);
            }
            other => panic!("expected ExpiringSoon, got {other:?}"),
        }
    }

    #[test]
    fn expires_at_at_exact_window_boundary_emits_expiring_soon() {
        // Today + 7 days = warning window cap (inclusive).
        let idx = index_with_entry("a/b/c", entry_with(Some("2026-05-08"), None, None));
        let w = check_rotation_reminders(&idx, date("2026-05-01"));
        match &w.first().unwrap().kind {
            ExpiryWarningKind::ExpiringSoon { days_remaining, .. } => {
                assert_eq!(*days_remaining, 7);
            }
            other => panic!("expected ExpiringSoon, got {other:?}"),
        }
    }

    #[test]
    fn expires_at_one_day_past_window_no_warning() {
        let idx = index_with_entry("a/b/c", entry_with(Some("2026-05-09"), None, None));
        let w = check_rotation_reminders(&idx, date("2026-05-01"));
        assert!(w.is_empty());
    }

    #[test]
    fn expires_at_in_the_past_emits_expired_with_days_overdue() {
        let idx = index_with_entry("a/b/c", entry_with(Some("2026-04-25"), None, None));
        let w = check_rotation_reminders(&idx, date("2026-05-01"));
        match &w.first().unwrap().kind {
            ExpiryWarningKind::Expired {
                expires_at,
                days_overdue,
            } => {
                assert_eq!(expires_at, "2026-04-25");
                assert_eq!(*days_overdue, 6);
            }
            other => panic!("expected Expired, got {other:?}"),
        }
    }

    #[test]
    fn unparseable_expires_at_yields_no_warning() {
        let idx = index_with_entry("a/b/c", entry_with(Some("not-a-date"), None, None));
        let w = check_rotation_reminders(&idx, date("2026-05-01"));
        assert!(w.is_empty());
    }

    // -- Rotation checks --------------------------------------

    #[test]
    fn no_rotation_metadata_no_warning() {
        let idx = index_with_entry("a/b/c", entry_with(None, Some("2026-04-01"), None));
        let w = check_rotation_reminders(&idx, date("2026-05-01"));
        assert!(w.is_empty());
    }

    #[test]
    fn rotation_far_in_future_no_warning() {
        // last_rotated 2026-01-01 + 90 days = 2026-04-01. Today
        // 2026-01-15 → due in 76 days → no warning.
        let idx = index_with_entry("a/b/c", entry_with(None, Some("2026-01-01"), Some(90)));
        let w = check_rotation_reminders(&idx, date("2026-01-15"));
        assert!(w.is_empty());
    }

    #[test]
    fn rotation_due_soon_emits_warning() {
        // last 2026-01-01 + 30 = 2026-01-31. Today 2026-01-29 →
        // 2 days remaining.
        let idx = index_with_entry("a/b/c", entry_with(None, Some("2026-01-01"), Some(30)));
        let w = check_rotation_reminders(&idx, date("2026-01-29"));
        match &w.first().unwrap().kind {
            ExpiryWarningKind::RotationDueSoon {
                last_rotated_at,
                rotate_every_days,
                days_remaining,
            } => {
                assert_eq!(last_rotated_at, "2026-01-01");
                assert_eq!(*rotate_every_days, 30);
                assert_eq!(*days_remaining, 2);
            }
            other => panic!("expected RotationDueSoon, got {other:?}"),
        }
    }

    #[test]
    fn rotation_overdue_emits_warning() {
        // last 2026-01-01 + 30 = 2026-01-31. Today 2026-02-10 →
        // 10 days overdue.
        let idx = index_with_entry("a/b/c", entry_with(None, Some("2026-01-01"), Some(30)));
        let w = check_rotation_reminders(&idx, date("2026-02-10"));
        match &w.first().unwrap().kind {
            ExpiryWarningKind::RotationOverdue { days_overdue, .. } => {
                assert_eq!(*days_overdue, 10);
            }
            other => panic!("expected RotationOverdue, got {other:?}"),
        }
    }

    // -- Joint expiry + rotation -------------------------------

    #[test]
    fn both_timers_within_window_emit_two_warnings() {
        // Expiring 2026-05-05 (4 days out) + rotation due
        // 2026-05-04 (last 2026-04-04 + 30).
        let idx = index_with_entry(
            "a/b/c",
            entry_with(Some("2026-05-05"), Some("2026-04-04"), Some(30)),
        );
        let w = check_rotation_reminders(&idx, date("2026-05-01"));
        assert_eq!(w.len(), 2);
        // Same path on both warnings.
        assert_eq!(w[0].path, w[1].path);
        // Expiry warning emitted first per the loop structure.
        assert!(matches!(w[0].kind, ExpiryWarningKind::ExpiringSoon { .. }));
        assert!(matches!(
            w[1].kind,
            ExpiryWarningKind::RotationDueSoon { .. }
        ));
    }

    // -- record_expiry / record_rotation -----------------------

    #[test]
    fn record_expiry_updates_existing_entry_returns_true() {
        let mut idx = index_with_entry("a/b/c", entry_with(None, None, None));
        let changed = idx.record_expiry(&p("a/b/c"), "2026-08-01");
        assert!(changed);
        let entry = idx.get(&p("a/b/c")).unwrap();
        assert_eq!(entry.expires_at.as_deref(), Some("2026-08-01"));
    }

    #[test]
    fn record_expiry_unchanged_returns_false() {
        let mut idx = index_with_entry("a/b/c", entry_with(Some("2026-08-01"), None, None));
        let changed = idx.record_expiry(&p("a/b/c"), "2026-08-01");
        assert!(!changed, "no-op write should report unchanged");
    }

    #[test]
    fn record_expiry_missing_path_returns_false() {
        let mut idx = GlobalIndex::new();
        let changed = idx.record_expiry(&p("a/b/c"), "2026-08-01");
        assert!(!changed);
    }

    #[test]
    fn record_rotation_updates_existing_entry() {
        let mut idx = index_with_entry("a/b/c", entry_with(None, None, None));
        let changed = idx.record_rotation(&p("a/b/c"), "2026-05-01");
        assert!(changed);
        let entry = idx.get(&p("a/b/c")).unwrap();
        assert_eq!(entry.last_rotated_at.as_deref(), Some("2026-05-01"));
    }

    // -- save_to / save round-trip -----------------------------

    #[test]
    fn save_to_round_trip_preserves_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("subdir").join("index.toml");
        let mut idx = index_with_entry(
            "team/x/y",
            IndexEntry {
                expires_at: Some("2026-08-01".to_owned()),
                description: Some("test".to_owned()),
                ..IndexEntry::default()
            },
        );
        idx.record_expiry(&p("team/x/y"), "2026-09-01");
        idx.save_to(&path).unwrap();

        let reloaded = GlobalIndex::load_from(&path).unwrap();
        let entry = reloaded.get(&p("team/x/y")).unwrap();
        assert_eq!(entry.expires_at.as_deref(), Some("2026-09-01"));
        assert_eq!(entry.description.as_deref(), Some("test"));
    }
}
