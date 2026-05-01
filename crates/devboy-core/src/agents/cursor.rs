//! Cursor IDE detector.
//!
//! Cross-platform paths (resolved via `dirs::data_dir()` or platform fallbacks):
//! - macOS:   `~/Library/Application Support/Cursor/User/workspaceStorage/`
//! - Linux:   `~/.config/Cursor/User/workspaceStorage/`
//! - Windows: `%APPDATA%/Cursor/User/workspaceStorage/`
//!
//! Sessions = count of subdirectories (one per workspace).
//! last_used = max mtime of `state.vscdb` files.
//!
//! We don't crack open the SQLite — for `onboard` parity, dir count + mtime
//! is sufficient. Reverse-engineering Cursor's chat keys belongs in
//! `analyze-usage`, not here.

use std::path::{Path, PathBuf};

use super::fs_util::{count_subdirs, to_utc};
use super::{AgentDetector, AgentSnapshot, InstallStatus};

const ID: &str = "cursor";
const DISPLAY_NAME: &str = "Cursor";

pub struct CursorDetector;

impl AgentDetector for CursorDetector {
    fn id(&self) -> &'static str { ID }
    fn display_name(&self) -> &'static str { DISPLAY_NAME }

    fn detect(&self, home: &Path) -> AgentSnapshot {
        let cursor_dir = cursor_data_dir(home);
        let workspace_storage = cursor_dir.join("User/workspaceStorage");
        let paths_checked = vec![cursor_dir.clone(), workspace_storage.clone()];

        let dir_present = cursor_dir.is_dir();
        // Cursor doesn't typically expose a CLI bin, but check anyway.
        let binary_present = which::which("cursor").is_ok();

        let status = if dir_present || binary_present { InstallStatus::Yes } else { InstallStatus::No };
        if status == InstallStatus::No {
            return empty(paths_checked);
        }

        let count = count_subdirs(&workspace_storage);
        let last_used = max_state_vscdb_mtime(&workspace_storage);

        AgentSnapshot {
            id: ID,
            display_name: DISPLAY_NAME,
            status,
            sessions: (count > 0).then_some(count),
            last_used,
            score: 0.0,
            paths_checked,
        }
    }
}

fn cursor_data_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Cursor")
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Cursor")
    } else {
        // Linux / BSD / fallback
        home.join(".config/Cursor")
    }
}

fn max_state_vscdb_mtime(workspace_storage: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    let Ok(workspaces) = std::fs::read_dir(workspace_storage) else { return None; };
    let mut best: Option<chrono::DateTime<chrono::Utc>> = None;
    for ws in workspaces.flatten() {
        let p = ws.path().join("state.vscdb");
        if let Ok(meta) = std::fs::metadata(&p)
            && let Ok(modified) = meta.modified()
            && let Some(t) = to_utc(modified)
        {
            best = Some(best.map_or(t, |b| b.max(t)));
        }
    }
    best
}

fn empty(paths_checked: Vec<PathBuf>) -> AgentSnapshot {
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
    #[cfg(target_os = "macos")]
    fn counts_workspace_subdirs_macos() {
        let home = tempdir().unwrap();
        let storage = home.path().join("Library/Application Support/Cursor/User/workspaceStorage");
        for ws in &["aaaa", "bbbb", "cccc"] {
            let dir = storage.join(ws);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("state.vscdb"), b"").unwrap();
        }
        let snap = CursorDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert_eq!(snap.sessions, Some(3));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn counts_workspace_subdirs_linux() {
        let home = tempdir().unwrap();
        let storage = home.path().join(".config/Cursor/User/workspaceStorage");
        for ws in &["aaaa", "bbbb"] {
            let dir = storage.join(ws);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("state.vscdb"), b"").unwrap();
        }
        let snap = CursorDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert_eq!(snap.sessions, Some(2));
    }
}
