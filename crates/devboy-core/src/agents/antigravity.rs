//! Antigravity (Google IDE-agent) detector.
//!
//! Antigravity shares storage with Gemini CLI but is a separate product
//! (`antigravity.google`). State lives at:
//! ```text
//! ~/.gemini/antigravity/
//! ├── conversations/<uuid>.pb     ← Protobuf — one per conversation
//! ├── code_tracker/{active,history}/
//! ├── brain/, implicit/, context_state/
//! ├── installation_id, mcp_config.json, user_settings.pb
//! └── browserAllowlist.txt
//! ```
//!
//! Sessions = count of `*.pb` files under `conversations/`.
//! last_used = max mtime of those files.

use std::path::Path;

use super::fs_util::{dir_nonempty, max_mtime_in};
use super::{AgentDetector, AgentSnapshot, InstallStatus};

const ID: &str = "antigravity";
const DISPLAY_NAME: &str = "Antigravity";

/// Antigravity Detector.
pub struct AntigravityDetector;

impl AgentDetector for AntigravityDetector {
    fn id(&self) -> &'static str {
        ID
    }
    fn display_name(&self) -> &'static str {
        DISPLAY_NAME
    }

    fn detect(&self, home: &Path) -> AgentSnapshot {
        let dir = home.join(".gemini/antigravity");
        let conversations = dir.join("conversations");
        let paths_checked = vec![dir.clone(), conversations.clone()];

        let installed = dir.is_dir() && dir_nonempty(&dir);

        if !installed {
            return AgentSnapshot {
                id: ID,
                display_name: DISPLAY_NAME,
                status: InstallStatus::No,
                sessions: None,
                last_used: None,
                score: 0.0,
                paths_checked,
            };
        }

        let count = count_pb_files(&conversations);
        let last_used = max_mtime_in(&conversations, |p| p.extension().is_some_and(|e| e == "pb"));

        AgentSnapshot {
            id: ID,
            display_name: DISPLAY_NAME,
            status: InstallStatus::Yes,
            sessions: (count > 0).then_some(count),
            last_used,
            score: 0.0,
            paths_checked,
        }
    }
}

fn count_pb_files(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|e| e == "pb"))
        .count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn counts_pb_conversations() {
        let home = tempdir().unwrap();
        let conv = home.path().join(".gemini/antigravity/conversations");
        fs::create_dir_all(&conv).unwrap();
        fs::write(conv.join("aaaa.pb"), b"\x00").unwrap();
        fs::write(conv.join("bbbb.pb"), b"\x00").unwrap();
        // non-.pb file should be ignored
        fs::write(conv.join("readme.txt"), b"x").unwrap();

        let snap = AntigravityDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert_eq!(snap.sessions, Some(2));
    }

    #[test]
    fn empty_antigravity_dir_means_not_installed() {
        let home = tempdir().unwrap();
        // Directory doesn't exist at all.
        let snap = AntigravityDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::No);
    }

    #[test]
    fn antigravity_empty_present_dir_means_not_installed() {
        let home = tempdir().unwrap();
        fs::create_dir_all(home.path().join(".gemini/antigravity")).unwrap();
        let snap = AntigravityDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::No);
    }

    #[test]
    fn antigravity_dir_with_only_marker_file_still_installed_no_sessions() {
        let home = tempdir().unwrap();
        let dir = home.path().join(".gemini/antigravity");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("installation_id"), b"abc").unwrap();
        let snap = AntigravityDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert!(snap.sessions.is_none());
    }
}
