//! Claude Code detector.
//!
//! On-disk traces:
//! - Install marker: `~/.claude/` exists OR `claude` in PATH.
//! - Sessions: `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl` — one file per session.
//! - last_used: max mtime across all session files.

use std::path::Path;

use super::fs_util::{max_mtime_in, walk_files};
use super::{AgentDetector, AgentSnapshot, InstallStatus};

const ID: &str = "claude";
const DISPLAY_NAME: &str = "Claude Code";
const MAX_FILES: usize = 20_000;

pub struct ClaudeDetector;

impl AgentDetector for ClaudeDetector {
    fn id(&self) -> &'static str {
        ID
    }
    fn display_name(&self) -> &'static str {
        DISPLAY_NAME
    }

    fn detect(&self, home: &Path) -> AgentSnapshot {
        let claude_dir = home.join(".claude");
        let projects_dir = claude_dir.join("projects");
        let paths_checked = vec![claude_dir.clone(), projects_dir.clone()];

        let binary_present = which::which("claude").is_ok();
        let dir_present = claude_dir.is_dir();

        let status = match (dir_present, binary_present) {
            (true, _) => InstallStatus::Yes,
            (false, true) => InstallStatus::Yes,
            (false, false) => InstallStatus::No,
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

        let session_files = walk_files(
            &projects_dir,
            |p| p.extension().is_some_and(|e| e == "jsonl"),
            MAX_FILES,
        );
        let sessions = (!session_files.is_empty()).then_some(session_files.len() as u64);
        let last_used = walk_max_mtime(&projects_dir);

        AgentSnapshot {
            id: ID,
            display_name: DISPLAY_NAME,
            status,
            sessions,
            last_used,
            score: 0.0,
            paths_checked,
        }
    }
}

/// Recursively walk the projects/ tree taking the max mtime of any .jsonl.
fn walk_max_mtime(projects_dir: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    if !projects_dir.is_dir() {
        return None;
    }
    let entries = std::fs::read_dir(projects_dir).ok()?;
    let mut best = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let inner = max_mtime_in(&path, |p| p.extension().is_some_and(|e| e == "jsonl"));
            if let Some(t) = inner {
                best = Some(best.map_or(t, |b: chrono::DateTime<chrono::Utc>| b.max(t)));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn no_claude_dir_means_not_installed() {
        let home = tempdir().unwrap();
        let snap = ClaudeDetector.detect(home.path());
        // Note: if `claude` is in PATH on the dev machine running tests,
        // status flips to Yes via binary lookup. Guard accordingly.
        if which::which("claude").is_err() {
            assert_eq!(snap.status, InstallStatus::No);
            assert!(snap.sessions.is_none());
        }
    }

    #[test]
    fn counts_session_files_under_projects() {
        let home = tempdir().unwrap();
        let projects = home.path().join(".claude/projects/-Users-x-foo");
        fs::create_dir_all(&projects).unwrap();
        for i in 0..3 {
            fs::write(projects.join(format!("session-{i}.jsonl")), b"{}\n").unwrap();
        }
        // Subdir for another project
        let other = home.path().join(".claude/projects/-Users-x-bar");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("only.jsonl"), b"{}\n").unwrap();

        let snap = ClaudeDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert_eq!(snap.sessions, Some(4));
        assert!(snap.last_used.is_some());
    }

    #[test]
    fn claude_dir_without_projects_subdir_still_reports_install() {
        let home = tempdir().unwrap();
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        let snap = ClaudeDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert!(snap.sessions.is_none());
        assert!(snap.last_used.is_none());
    }

    #[test]
    fn empty_projects_dir_yields_no_sessions() {
        let home = tempdir().unwrap();
        fs::create_dir_all(home.path().join(".claude/projects")).unwrap();
        let snap = ClaudeDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert!(snap.sessions.is_none());
    }

    #[test]
    fn ignores_non_jsonl_files_inside_project_dir() {
        let home = tempdir().unwrap();
        let proj = home.path().join(".claude/projects/-x-y");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("session.jsonl"), b"{}\n").unwrap();
        fs::write(proj.join("README.md"), b"x").unwrap();
        fs::write(proj.join("data.json"), b"{}").unwrap();

        let snap = ClaudeDetector.detect(home.path());
        assert_eq!(snap.sessions, Some(1));
    }
}
