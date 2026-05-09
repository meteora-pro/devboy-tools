//! Kimi Code CLI detector.
//!
//! Storage layout (from `MoonshotAI/kimi-cli/src/kimi_cli/{share,metadata,session}.py`):
//! ```text
//! $KIMI_SHARE_DIR | ~/.kimi/
//! └── sessions/<md5(work_dir_path)>/<session_uuid>/
//!     ├── context.jsonl      ← message history
//!     ├── state.json         ← SessionState
//!     └── subagents/<id>/
//! ```
//!
//! Sessions = count of `<uuid>/context.jsonl` across all workdir buckets.
//! last_used = max mtime of those files.

use std::path::Path;

use super::fs_util::to_utc;
use super::{AgentDetector, AgentSnapshot, InstallStatus};

const ID: &str = "kimi";
const DISPLAY_NAME: &str = "Kimi Code CLI";
const MAX_SESSIONS: usize = 50_000;

/// Kimi Detector.
pub struct KimiDetector;

impl AgentDetector for KimiDetector {
    fn id(&self) -> &'static str {
        ID
    }
    fn display_name(&self) -> &'static str {
        DISPLAY_NAME
    }

    fn detect(&self, home: &Path) -> AgentSnapshot {
        let share_dir = std::env::var_os("KIMI_SHARE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home.join(".kimi"));
        let sessions_dir = share_dir.join("sessions");
        let paths_checked = vec![share_dir.clone(), sessions_dir.clone()];

        let dir_present = share_dir.is_dir();
        let binary_present = which::which("kimi").is_ok();

        let status = if dir_present || binary_present {
            InstallStatus::Yes
        } else {
            InstallStatus::No
        };
        if status == InstallStatus::No {
            return empty(paths_checked);
        }

        let (count, last_used) = walk_sessions(&sessions_dir);

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

fn walk_sessions(sessions_dir: &Path) -> (u64, Option<chrono::DateTime<chrono::Utc>>) {
    let Ok(workdir_buckets) = std::fs::read_dir(sessions_dir) else {
        return (0, None);
    };
    let mut count = 0u64;
    let mut best: Option<chrono::DateTime<chrono::Utc>> = None;
    for bucket in workdir_buckets.flatten() {
        let bucket_path = bucket.path();
        if !bucket_path.is_dir() {
            continue;
        }
        let Ok(session_uuids) = std::fs::read_dir(&bucket_path) else {
            continue;
        };
        for session in session_uuids.flatten() {
            if count as usize >= MAX_SESSIONS {
                return (count, best);
            }
            let session_path = session.path();
            if !session_path.is_dir() {
                continue;
            }
            let context = session_path.join("context.jsonl");
            if !context.exists() {
                continue;
            }
            count += 1;
            if let Ok(meta) = context.metadata()
                && let Ok(t) = meta.modified()
                && let Some(t) = to_utc(t)
            {
                best = Some(best.map_or(t, |b| b.max(t)));
            }
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
    fn counts_context_jsonl_under_md5_buckets() {
        let home = tempdir().unwrap();
        let kimi = home.path().join(".kimi/sessions");
        let bucket1 = kimi.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let bucket2 = kimi.join("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        fs::create_dir_all(bucket1.join("uuid-1")).unwrap();
        fs::write(bucket1.join("uuid-1/context.jsonl"), b"{}\n").unwrap();
        fs::create_dir_all(bucket1.join("uuid-2")).unwrap();
        fs::write(bucket1.join("uuid-2/context.jsonl"), b"{}\n").unwrap();
        // Empty session — no context.jsonl, should not count
        fs::create_dir_all(bucket1.join("uuid-empty")).unwrap();
        fs::create_dir_all(bucket2.join("uuid-3")).unwrap();
        fs::write(bucket2.join("uuid-3/context.jsonl"), b"{}\n").unwrap();

        let snap = KimiDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert_eq!(snap.sessions, Some(3));
    }

    #[test]
    fn no_kimi_dir_means_not_installed() {
        let home = tempdir().unwrap();
        let snap = KimiDetector.detect(home.path());
        if which::which("kimi").is_err() {
            assert_eq!(snap.status, InstallStatus::No);
            assert!(snap.sessions.is_none());
        }
    }

    #[test]
    fn kimi_dir_without_sessions_still_reports_install() {
        let home = tempdir().unwrap();
        fs::create_dir_all(home.path().join(".kimi")).unwrap();
        let snap = KimiDetector.detect(home.path());
        assert_eq!(snap.status, InstallStatus::Yes);
        assert!(snap.sessions.is_none());
    }
}
