//! Gemini CLI detector.
//!
//! Gemini CLI keeps a tiny footprint — `~/.gemini/` ~50KB total. There is no
//! per-session events file we can count meaningfully. Best heuristics:
//! - install marker: `~/.gemini/` exists OR `gemini` in PATH.
//! - sessions: count of subdirectories under `~/.gemini/history/` (one per
//!   project the user touched with Gemini); upper bound, not per-session.
//! - last_used: max of `state.json` / `settings.json` mtimes.
//!
//! Antigravity (Google's IDE-agent) shares `~/.gemini/antigravity/` and is a
//! separate detector (`agents::antigravity`).

use std::path::Path;

use super::fs_util::{count_subdirs, to_utc};
use super::{AgentDetector, AgentSnapshot, InstallStatus};

const ID: &str = "gemini";
const DISPLAY_NAME: &str = "Gemini CLI";

/// Gemini Detector.
pub struct GeminiDetector;

impl AgentDetector for GeminiDetector {
    fn id(&self) -> &'static str {
        ID
    }
    fn display_name(&self) -> &'static str {
        DISPLAY_NAME
    }

    fn detect(&self, home: &Path) -> AgentSnapshot {
        let gemini_dir = home.join(".gemini");
        let history = gemini_dir.join("history");
        let state_json = gemini_dir.join("state.json");
        let settings = gemini_dir.join("settings.json");
        let paths_checked = vec![
            gemini_dir.clone(),
            history.clone(),
            state_json.clone(),
            settings.clone(),
        ];

        let dir_present = gemini_dir.is_dir();
        let binary_present = which::which("gemini").is_ok();

        let status = if dir_present || binary_present {
            InstallStatus::Yes
        } else {
            InstallStatus::No
        };
        if status == InstallStatus::No {
            return AgentSnapshot {
                id: ID,
                display_name: DISPLAY_NAME,
                status,
                sessions: None,
                last_used: None,
                score: 0.0,
                paths_checked,
            };
        }

        let projects = count_subdirs(&history);
        let last_used = [&state_json, &settings, &gemini_dir]
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .filter_map(|m| m.modified().ok())
            .filter_map(to_utc)
            .max();

        AgentSnapshot {
            id: ID,
            display_name: DISPLAY_NAME,
            status,
            sessions: (projects > 0).then_some(projects),
            last_used,
            score: 0.0,
            paths_checked,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn counts_history_project_dirs() {
        let home = tempdir().unwrap();
        let gemini = home.path().join(".gemini");
        fs::create_dir_all(gemini.join("history/devboy-tools")).unwrap();
        fs::write(gemini.join("history/devboy-tools/.project_root"), b"x").unwrap();
        fs::create_dir_all(gemini.join("history/another")).unwrap();
        fs::write(gemini.join("state.json"), b"{}").unwrap();
        fs::write(gemini.join("settings.json"), b"{}").unwrap();

        let snap = GeminiDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert_eq!(snap.sessions, Some(2));
        assert!(snap.last_used.is_some());
    }

    #[test]
    fn no_gemini_dir_means_not_installed() {
        let home = tempdir().unwrap();
        let snap = GeminiDetector.detect(home.path());
        if which::which("gemini").is_err() {
            assert_eq!(snap.status, InstallStatus::No);
        }
    }

    #[test]
    fn gemini_dir_without_history_still_reports_install() {
        let home = tempdir().unwrap();
        fs::create_dir_all(home.path().join(".gemini")).unwrap();
        let snap = GeminiDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert!(snap.sessions.is_none());
    }
}
