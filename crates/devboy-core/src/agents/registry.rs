//! Registry that runs every per-agent detector and returns sorted snapshots.
//!
//! `detect_all` reads `dirs::home_dir()` and is the public entrypoint.
//! `detect_all_with_home` is for tests / users who want to point at a
//! sandboxed home (e.g. tempdir fixtures).

use std::path::Path;

use chrono::Utc;

use super::{AgentDetector, AgentSnapshot};

fn detectors() -> Vec<Box<dyn AgentDetector>> {
    vec![
        Box::new(super::claude::ClaudeDetector),
        Box::new(super::copilot::CopilotDetector),
        Box::new(super::codex::CodexDetector),
        Box::new(super::kimi::KimiDetector),
        Box::new(super::cursor::CursorDetector),
        Box::new(super::gemini::GeminiDetector),
        Box::new(super::antigravity::AntigravityDetector),
    ]
}

/// Run every detector against the user's real home dir. Returns snapshots
/// sorted by score (descending). Empty home → empty result.
pub fn detect_all() -> Vec<AgentSnapshot> {
    match dirs::home_dir() {
        Some(home) => detect_all_with_home(&home),
        None => Vec::new(),
    }
}

/// Run every detector against the given home dir. Used by tests with
/// synthetic fixtures.
pub fn detect_all_with_home(home: &Path) -> Vec<AgentSnapshot> {
    let now = Utc::now();
    let mut out: Vec<AgentSnapshot> = detectors()
        .into_iter()
        .map(|d| {
            let mut snap = d.detect(home);
            snap.score = super::score::compute_score(snap.last_used, snap.sessions, now);
            snap
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::InstallStatus;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn returns_one_snapshot_per_known_detector() {
        let home = tempdir().unwrap();
        let snaps = detect_all_with_home(home.path());
        // Seven detectors: claude, copilot, codex, kimi, cursor, gemini, antigravity.
        assert_eq!(snaps.len(), 7);
        let ids: std::collections::HashSet<_> = snaps.iter().map(|s| s.id).collect();
        for expected in [
            "claude",
            "copilot",
            "codex",
            "kimi",
            "cursor",
            "gemini",
            "antigravity",
        ] {
            assert!(ids.contains(expected), "missing detector for {expected}");
        }
    }

    #[test]
    fn scores_are_clamped_in_unit_interval() {
        let home = tempdir().unwrap();
        let snaps = detect_all_with_home(home.path());
        for s in &snaps {
            assert!(
                (0.0..=1.0).contains(&s.score),
                "score out of range for {}: {}",
                s.id,
                s.score
            );
        }
    }

    #[test]
    fn snapshots_sorted_by_score_descending() {
        // Build a synthetic Claude install — many sessions, recent mtime.
        // Should rank highest among the seven on this synthetic home.
        let home = tempdir().unwrap();
        let projects = home.path().join(".claude/projects/-Users-x-foo");
        fs::create_dir_all(&projects).unwrap();
        for i in 0..50 {
            fs::write(projects.join(format!("session-{i}.jsonl")), b"{}\n").unwrap();
        }

        let snaps = detect_all_with_home(home.path());
        for window in snaps.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "not sorted: {} ({}) before {} ({})",
                window[0].id,
                window[0].score,
                window[1].id,
                window[1].score,
            );
        }
        assert_eq!(snaps[0].id, "claude");
        assert_eq!(snaps[0].status, InstallStatus::Yes);
        assert_eq!(snaps[0].sessions, Some(50));
    }
}
