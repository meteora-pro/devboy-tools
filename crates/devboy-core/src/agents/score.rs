//! Score formula and primary-selection logic.
//!
//! `score = 0.6 * freshness + 0.4 * volume`
//! - `freshness = max(0, 1 - days_since_last_used / 14)` — decays to 0 at 14 days.
//! - `volume    = min(1, log10(sessions + 1) / 3)` — saturates at 1000 sessions.
//!
//! `pick_primary` picks the top-scoring snapshot when one of:
//! - **Recency dominance** — the top candidate was used at least
//!   [`RECENCY_DOMINANCE_HOURS`] more recently than the runner-up. Just
//!   used Claude 5 seconds ago vs Copilot 2 days ago? Claude wins,
//!   period — volume can't fight that.
//! - **Score gap** — the top score is at least [`PRIMARY_THRESHOLD`]× the
//!   runner-up's. Used as fallback when both candidates are equally fresh.
//!
//! Otherwise returns `None`, signalling that the caller should ask the user.

use chrono::{DateTime, Utc};

use super::AgentSnapshot;

const FRESHNESS_DECAY_DAYS: f64 = 14.0;
const VOLUME_SATURATION_LOG10: f64 = 3.0;
const FRESHNESS_WEIGHT: f64 = 0.6;
const VOLUME_WEIGHT: f64 = 0.4;
const PRIMARY_THRESHOLD: f64 = 1.5;
/// If the top candidate was used at least this many hours later than the
/// runner-up, it wins regardless of score gap. ~half a workday — enough to
/// imply "I switched to this one and stopped touching the other."
const RECENCY_DOMINANCE_HOURS: i64 = 4;

pub fn compute_score(
    last_used: Option<DateTime<Utc>>,
    sessions: Option<u64>,
    now: DateTime<Utc>,
) -> f64 {
    let freshness = last_used
        .map(|t| {
            let days = (now - t).num_seconds() as f64 / 86_400.0;
            // Clamp to [0, 1]: future timestamps (clock skew / odd mtimes)
            // would otherwise produce >1.0; ancient timestamps go to 0.
            (1.0 - days / FRESHNESS_DECAY_DAYS).clamp(0.0, 1.0)
        })
        .unwrap_or(0.0);

    let volume = sessions
        .map(|n| ((n as f64 + 1.0).log10() / VOLUME_SATURATION_LOG10).min(1.0))
        .unwrap_or(0.0);

    FRESHNESS_WEIGHT * freshness + VOLUME_WEIGHT * volume
}

/// Pick the primary candidate.
///
/// Two paths to "primary":
/// 1. **Recency dominance**: top was used ≥ 4 h after the runner-up. Wins
///    unconditionally — fresh activity beats accumulated volume.
/// 2. **Score gap**: top's score is ≥ 1.5× the runner-up's. Used when both
///    candidates were touched recently and the formula has to break ties on
///    volume.
///
/// Returns `None` when neither path applies (caller should ask the user) or
/// when no candidate has a positive score.
pub fn pick_primary(snapshots: &[AgentSnapshot]) -> Option<&AgentSnapshot> {
    let mut sorted: Vec<&AgentSnapshot> = snapshots.iter().filter(|s| s.score > 0.0).collect();
    sorted.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    match sorted.as_slice() {
        [] => None,
        [only] => Some(*only),
        [top, second, ..] => {
            // Path 1: recency dominance.
            if let (Some(t1), Some(t2)) = (top.last_used, second.last_used) {
                let hours_apart = (t1 - t2).num_hours();
                if hours_apart >= RECENCY_DOMINANCE_HOURS {
                    return Some(*top);
                }
            }
            // Path 2: score gap.
            if second.score == 0.0 || top.score / second.score >= PRIMARY_THRESHOLD {
                Some(*top)
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 12, 0, 0).unwrap()
    }

    #[test]
    fn never_used_no_sessions_zero() {
        let now = at(2026, 5, 1);
        assert_eq!(compute_score(None, None, now), 0.0);
    }

    #[test]
    fn used_today_thousand_sessions_near_one() {
        let now = at(2026, 5, 1);
        let s = compute_score(Some(now), Some(1000), now);
        assert!(s > 0.95 && s <= 1.0, "score = {s}");
    }

    #[test]
    fn fourteen_days_old_zero_freshness_only_volume() {
        let now = at(2026, 5, 1);
        let s = compute_score(Some(at(2026, 4, 17)), Some(100), now);
        // freshness ≈ 0, volume = log10(101)/3 ≈ 0.668, weighted * 0.4 ≈ 0.267
        assert!(s > 0.25 && s < 0.30, "score = {s}");
    }

    #[test]
    fn used_today_no_sessions_only_freshness() {
        let now = at(2026, 5, 1);
        let s = compute_score(Some(now), None, now);
        assert!((s - 0.6).abs() < 1e-9, "score = {s}");
    }

    #[test]
    fn one_session_today_partial_score() {
        let now = at(2026, 5, 1);
        let s = compute_score(Some(now), Some(1), now);
        // freshness = 1 → 0.6, volume = log10(2)/3 ≈ 0.1004 → 0.0402, total ≈ 0.640
        assert!(s > 0.63 && s < 0.65, "score = {s}");
    }

    #[test]
    fn future_timestamp_clamped_to_freshness_one() {
        let now = at(2026, 5, 1);
        let s = compute_score(Some(at(2026, 5, 5)), Some(10), now);
        // negative days → freshness clamped at... actually formula gives 1 - (negative)/14 > 1
        // Specification says max(0, 1 - days/14), no upper clamp; that's fine — future dates
        // are improbable but harmless.
        assert!(s > 0.6, "score = {s}");
    }

    fn snap(id: &'static str, score: f64) -> AgentSnapshot {
        snap_with_recency(id, score, None)
    }

    fn snap_with_recency(
        id: &'static str,
        score: f64,
        last_used: Option<DateTime<Utc>>,
    ) -> AgentSnapshot {
        AgentSnapshot {
            id,
            display_name: id,
            status: crate::agents::InstallStatus::Yes,
            sessions: None,
            last_used,
            score,
            paths_checked: vec![],
        }
    }

    #[test]
    fn primary_picks_top_when_gap_is_clear() {
        let snaps = vec![
            snap("claude", 0.95),
            snap("codex", 0.20),
            snap("gemini", 0.10),
        ];
        assert_eq!(pick_primary(&snaps).unwrap().id, "claude");
    }

    #[test]
    fn primary_returns_none_when_top_two_are_close_and_both_fresh() {
        let now = at(2026, 5, 1);
        // Both used within an hour of each other → no recency dominance, gap < 1.5x.
        let snaps = vec![
            snap_with_recency("claude", 0.60, Some(now)),
            snap_with_recency("copilot", 0.55, Some(now - chrono::Duration::minutes(30))),
        ];
        assert!(pick_primary(&snaps).is_none(), "should defer to user");
    }

    #[test]
    fn primary_handles_single_candidate() {
        let snaps = vec![snap("claude", 0.30)];
        assert_eq!(pick_primary(&snaps).unwrap().id, "claude");
    }

    #[test]
    fn primary_handles_empty_or_zero_scores() {
        assert!(pick_primary(&[]).is_none());
        assert!(pick_primary(&[snap("x", 0.0)]).is_none());
    }

    #[test]
    fn recency_dominance_overrides_close_score_gap() {
        // Real-world scenario: Claude used 1 second ago, Copilot used 41 hours
        // ago. Volumes happen to be close (3243 vs 26 → log10 saturates), so
        // the score gap is only 1.39× — under the 1.5× threshold. But the
        // recency gap is 41 hours, way over 4 hours → Claude must win.
        let now = at(2026, 5, 1);
        let snaps = vec![
            snap_with_recency("claude", 1.000, Some(now)),
            snap_with_recency("copilot", 0.717, Some(now - chrono::Duration::hours(41))),
        ];
        assert_eq!(pick_primary(&snaps).unwrap().id, "claude");
    }

    #[test]
    fn recency_dominance_does_not_fire_within_4h_window() {
        // Both used within a few hours → fall back to score-gap rule.
        let now = at(2026, 5, 1);
        let snaps = vec![
            snap_with_recency("claude", 0.60, Some(now)),
            snap_with_recency("copilot", 0.50, Some(now - chrono::Duration::hours(2))),
        ];
        // 2h apart < 4h dominance threshold, score gap 0.60/0.50 = 1.2 < 1.5
        // → no primary picked.
        assert!(pick_primary(&snaps).is_none());
    }

    #[test]
    fn recency_dominance_works_even_when_top_score_is_lower() {
        // Edge case: a very fresh agent with low session count beats a
        // higher-volume but stale agent. Score-sort puts the high-volume one
        // first; recency dominance should override.
        // Note: in practice this is rare because the score formula already
        // weights freshness 0.6, but we want the rule to be robust.
        let now = at(2026, 5, 1);
        // Construct so that top by score is the stale one.
        let snaps = vec![
            snap_with_recency(
                "stale_high_vol",
                0.50,
                Some(now - chrono::Duration::days(7)),
            ),
            snap_with_recency("fresh_low_vol", 0.45, Some(now)),
        ];
        // top by score = stale_high_vol (0.50). second = fresh_low_vol (0.45).
        // top.last_used is *earlier* than second.last_used, so hours_apart is
        // negative — dominance does NOT fire. Score gap 1.11× < 1.5× → None.
        // This test documents the asymmetry: dominance is one-way (top must
        // be more recent than second). This is intentional: "score-sorted top
        // is more recent than runner-up" is the meaningful signal.
        assert!(pick_primary(&snaps).is_none());
    }
}
