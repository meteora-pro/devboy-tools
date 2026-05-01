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
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out
}
