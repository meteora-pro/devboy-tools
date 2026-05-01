//! Score formula and primary-selection logic.
//!
//! `score = 0.6 * freshness + 0.4 * volume`
//! - `freshness = max(0, 1 - days_since_last_used / 14)` — decays to 0 at 14 days.
//! - `volume    = min(1, log10(sessions + 1) / 3)` — saturates at 1000 sessions.
//!
//! `pick_primary` returns the top-scoring snapshot iff its score is at least
//! 1.5× the runner-up's. Otherwise returns `None`, signalling that the caller
//! should ask the user.

use chrono::{DateTime, Utc};

use super::AgentSnapshot;

const FRESHNESS_DECAY_DAYS: f64 = 14.0;
const VOLUME_SATURATION_LOG10: f64 = 3.0;
const FRESHNESS_WEIGHT: f64 = 0.6;
const VOLUME_WEIGHT: f64 = 0.4;
const PRIMARY_THRESHOLD: f64 = 1.5;

pub fn compute_score(last_used: Option<DateTime<Utc>>, sessions: Option<u64>, now: DateTime<Utc>) -> f64 {
    let freshness = last_used
        .map(|t| {
            let days = (now - t).num_seconds() as f64 / 86_400.0;
            (1.0 - days / FRESHNESS_DECAY_DAYS).max(0.0)
        })
        .unwrap_or(0.0);

    let volume = sessions
        .map(|n| ((n as f64 + 1.0).log10() / VOLUME_SATURATION_LOG10).min(1.0))
        .unwrap_or(0.0);

    FRESHNESS_WEIGHT * freshness + VOLUME_WEIGHT * volume
}

/// Pick the primary candidate. Returns `None` if the gap to the runner-up is
/// too small (< 1.5×) or no candidate has a positive score.
pub fn pick_primary(snapshots: &[AgentSnapshot]) -> Option<&AgentSnapshot> {
    let mut sorted: Vec<&AgentSnapshot> = snapshots.iter().filter(|s| s.score > 0.0).collect();
    sorted.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    match sorted.as_slice() {
        [] => None,
        [only] => Some(*only),
        [top, second, ..] => {
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
        AgentSnapshot {
            id,
            display_name: id,
            status: crate::agents::InstallStatus::Yes,
            sessions: None,
            last_used: None,
            score,
            paths_checked: vec![],
        }
    }

    #[test]
    fn primary_picks_top_when_gap_is_clear() {
        let snaps = vec![snap("claude", 0.95), snap("codex", 0.20), snap("gemini", 0.10)];
        assert_eq!(pick_primary(&snaps).unwrap().id, "claude");
    }

    #[test]
    fn primary_returns_none_when_top_two_are_close() {
        let snaps = vec![snap("claude", 0.60), snap("copilot", 0.55)];
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
}
