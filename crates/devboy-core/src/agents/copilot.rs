//! GitHub Copilot CLI detector.
//!
//! Two formats are supported:
//! - **New** (≥1.0, apr 2026+): `~/.copilot/session-state/<uuid>/events.jsonl`
//! - **Old** (<1.0): `~/.copilot/session-state/<uuid>.jsonl`
//!
//! Sessions = count of (new dirs with events.jsonl) + (old jsonl files).
//! last_used = max mtime across both.

use std::path::Path;

use super::fs_util::to_utc;
use super::{AgentDetector, AgentSnapshot, InstallStatus};

const ID: &str = "copilot";
const DISPLAY_NAME: &str = "GitHub Copilot CLI";
const MAX_ENTRIES: usize = 5_000;

pub struct CopilotDetector;

impl AgentDetector for CopilotDetector {
    fn id(&self) -> &'static str {
        ID
    }
    fn display_name(&self) -> &'static str {
        DISPLAY_NAME
    }

    fn detect(&self, home: &Path) -> AgentSnapshot {
        let copilot_dir = home.join(".copilot");
        let session_state = copilot_dir.join("session-state");
        let paths_checked = vec![copilot_dir.clone(), session_state.clone()];

        let dir_present = copilot_dir.is_dir();
        let binary_present = which::which("copilot").is_ok();

        let status = if dir_present || binary_present {
            InstallStatus::Yes
        } else {
            InstallStatus::No
        };
        if status == InstallStatus::No {
            return empty(paths_checked);
        }

        let (sessions, last_used) = walk_session_state(&session_state);

        AgentSnapshot {
            id: ID,
            display_name: DISPLAY_NAME,
            status,
            sessions: (sessions > 0).then_some(sessions),
            last_used,
            score: 0.0,
            paths_checked,
        }
    }
}

fn walk_session_state(root: &Path) -> (u64, Option<chrono::DateTime<chrono::Utc>>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return (0, None);
    };
    let mut count = 0u64;
    let mut best: Option<chrono::DateTime<chrono::Utc>> = None;
    for entry in entries.flatten().take(MAX_ENTRIES) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let candidate_mtime = if file_type.is_dir() {
            let events = path.join("events.jsonl");
            if events.exists() {
                count += 1;
                events
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(to_utc)
            } else {
                None
            }
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            count += 1;
            entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(to_utc)
        } else {
            None
        };
        if let Some(t) = candidate_mtime {
            best = Some(best.map_or(t, |b| b.max(t)));
        }
    }
    (count, best)
}

fn empty(paths_checked: Vec<std::path::PathBuf>) -> AgentSnapshot {
    AgentSnapshot {
        id: ID,
        display_name: DISPLAY_NAME,
        status: InstallStatus::No,
        sessions: None,
        last_used: None,
        score: 0.0,
        paths_checked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn detects_mixed_old_and_new_format() {
        let home = tempdir().unwrap();
        let state = home.path().join(".copilot/session-state");
        fs::create_dir_all(&state).unwrap();
        // New format
        fs::create_dir_all(state.join("aaa-new1")).unwrap();
        fs::write(state.join("aaa-new1/events.jsonl"), b"{}\n").unwrap();
        fs::create_dir_all(state.join("bbb-new2")).unwrap();
        fs::write(state.join("bbb-new2/events.jsonl"), b"{}\n").unwrap();
        // Old format
        fs::write(state.join("ccc-old1.jsonl"), b"{}\n").unwrap();

        let snap = CopilotDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert_eq!(snap.sessions, Some(3));
    }
}
